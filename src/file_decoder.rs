extern crate ffmpeg_rs;
use blocking_delay_queue::{BlockingDelayQueue, DelayItem};
pub use error_stack::{Context, IntoReport, Report, Result, ResultExt};
use ffmpeg_rs::{
    format::{input, Pixel},
    mathematics::Rounding,
    media::Type,
    rescale::TIME_BASE,
    software::scaling::{context, flag::Flags},
    util::frame::video::Video,
    Packet, {Rational, Rescale},
};
use log::{debug, error, trace, warn};
use std::fmt;
use std::{
    mem::swap,
    ops::RangeFull,
    path::Path,
    sync::{mpsc, mpsc::channel, Arc, Weak},
    thread::{self, JoinHandle},
    time::Instant,
};

#[derive(Debug)]
pub struct FileDecoderError;

impl fmt::Display for FileDecoderError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.write_str("File decoder error")
    }
}

impl Context for FileDecoderError {}

type PacketQueue = Arc<BlockingDelayQueue<DelayItem<Option<PacketData>>>>;
pub type VideoQueue = Arc<BlockingDelayQueue<DelayItem<Option<VideoData>>>>;

#[derive(new)]
#[allow(clippy::too_many_arguments)]
pub struct FileDecoderBuilder {
    uri: String,
    #[new(value = "Pixel::YUV420P")]
    pixel_format: Pixel,
}

impl FileDecoderBuilder {
    pub fn build(&self) -> Result<FileDecoder, FileDecoderError> {
        let mut file_decoder = FileDecoder::new(self.uri.to_owned(), self.pixel_format);
        file_decoder.init()?;
        Ok(file_decoder)
    }

    pub fn pixel_format(&mut self, pix_fmt: Pixel) -> &mut FileDecoderBuilder {
        self.pixel_format = pix_fmt;
        self
    }

    #[allow(dead_code)]
    pub fn uri(&mut self, uri: String) -> &mut FileDecoderBuilder {
        self.uri = uri;
        self
    }
}

#[derive(new)]
#[allow(clippy::too_many_arguments)]
pub struct FileDecoder {
    uri: String,
    pixel_format: Pixel,
    #[new(default)]
    width: u32,
    #[new(default)]
    height: u32,
    #[new(
        value = "Arc::new(BlockingDelayQueue::new_with_capacity(FileDecoder::PACKET_QUEUE_SIZE))"
    )]
    packet_queue: PacketQueue,
    #[new(
        value = "Arc::new(BlockingDelayQueue::new_with_capacity(FileDecoder::FRAME_QUEUE_SIZE))"
    )]
    video_queue: VideoQueue,
    #[new(default)]
    running: Option<Arc<bool>>,
    #[new(default)]
    seek_serial: u64,
    #[new(default)]
    threads: Vec<JoinHandle<Result<(), FileDecoderError>>>,
    // Sender for demuxer:
    #[new(default)]
    demuxer_seek_sender: Option<mpsc::Sender<i64>>,
    #[new(default)]
    demuxer_serial_sender: Option<mpsc::Sender<u64>>,
    // Sender for decoder:
    #[new(default)]
    decoder_serial_sender: Option<mpsc::Sender<u64>>,
    #[new(value = "None")]
    demuxer_data: Option<DemuxerData>,
    #[new(value = "None")]
    decoder_data: Option<DecoderData>,
}

#[derive(new)]
#[allow(clippy::too_many_arguments)]
struct DemuxerData {
    stream: ffmpeg_rs::format::context::Input,
    stream_index: usize,
    time_base: Rational,
    #[new(value = "0")]
    seek_serial: u64,
    packet_queue: PacketQueue,
    running: Weak<bool>,
    seek_receiver: mpsc::Receiver<i64>,
    serial_receiver: mpsc::Receiver<u64>,
}

#[derive(new)]
struct DecoderData {
    pixel_format: Pixel,
    decoder: ffmpeg_rs::decoder::Video,
    time_base: Rational,
    packet_queue: PacketQueue,
    video_queue: VideoQueue,
    running: Weak<bool>,
    #[new(value = "0")]
    seek_serial: u64,
    serial_receiver: mpsc::Receiver<u64>,
}

