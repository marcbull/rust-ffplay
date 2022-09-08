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
    time::{Duration, Instant},
};

pub use error::PlayerError;

use self::error::to_player_error;

pub struct Player {
    uri: String,
    width: u32,
    height: u32,
    packet_queue: Arc<BlockingDelayQueue<DelayItem<Packet>>>,
    video_queue: Arc<BlockingDelayQueue<DelayItem<Video>>>,
    running: Option<Arc<bool>>,
    threads: Vec<JoinHandle<Result<(), PlayerError>>>,
}

struct DemuxerData {
    stream: ffmpeg_next::format::context::Input,
    stream_index: usize,
    packet_queue: Arc<BlockingDelayQueue<DelayItem<Packet>>>,
    running: Weak<bool>,
}

struct DecoderData {
    decoder: ffmpeg_next::decoder::Video,
    time_base: Rational,
    packet_queue: Arc<BlockingDelayQueue<DelayItem<Packet>>>,
    video_queue: Arc<BlockingDelayQueue<DelayItem<Video>>>,
    running: Weak<bool>,
}

impl DemuxerData {
    pub fn new(
        stream: ffmpeg_next::format::context::Input,
        stream_index: usize,
        packet_queue: Arc<BlockingDelayQueue<DelayItem<Packet>>>,
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
    pub fn new(
        decoder: ffmpeg_next::decoder::Video,
        time_base: Rational,
        packet_queue: Arc<BlockingDelayQueue<DelayItem<Packet>>>,
        video_queue: Arc<BlockingDelayQueue<DelayItem<Video>>>,
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

impl Player {
    const PACKET_QUEUE_SIZE: usize = 60;
    const FRAME_QUEUE_SIZE: usize = 60;

    pub fn new() -> Self {
        Self {
            uri: "".to_owned(),
            width: 0,
            height: 0,
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
        ffmpeg_next::init().map_err(to_player_error)?;
        self.uri = uri.to_owned();
        //let path = Path::new(&self.uri);
        let input = input(&Path::new(&self.uri)).map_err(to_player_error)?;

        let video_stream_input = input
            .streams()
            .best(Type::Video)
            .ok_or(ffmpeg_next::Error::StreamNotFound)
            .map_err(to_player_error)?;
        let video_stream_index = video_stream_input.index();
        let video_stream_tb = video_stream_input.time_base();

        let context_decoder =
            ffmpeg_next::codec::context::Context::from_parameters(video_stream_input.parameters())
                .map_err(to_player_error)?;
        let decoder = context_decoder.decoder().video().map_err(to_player_error)?;

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
                let mut sent_eof = false;

                'demuxing: loop {
                    if let Some((stream, packet)) = demuxer_data.stream.packets().next() {
                        if stream.index() == demuxer_data.stream_index {
                            sent_eof = false;
                            demuxer_data
                                .packet_queue
                                .add(DelayItem::new(packet, Instant::now()));
                        }
                    } else if !sent_eof {
                        sent_eof = true;
                        let packet = Packet::new(0);
                        demuxer_data
                            .packet_queue
                            .add(DelayItem::new(packet, Instant::now()));
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
                .map_err(to_player_error)?;

                let mut last_frame_time: u64 = 0;
                let mut presentation_time = Instant::now();

                let mut receive_and_process_decoded_frame =
                    |decoder: &mut ffmpeg_next::decoder::Video,
                     video_producer_queue: &Arc<BlockingDelayQueue<DelayItem<Video>>>,
                     presentation_time: &mut Instant|
                     -> Result<bool, PlayerError> {
                        let mut decoded = Video::empty();
                        let status = decoder.receive_frame(&mut decoded);
                        match status {
                            Err(err) => match err {
                                ffmpeg_next::Error::Eof => {
                                    decoder_data
                                        .video_queue
                                        .add(DelayItem::new(Video::empty(), Instant::now()));
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
                                    .map_err(to_player_error)?;
                                rgb_frame.set_pts(decoded.timestamp());

                                let deocded_timestamp = decoded.timestamp().unwrap_or(0);
                                let frame_time = deocded_timestamp.rescale_with(
                                    decoder_data.time_base,
                                    Rational(1, 1000),
                                    Rounding::Zero,
                                ) as u64;

                                println!(
                                    "Queue frame with pts {} and timestamp {}",
                                    deocded_timestamp, frame_time,
                                );

                                let frame_diff = frame_time - last_frame_time;

                                last_frame_time = frame_time;

                                *presentation_time += Duration::from_millis(frame_diff);
                                println!("add to video queue");
                                video_producer_queue
                                    .add(DelayItem::new(rgb_frame, *presentation_time));
                                println!(
                                    "got back from adding to video queue running={}",
                                    decoder_data.running.upgrade().is_none()
                                );
                                Ok(decoder_data.running.upgrade().is_none())
                            }
                        }
                    };

                'decoding: loop {
                    let packet_delay_item = decoder_data.packet_queue.take();
                    let packet = packet_delay_item.data;

                    if packet.size() != 0 {
                        decoder_data
                            .decoder
                            .send_packet(&packet)
                            .map_err(to_player_error)?;
                    } else {
                        decoder_data.decoder.send_eof().map_err(to_player_error)?;
                    }

                    let is_eof = receive_and_process_decoded_frame(
                        &mut decoder_data.decoder,
                        &decoder_data.video_queue,
                        &mut presentation_time,
                    )?;
                    println!("received frame is_eof={}", is_eof);
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

    pub fn video_queue(&self) -> Arc<BlockingDelayQueue<DelayItem<Video>>> {
        self.video_queue.clone()
    }
}