pub mod error;

extern crate ffmpeg_next;
use blocking_delay_queue::{BlockingDelayQueue, DelayItem};
pub use error_stack::{IntoReport, Report, Result, ResultExt};
use ffmpeg_next::{
    format::{input, Pixel},
    mathematics::Rounding,
    media::Type,
    software::scaling::{context::Context, flag::Flags},
    util::frame::video::Video,
    Packet, {Rational, Rescale},
};
use std::{
    path::Path,
    sync::{Arc, Weak},
    thread::{self, JoinHandle},
    time::Instant,
};

pub use error::PlayerError;

type PacketQueue = Arc<BlockingDelayQueue<DelayItem<Option<Packet>>>>;
pub type VideoQueue = Arc<BlockingDelayQueue<DelayItem<Option<VideoData>>>>;

pub struct Player {
    uri: String,
    width: u32,
    height: u32,
    paused: bool,
    packet_queue: PacketQueue,
    video_queue: VideoQueue,
    running: Option<Arc<bool>>,
    threads: Vec<JoinHandle<Result<(), PlayerError>>>,
}

struct DemuxerData {
    stream: ffmpeg_next::format::context::Input,
    stream_index: usize,
    packet_queue: PacketQueue,
    running: Weak<bool>,
}

struct DecoderData {
    decoder: ffmpeg_next::decoder::Video,
    time_base: Rational,
    packet_queue: PacketQueue,
    video_queue: VideoQueue,
    running: Weak<bool>,
}

pub struct VideoData {
    pub frame_time: u64,
    pub diff_to_prev_frame: u64,
    pub video_frame: Video,
}

impl DemuxerData {
    fn new(
        stream: ffmpeg_next::format::context::Input,
        stream_index: usize,
        packet_queue: PacketQueue,
        running: Weak<bool>,
    ) -> Self {
        Self {
            stream,
            stream_index,
            packet_queue,
            running,
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
    ) -> Self {
        Self {
            decoder,
            time_base,
            packet_queue,
            video_queue,
            running,
        }
    }
}

impl VideoData {
    fn new(frame_time: u64, diff_to_prev_frame: u64, video_frame: Video) -> Self {
        Self {
            frame_time,
            diff_to_prev_frame,
            video_frame,
        }
    }
}

impl Player {
    const PACKET_QUEUE_SIZE: usize = 60;
    const FRAME_QUEUE_SIZE: usize = 3;

    pub fn new() -> Self {
        Self {
            uri: "".to_owned(),
            width: 0,
            height: 0,
            paused: false,
            packet_queue: Arc::new(BlockingDelayQueue::new_with_capacity(
                Player::PACKET_QUEUE_SIZE,
            )),
            video_queue: Arc::new(BlockingDelayQueue::new_with_capacity(
                Player::FRAME_QUEUE_SIZE,
            )),
            running: None,
            threads: Vec::new(),
        }
    }

    pub fn start(&mut self, uri: &String) -> Result<(), PlayerError> {
        ffmpeg_next::init().map_err(PlayerError::FfmpegError)?;
        self.uri = uri.to_owned();
        //let path = Path::new(&self.uri);
        let input = input(&Path::new(&self.uri)).map_err(PlayerError::FfmpegError)?;

        let video_stream_input = input
            .streams()
            .best(Type::Video)
            .ok_or(ffmpeg_next::Error::StreamNotFound)
            .map_err(PlayerError::FfmpegError)?;
        let video_stream_index = video_stream_input.index();
        let video_stream_tb = video_stream_input.time_base();

        let context_decoder =
            ffmpeg_next::codec::context::Context::from_parameters(video_stream_input.parameters())
                .map_err(PlayerError::FfmpegError)?;
        let decoder = context_decoder
            .decoder()
            .video()
            .map_err(PlayerError::FfmpegError)?;

        let running = Arc::new(true);

        let packet_queue = self.packet_queue.clone();
        let demuxer_data = DemuxerData::new(
            input,
            video_stream_index,
            packet_queue,
            Arc::downgrade(&running),
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
        );

        self.running.replace(running);

        self.threads.push(thread::spawn({
            let mut demuxer_data = demuxer_data;
            move || -> Result<(), PlayerError> {
                'demuxing: loop {
                    if let Some((stream, packet)) = demuxer_data.stream.packets().next() {
                        if stream.index() == demuxer_data.stream_index {
                            demuxer_data
                                .packet_queue
                                .add(DelayItem::new(Some(packet), Instant::now()));
                        }
                    } else {
                        demuxer_data
                            .packet_queue
                            .add(DelayItem::new(None, Instant::now()));
                        break 'demuxing;
                    }

                    if demuxer_data.running.upgrade().is_none() {
                        break 'demuxing;
                    }
                }

                println!("################### return from demuxer spawn");
                Ok(())
            }
        }));

