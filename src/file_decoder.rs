pub mod error;

extern crate ffmpeg_next;
use blocking_delay_queue::{BlockingDelayQueue, DelayItem};
pub use error_stack::{IntoReport, Report, Result, ResultExt};
use ffmpeg_next::{
    format::{input, Pixel},
    mathematics::Rounding,
    media::Type,
    rescale::TIME_BASE,
    software::scaling::{context::Context, flag::Flags},
    util::frame::video::Video,
    Packet, {Rational, Rescale},
};
use log::{debug, error, trace, warn};
use std::{
    ops::RangeFull,
    path::Path,
    sync::{mpsc, mpsc::channel, Arc, Weak},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub use error::FileDecoderError;

type PacketQueue = Arc<BlockingDelayQueue<DelayItem<Option<PacketData>>>>;
pub type VideoQueue = Arc<BlockingDelayQueue<DelayItem<Option<VideoData>>>>;

pub struct FileDecoder {
    uri: String,
    width: u32,
    height: u32,
    paused: bool,
    packet_queue: PacketQueue,
    video_queue: VideoQueue,
    running: Option<Arc<bool>>,
    seek_serial: u64,
    threads: Vec<JoinHandle<Result<(), FileDecoderError>>>,
    // Sender for demuxer:
    demuxer_seek_sender: Option<mpsc::Sender<i64>>,
    demuxer_serial_sender: Option<mpsc::Sender<u64>>,
    demuxer_pause_sender: Option<mpsc::Sender<bool>>,
    // Sender for decoder:
    decoder_serial_sender: Option<mpsc::Sender<u64>>,
}

struct DemuxerData {
    stream: ffmpeg_next::format::context::Input,
    stream_index: usize,
    time_base: Rational,
    seek_serial: u64,
    paused: bool,
    packet_queue: PacketQueue,
    running: Weak<bool>,
    seek_receiver: mpsc::Receiver<i64>,
    serial_receiver: mpsc::Receiver<u64>,
    pause_receiver: mpsc::Receiver<bool>,
}

struct DecoderData {
    decoder: ffmpeg_next::decoder::Video,
    time_base: Rational,
    packet_queue: PacketQueue,
    video_queue: VideoQueue,
    running: Weak<bool>,
    seek_serial: u64,
    serial_receiver: mpsc::Receiver<u64>,
}

struct PacketData {
    serial: u64,
    packet: Packet,
}

pub struct VideoData {
    pub serial: u64,
    pub frame_time: u64,
    pub diff_to_prev_frame: u64,
    pub video_frame: Video,
}

impl DemuxerData {
    fn new(
        stream: ffmpeg_next::format::context::Input,
        stream_index: usize,
        time_base: Rational,
        packet_queue: PacketQueue,
        running: Weak<bool>,
        seek_receiver: mpsc::Receiver<i64>,
        serial_receiver: mpsc::Receiver<u64>,
        pause_receiver: mpsc::Receiver<bool>,
    ) -> Self {
        Self {
            stream,
            stream_index,
            time_base,
            seek_serial: 0,
            paused: false,
            packet_queue,
            running,
            seek_receiver,
            serial_receiver,
            pause_receiver,
        }
    }
}

impl DecoderData {
    fn new(
        decoder: ffmpeg_next::decoder::Video,
        time_base: Rational,
        packet_queue: PacketQueue,
        video_queue: VideoQueue,
        running: Weak<bool>,
        serial_receiver: mpsc::Receiver<u64>,
    ) -> Self {
        Self {
            decoder,
            time_base,
            packet_queue,
            video_queue,
            running,
            seek_serial: 0,
            serial_receiver,
        }
    }
}

impl PacketData {
    fn new(serial: u64, packet: Packet) -> Self {
        Self { serial, packet }
    }
}

impl VideoData {
    fn new(serial: u64, frame_time: u64, diff_to_prev_frame: u64, video_frame: Video) -> Self {
        Self {
            serial,
            frame_time,
            diff_to_prev_frame,
            video_frame,
        }
    }
}

impl FileDecoder {
    const PACKET_QUEUE_SIZE: usize = 60;
    const FRAME_QUEUE_SIZE: usize = 3;

    pub fn new() -> Self {
        Self {
            uri: "".to_owned(),
            width: 0,
            height: 0,
            paused: false,
            packet_queue: Arc::new(BlockingDelayQueue::new_with_capacity(
                FileDecoder::PACKET_QUEUE_SIZE,
            )),
            video_queue: Arc::new(BlockingDelayQueue::new_with_capacity(
                FileDecoder::FRAME_QUEUE_SIZE,
            )),
            running: None,
            seek_serial: 0,
            threads: Vec::new(),
            demuxer_seek_sender: None,
            demuxer_serial_sender: None,
            demuxer_pause_sender: None,
            decoder_serial_sender: None,
        }
    }

    pub fn start(&mut self, uri: &String) -> Result<(), FileDecoderError> {
        ffmpeg_next::init().map_err(FileDecoderError::FfmpegError)?;
        self.uri = uri.to_owned();
        //let path = Path::new(&self.uri);
        let input = input(&Path::new(&self.uri)).map_err(FileDecoderError::FfmpegError)?;

        let video_stream_input = input
            .streams()
            .best(Type::Video)
            .ok_or(ffmpeg_next::Error::StreamNotFound)
            .map_err(FileDecoderError::FfmpegError)?;
        let video_stream_index = video_stream_input.index();
        let video_stream_tb = video_stream_input.time_base();

        let context_decoder =
            ffmpeg_next::codec::context::Context::from_parameters(video_stream_input.parameters())
                .map_err(FileDecoderError::FfmpegError)?;
        let decoder = context_decoder
            .decoder()
            .video()
            .map_err(FileDecoderError::FfmpegError)?;

        let running = Arc::new(true);

        let (demuxer_seek_sender, demuxer_seek_receiver): (mpsc::Sender<i64>, mpsc::Receiver<i64>) =
            channel();
        let (demuxer_serial_sender, demuxer_serial_receiver): (
            mpsc::Sender<u64>,
            mpsc::Receiver<u64>,
        ) = channel();
        let (demuxer_pause_sender, demuxer_pause_receiver): (
            mpsc::Sender<bool>,
            mpsc::Receiver<bool>,
        ) = channel();
        let (decoder_serial_sender, decoder_serial_receiver): (
            mpsc::Sender<u64>,
            mpsc::Receiver<u64>,
        ) = channel();

        self.demuxer_seek_sender = Some(demuxer_seek_sender);
        self.demuxer_serial_sender = Some(demuxer_serial_sender);
        self.demuxer_pause_sender = Some(demuxer_pause_sender);
        self.decoder_serial_sender = Some(decoder_serial_sender);

        let packet_queue = self.packet_queue.clone();
        let demuxer_data = DemuxerData::new(
            input,
            video_stream_index,
            video_stream_tb,
            packet_queue,
            Arc::downgrade(&running),
            demuxer_seek_receiver,
            demuxer_serial_receiver,
            demuxer_pause_receiver,
        );

        self.width = decoder.width();
        self.height = decoder.height();

        let video_producer_queue = self.video_queue.clone();
        let decoder_data = DecoderData::new(
            decoder,
            video_stream_tb,
            demuxer_data.packet_queue.clone(),
            video_producer_queue,
            Arc::downgrade(&running),
            decoder_serial_receiver,
        );

        self.running.replace(running);

        self.threads.push(thread::spawn({
            let mut demuxer_data = demuxer_data;
            move || -> Result<(), FileDecoderError> {
                'demuxing: loop {
                    let rec = demuxer_data.pause_receiver.try_recv();
                    if rec.is_ok() {
                        if rec.ok().unwrap() {
                            demuxer_data.paused = true;
                            // demuxer_data
                            //     .stream
                            //     .pause()
                            //     .map_err(FileDecoderError::FfmpegError)?;
                        } else {
                            demuxer_data.paused = false;
                            // demuxer_data
                            //     .stream
                            //     .play()
                            //     .map_err(FileDecoderError::FfmpegError)?;
                        }
                    }
                    let rec = demuxer_data.seek_receiver.try_recv();
                    if rec.is_ok() {
                        let seek_to = rec.ok().unwrap();

                        let rec = demuxer_data.serial_receiver.try_recv();
                        if rec.is_ok() {
                            demuxer_data.seek_serial = rec.ok().unwrap();
                        }

                        let seek_to =
                            seek_to.rescale_with(demuxer_data.time_base, TIME_BASE, Rounding::Zero);

                        debug!("seek to {}", seek_to);
                        demuxer_data
                            .stream
                            .seek(0, RangeFull)
                            .map_err(FileDecoderError::FfmpegError)?;
                        demuxer_data
                            .stream
                            .seek(seek_to, RangeFull)
                            .map_err(FileDecoderError::FfmpegError)?;
                        demuxer_data.packet_queue.clear();
                    }
                    if !demuxer_data.paused {
                        if let Some((stream, packet)) = demuxer_data.stream.packets().next() {
                            if stream.index() == demuxer_data.stream_index {
                                trace!(
                                    "Demuxer: queue packet with pts {}",
                                    packet.pts().unwrap_or_default()
                                );
                                let packet_data = PacketData::new(demuxer_data.seek_serial, packet);
                                demuxer_data
                                    .packet_queue
                                    .add(DelayItem::new(Some(packet_data), Instant::now()));
                            }
                        } else {
                            debug!("no more packages, quit demuxer");
                            demuxer_data
                                .packet_queue
                                .add(DelayItem::new(None, Instant::now()));
                            break 'demuxing;
                        }
                    } else {
                        thread::sleep(Duration::from_millis(100));
                    }

                    if demuxer_data.running.upgrade().is_none() {
                        break 'demuxing;
                    }
                }

                debug!("################### return from demuxer spawn");
                Ok(())
            }
        }));

        self.threads.push(thread::spawn({
            let mut decoder_data = decoder_data;
            move || -> Result<(), FileDecoderError> {
                let mut scaler = Context::get(
                    decoder_data.decoder.format(),
                    decoder_data.decoder.width(),
                    decoder_data.decoder.height(),
                    Pixel::RGB24,
                    decoder_data.decoder.width(),
                    decoder_data.decoder.height(),
                    Flags::BILINEAR,
                )
                .map_err(FileDecoderError::FfmpegError)?;

                let mut sent_eof = false;
                let mut last_frame_time: Option<u64> = None;

                let mut receive_and_process_decoded_frame =
                    |current_serial: &u64,
                     decoder: &mut ffmpeg_next::decoder::Video,
                     last_frame_time: &mut Option<u64>,
                     video_producer_queue: &VideoQueue|
                     -> Result<bool, FileDecoderError> {
                        let mut decoded = Video::empty();
                        let status = decoder.receive_frame(&mut decoded);
                        match status {
                            Err(err) => match err {
                                ffmpeg_next::Error::Eof => {
                                    debug!("Decoder returned EOF, send EOF frame");
                                    decoder_data
                                        .video_queue
                                        .add(DelayItem::new(None, Instant::now()));
                                    Ok(true)
                                }
                                ffmpeg_next::Error::Other { errno } => match errno {
                                    ffmpeg_next::util::error::EAGAIN => Ok(false),
                                    _ => Err(Report::new(FileDecoderError::FfmpegError(
                                        ffmpeg_next::Error::Other { errno },
                                    ))),
                                },
                                _ => Err(Report::new(FileDecoderError::FfmpegError(err))),
                            },
                            Ok(()) => {
                                trace!(
                                    "decoder: received frame with pts {}",
                                    decoded.timestamp().unwrap_or_default()
                                );
                                let mut rgb_frame = Video::empty();
                                scaler
                                    .run(&decoded, &mut rgb_frame)
                                    .map_err(FileDecoderError::FfmpegError)?;
                                rgb_frame.set_pts(decoded.timestamp());

                                let deocded_timestamp = decoded.timestamp().unwrap_or(0);
                                let frame_time = deocded_timestamp.rescale_with(
                                    decoder_data.time_base,
                                    Rational(1, 1000),
                                    Rounding::Zero,
                                ) as u64;

                                let mut frame_diff: u64 = 0;
                                if let Some(prev_time) = *last_frame_time {
                                    frame_diff = frame_time - prev_time;
                                }

                                *last_frame_time = Some(frame_time);

                                trace!(
                                    "decoder: add frame with pts {} to video queue",
                                    deocded_timestamp
                                );
                                video_producer_queue.add(DelayItem::new(
                                    Some(VideoData::new(
                                        *current_serial,
                                        frame_time,
                                        frame_diff,
                                        rgb_frame,
                                    )),
                                    Instant::now(),
                                ));
                                trace!(
                                    "got back from adding to video queue running={}",
                                    decoder_data.running.upgrade().is_none()
                                );
                                Ok(decoder_data.running.upgrade().is_none())
                            }
                        }
                    };

                'decoding: loop {
                    let rec = decoder_data.serial_receiver.try_recv();
                    if rec.is_ok() {
                        decoder_data.seek_serial = rec.ok().unwrap();
                        debug!("decoder: received serial {}", decoder_data.seek_serial);
                        sent_eof = false;
                        decoder_data.decoder.flush();
                        decoder_data.video_queue.clear();
                        last_frame_time = None;
                    }
                    if !sent_eof {
                        let packet_delay_item = decoder_data.packet_queue.take();
                        let packet_data = packet_delay_item.data;

                        if let Some(packet_data) = packet_data {
                            trace!("decoder: got packet");
                            if decoder_data.seek_serial != packet_data.serial {
                                debug!("decoder: serial wrong continue");
                                continue 'decoding;
                            }
                            trace!(
                                "decoder: send packet with pts {}",
                                packet_data.packet.pts().unwrap_or_default()
                            );
                            decoder_data
                                .decoder
                                .send_packet(&packet_data.packet)
                                .unwrap();
                        } else {
                            debug!("Send EOF to decoder");
                            sent_eof = true;
                            decoder_data.decoder.send_eof().unwrap();
                        }
                    }

                    let is_eof = receive_and_process_decoded_frame(
                        &decoder_data.seek_serial,
                        &mut decoder_data.decoder,
                        &mut last_frame_time,
                        &decoder_data.video_queue,
                    )
                    .unwrap();
                    trace!("received frame is_eof={}", is_eof);
                    if is_eof {
                        break 'decoding;
                    }
                }
                debug!("################### return from decoder spawn");
                Ok(())
            }
        }));

        Ok(())
    }

    pub fn stop(&mut self) {
        debug!("FileDecoder::stop()");
        self.running.take();
        self.packet_queue.clear();
        self.video_queue.clear();
        while let Some(t) = self.threads.pop() {
            match t.join() {
                Ok(res) => match res {
                    Ok(_) => {}
                    Err(err) => {
                        warn!("FileDecoder: thread exited with error {:?}", err);
                    }
                },
                Err(err) => {
                    error!("FileDecoder: thread exited with error {:?}", err);
                }
            };
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn seek(&mut self, seek_to: i64) -> u64 {
        self.demuxer_seek_sender
            .as_ref()
            .unwrap()
            .send(seek_to)
            .unwrap();
        self.seek_serial += 1;
        self.demuxer_serial_sender
            .as_ref()
            .unwrap()
            .send(self.seek_serial)
            .unwrap();
        self.decoder_serial_sender
            .as_ref()
            .unwrap()
            .send(self.seek_serial)
            .unwrap();
        self.seek_serial
    }

    pub fn set_paused(&mut self, paused: bool) -> Result<(), FileDecoderError> {
        self.paused = paused;
        self.demuxer_pause_sender
            .as_ref()
            .unwrap()
            .send(self.paused)
            .map_err(FileDecoderError::SendError)?;
        Ok(())
    }

    pub fn video_queue(&self) -> VideoQueue {
        self.video_queue.clone()
    }
}

impl Drop for FileDecoder {
    fn drop(&mut self) {
        self.stop();
    }
}