#[derive(new)]
struct PacketData {
    serial: u64,
    packet: Packet,
}

#[derive(new)]
pub struct VideoData {
    pub serial: u64,
    pub frame_time: u64,
    pub diff_to_prev_frame: u64,
    pub video_frame: Video,
}

impl FileDecoder {
    const PACKET_QUEUE_SIZE: usize = 60;
    const FRAME_QUEUE_SIZE: usize = 3;

    pub fn init(&mut self) -> Result<(), FileDecoderError> {
        ffmpeg_rs::init()
            .into_report()
            .attach_printable("FFmpeg init failed")
            .change_context(FileDecoderError)?;
        let input = input(&Path::new(&self.uri))
            .into_report()
            .attach_printable("Cannot open file")
            .change_context(FileDecoderError)?;
        let video_stream_input = input
            .streams()
            .best(Type::Video)
            .ok_or(ffmpeg_rs::Error::StreamNotFound)
            .into_report()
            .attach_printable("Could not open video stream")
            .change_context(FileDecoderError)?;
        let video_stream_index = video_stream_input.index();
        let video_stream_tb = video_stream_input.time_base();

        let context_decoder =
            ffmpeg_rs::codec::context::Context::from_parameters(video_stream_input.parameters())
                .into_report()
                .attach_printable("Cannot create context from parameters")
                .change_context(FileDecoderError)?;

        let decoder = context_decoder
            .decoder()
            .video()
            .into_report()
            .attach_printable("Cannot create decoder")
            .change_context(FileDecoderError)?;

        let running = Arc::new(true);

        let (demuxer_seek_sender, demuxer_seek_receiver): (mpsc::Sender<i64>, mpsc::Receiver<i64>) =
            channel();
        let (demuxer_serial_sender, demuxer_serial_receiver): (
            mpsc::Sender<u64>,
            mpsc::Receiver<u64>,
        ) = channel();
        let (decoder_serial_sender, decoder_serial_receiver): (
            mpsc::Sender<u64>,
            mpsc::Receiver<u64>,
        ) = channel();

        self.demuxer_seek_sender = Some(demuxer_seek_sender);
        self.demuxer_serial_sender = Some(demuxer_serial_sender);
        self.decoder_serial_sender = Some(decoder_serial_sender);

        let packet_queue = self.packet_queue.clone();
        self.demuxer_data.replace(DemuxerData::new(
            input,
            video_stream_index,
            video_stream_tb,
            packet_queue.clone(),
            Arc::downgrade(&running),
            demuxer_seek_receiver,
            demuxer_serial_receiver,
        ));

        self.width = decoder.width();
        self.height = decoder.height();

        let video_producer_queue = self.video_queue.clone();
        self.decoder_data.replace(DecoderData::new(
            self.pixel_format,
            decoder,
            video_stream_tb,
            packet_queue,
            video_producer_queue,
            Arc::downgrade(&running),
            decoder_serial_receiver,
        ));

        self.running.replace(running);

        Ok(())
    }