        self.threads.push(thread::spawn({
            let mut decoder_data = decoder_data;
            move || -> Result<(), PlayerError> {
                let mut scaler = Context::get(
                    decoder_data.decoder.format(),
                    decoder_data.decoder.width(),
                    decoder_data.decoder.height(),
                    Pixel::RGB24,
                    decoder_data.decoder.width(),
                    decoder_data.decoder.height(),
                    Flags::BILINEAR,
                )
                .map_err(PlayerError::FfmpegError)?;

                let mut sent_eof = false;
                let mut last_frame_time: u64 = 0;

                let mut receive_and_process_decoded_frame =
                    |decoder: &mut ffmpeg_next::decoder::Video,
                     video_producer_queue: &VideoQueue|
                     -> Result<bool, PlayerError> {
                        let mut decoded = Video::empty();
                        let status = decoder.receive_frame(&mut decoded);
                        match status {
                            Err(err) => match err {
                                ffmpeg_next::Error::Eof => {
                                    println!("Decoder returned EOF, send EOF frame");
                                    decoder_data
                                        .video_queue
                                        .add(DelayItem::new(None, Instant::now()));
                                    Ok(true)
                                }
                                ffmpeg_next::Error::Other { errno } => match errno {
                                    ffmpeg_next::util::error::EAGAIN => Ok(false),
                                    _ => Err(Report::new(PlayerError::FfmpegError(
                                        ffmpeg_next::Error::Other { errno },
                                    ))),
                                },
                                _ => Err(Report::new(PlayerError::FfmpegError(err))),
                            },
                            Ok(()) => {
                                let mut rgb_frame = Video::empty();
                                scaler
                                    .run(&decoded, &mut rgb_frame)
                                    .map_err(PlayerError::FfmpegError)?;
                                rgb_frame.set_pts(decoded.timestamp());

                                let deocded_timestamp = decoded.timestamp().unwrap_or(0);
                                let frame_time = deocded_timestamp.rescale_with(
                                    decoder_data.time_base,
                                    Rational(1, 1000),
                                    Rounding::Zero,
                                ) as u64;

                                // println!(
                                //     "Queue frame with pts {} and timestamp {}",
                                //     deocded_timestamp, frame_time,
                                // );

                                let frame_diff = frame_time - last_frame_time;

                                last_frame_time = frame_time;

                                // println!("add to video queue");
                                video_producer_queue.add(DelayItem::new(
                                    Some(VideoData::new(frame_time, frame_diff, rgb_frame)),
                                    Instant::now(),
                                ));
                                // println!(
                                //     "got back from adding to video queue running={}",
                                //     decoder_data.running.upgrade().is_none()
                                // );
                                Ok(decoder_data.running.upgrade().is_none())
                            }
                        }
                    };

                'decoding: loop {
                    if !sent_eof {
                        let packet_delay_item = decoder_data.packet_queue.take();
                        let packet = packet_delay_item.data;

                        if let Some(packet) = packet {
                            decoder_data
                                .decoder
                                .send_packet(&packet)
                                .map_err(PlayerError::FfmpegError)?;
                        } else {
                            println!("Send EOF to decoder");
                            sent_eof = true;
                            decoder_data
                                .decoder
                                .send_eof()
                                .map_err(PlayerError::FfmpegError)?;
                        }
                    }

                    let is_eof = receive_and_process_decoded_frame(
                        &mut decoder_data.decoder,
                        &decoder_data.video_queue,
                    )?;
                    // println!("received frame is_eof={}", is_eof);
                    if is_eof {
                        break 'decoding;
                    }
                }
                println!("################### return from decoder spawn");
                Ok(())
            }
        }));

        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), PlayerError> {
        println!("Player::stop()");
        self.running.take();
        self.packet_queue.clear();
        self.video_queue.clear();
        while let Some(t) = self.threads.pop() {
            if let Err(err) = t.join() {
                println!("Player: thread exited with error {:?}", err);
            }
        }
        Ok(())
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if !self.paused {}
    }

    pub fn video_queue(&self) -> VideoQueue {
        self.video_queue.clone()
    }
}
