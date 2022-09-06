extern crate ffmpeg_next as ffmpeg;
extern crate sdl2;

use blocking_delay_queue::{BlockingDelayQueue, DelayItem};
use ffmpeg::format::{input, Pixel};
use ffmpeg::media::Type;
use ffmpeg::software::scaling::{context::Context, flag::Flags};
use ffmpeg::util::frame::video::Video;
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::{Color, PixelFormatEnum};
use std::env;
use std::io::prelude::*;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

fn main() -> Result<(), ffmpeg::Error> {
    ffmpeg::init().unwrap();
    let sdl_context = sdl2::init().unwrap();
    let video_subsystem = sdl_context.video().unwrap();

    if let Ok(mut ictx) = input(&env::args().nth(1).expect("Cannot open file.")) {
        let input = ictx
            .streams()
            .best(Type::Video)
            .ok_or(ffmpeg::Error::StreamNotFound)?;
        let video_stream_index = input.index();

        let context_decoder = ffmpeg::codec::context::Context::from_parameters(input.parameters())?;
        let mut decoder = context_decoder.decoder().video()?;

        let window = video_subsystem
            .window("ffplay", decoder.width(), decoder.height())
            .position_centered()
            .build()
            .unwrap();

        let mut canvas = window.into_canvas().build().unwrap();
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();
        canvas.present();
        let mut event_pump = sdl_context.event_pump().unwrap();

        let texture_creator = canvas.texture_creator();

        let video_queue = Arc::new(BlockingDelayQueue::new_with_capacity(16));

        let mut sent_eof = false;

        let video_producer_queue = video_queue.clone();
        let video_producer_handle: JoinHandle<Result<(), ffmpeg::Error>> =
            thread::spawn(move || -> Result<(), ffmpeg::Error> {
                let mut scaler = Context::get(
                    decoder.format(),
                    decoder.width(),
                    decoder.height(),
                    Pixel::RGB24,
                    decoder.width(),
                    decoder.height(),
                    Flags::BILINEAR,
                )?;

                let mut receive_and_process_decoded_frame =
                    |decoder: &mut ffmpeg::decoder::Video,
                     video_producer_queue: &Arc<BlockingDelayQueue<DelayItem<Video>>>|
                     -> Result<bool, ffmpeg::Error> {
                        let mut decoded = Video::empty();
                        let status = decoder.receive_frame(&mut decoded);
                        match status {
                            Err(err) => match err {
                                ffmpeg::Error::Eof => {
                                    video_producer_queue
                                        .add(DelayItem::new(Video::empty(), Instant::now()));
                                    Ok(true)
                                }
                                ffmpeg::Error::Other { errno } => match errno {
                                    ffmpeg::util::error::EAGAIN => Ok(false),
                                    _ => Err(ffmpeg::Error::Other { errno }),
                                },
                                _ => Err(err),
                            },
                            Ok(()) => {
                                let mut rgb_frame = Video::empty();
                                scaler.run(&decoded, &mut rgb_frame)?;
                                rgb_frame.set_pts(decoded.pts());
                                video_producer_queue.add(DelayItem::new(rgb_frame, Instant::now()));
                                Ok(false)
                            }
                        }
                    };

                'decoding: loop {
                    if let Some((stream, packet)) = ictx.packets().next() {
                        if stream.index() == video_stream_index {
                            decoder.send_packet(&packet)?;
                        }
                    } else if !sent_eof {
                        sent_eof = true;
                        decoder.send_eof()?;
                    }

                    let is_eof =
                        receive_and_process_decoded_frame(&mut decoder, &video_producer_queue)?;
                    if is_eof {
                        break 'decoding;
                    }
                }
                println!("################### return from spawn");
                Ok(())
            });

        'running: loop {
            canvas.clear();
            for event in event_pump.poll_iter() {
                match event {
                    Event::Quit { .. }
                    | Event::KeyDown {
                        keycode: Some(Keycode::Escape),
                        ..
                    } => break 'running,
                    _ => {}
                }
            }

            let rgb_frame_delay_item = video_queue.take();
            let rgb_frame = rgb_frame_delay_item.data;

            if unsafe { rgb_frame.is_empty() } {
                break 'running;
            }

            let mut texture = texture_creator
                .create_texture_streaming(
                    PixelFormatEnum::RGB24,
                    rgb_frame.width(),
                    rgb_frame.height(),
                )
                .map_err(|e| e.to_string())
                .unwrap();

            let pts = rgb_frame.pts().unwrap();
            println!("write to texture {pts}");
            texture
                .with_lock(None, |buffer: &mut [u8], _pitch: usize| {
                    assert!(rgb_frame.planes() == 1);
                    rgb_frame.data(0).read_exact(buffer).unwrap();
                })
                .unwrap();

            canvas.copy(&texture, None, None).unwrap();

            canvas.present();
            //::std::thread::sleep(Duration::new(0, 1_000_000_000u32 / 60));
        }

        video_producer_handle.join().unwrap().unwrap();
    }

    Ok(())
}