    pub fn start(&mut self) -> Result<(), FileDecoderError> {
        let mut demuxer_data: Option<DemuxerData> = None;
        swap(&mut self.demuxer_data, &mut demuxer_data);

        self.threads.push(thread::spawn({
            let mut demuxer_data = demuxer_data.unwrap();
            move || -> Result<(), FileDecoderError> {
                // let mut demuxer_data = demuxer_data.unwrap();
                'demuxing: loop {
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
                        // demuxer_data
                        //     .stream
                        //     .seek(0, RangeFull)
                        //     .map_err(FileDecoderError::FfmpegError)?;
                        demuxer_data
                            .stream
                            .seek(seek_to, RangeFull)
                            .into_report()
                            .attach_printable(format!("Cannot seek to {}", seek_to))
                            .change_context(FileDecoderError)?;
                        demuxer_data.packet_queue.clear();
                    }

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

                    if demuxer_data.running.upgrade().is_none() {
                        trace!("quit demuxer, running is false");
                        break 'demuxing;
                    }
                }

                debug!("################### return from demuxer spawn");
                Ok(())
            }
        }));

        let mut decoder_data: Option<DecoderData> = None;
        swap(&mut self.decoder_data, &mut decoder_data);

        self.threads.push(thread::spawn({
            let mut decoder_data = decoder_data.unwrap();
            move || -> Result<(), FileDecoderError> {
                let mut scaler = context::Context::get(
                    decoder_data.decoder.format(),
                    decoder_data.decoder.width(),
                    decoder_data.decoder.height(),
                    decoder_data.pixel_format,
                    decoder_data.decoder.width(),
                    decoder_data.decoder.height(),
                    Flags::BILINEAR,
                )
                .into_report()
                .attach_printable("Cannot get scaling context")
                .change_context(FileDecoderError)?;

                let mut sent_eof = false;
                let mut last_frame_time: Option<u64> = None;

                let mut receive_and_process_decoded_frame =
                    |current_serial: &u64,
                     decoder: &mut ffmpeg_rs::decoder::Video,
                     last_frame_time: &mut Option<u64>,
                     video_producer_queue: &VideoQueue|
                     -> Result<bool, FileDecoderError> {
                        let mut decoded = Video::empty();
                        let status = decoder.receive_frame(&mut decoded);
                        match status {
                            Err(err) => match err {
                                ffmpeg_rs::Error::Eof => {
                                    debug!("Decoder returned EOF, send EOF frame");
                                    decoder_data
                                        .video_queue
                                        .add(DelayItem::new(None, Instant::now()));
                                    Ok(true)
                                }
                                ffmpeg_rs::Error::Other {
                                    errno: ffmpeg_rs::util::error::EAGAIN,
                                } => Ok(false),
                                _ => Err(Report::new(FileDecoderError)
                                    .attach_printable(format!("{err}"))),
                            },
                            Ok(()) => {
                                trace!(
                                    "decoder: received frame with pts {}",
                                    decoded.timestamp().unwrap_or_default()
                                );
                                let mut rgb_frame = Video::empty();
                                scaler
                                    .run(&decoded, &mut rgb_frame)
                                    .into_report()
                                    .attach_printable("Scaling failed")
                                    .change_context(FileDecoderError)?;
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
                                trace!("decoder: serial wrong continue");
                                continue 'decoding;
                            }
                            trace!(
                                "decoder: send packet with pts {}",
                                packet_data.packet.pts().unwrap_or_default()
                            );
                            decoder_data
                                .decoder
                                .send_packet(&packet_data.packet)
                                .into_report()
                                .change_context(FileDecoderError)?;
                        } else {
                            debug!("Send EOF to decoder");
                            sent_eof = true;
                            decoder_data
                                .decoder
                                .send_eof()
                                .into_report()
                                .change_context(FileDecoderError)?;
                        }
                    }

                    let is_eof = receive_and_process_decoded_frame(
                        &decoder_data.seek_serial,
                        &mut decoder_data.decoder,
                        &mut last_frame_time,
                        &decoder_data.video_queue,
                    )?;
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

    pub fn seek(&mut self, seek_to: i64) -> Result<u64, FileDecoderError> {
        self.seek_serial += 1;
        self.demuxer_serial_sender
            .as_ref()
            .unwrap()
            .send(self.seek_serial)
            .into_report()
            .change_context(FileDecoderError)?;
        self.decoder_serial_sender
            .as_ref()
            .unwrap()
            .send(self.seek_serial)
            .into_report()
            .change_context(FileDecoderError)?;
        self.demuxer_seek_sender
            .as_ref()
            .unwrap()
            .send(seek_to)
            .into_report()
            .change_context(FileDecoderError)?;
        Ok(self.seek_serial)
    }

    pub fn video_queue(&self) -> VideoQueue {
        self.video_queue.clone()
    }

    pub fn pixel_format(&self) -> Pixel {
        self.pixel_format
    }
}

impl Drop for FileDecoder {
    fn drop(&mut self) {
        self.stop();
    }
}
