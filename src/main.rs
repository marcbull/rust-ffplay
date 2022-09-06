extern crate ffmpeg_next as ffmpeg;
extern crate sdl2;

use ffmpeg::format::{input, Pixel};
use ffmpeg::media::Type;
use ffmpeg::software::scaling::{context::Context, flag::Flags};
use ffmpeg::util::frame::video::Video;
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::render::Texture;
use std::env;
use std::io::prelude::*;
use std::time::Duration;

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

        let mut scaler = Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            Pixel::RGB24,
            decoder.width(),
            decoder.height(),
            Flags::BILINEAR,
        )?;

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

        let mut texture = texture_creator
            .create_texture_streaming(PixelFormatEnum::RGB24, decoder.width(), decoder.height())
            .map_err(|e| e.to_string())
            .unwrap();

        let mut frame_index = 0;
        let mut sent_eof = false;

        let mut receive_and_process_decoded_frame = |decoder: &mut ffmpeg::decoder::Video,
                                                     texture: &mut Texture|
         -> Result<bool, ffmpeg::Error> {
            let mut decoded = Video::empty();
            let mut got_frame = false;
            if decoder.receive_frame(&mut decoded).is_ok() {
                let mut rgb_frame = Video::empty();
                scaler.run(&decoded, &mut rgb_frame)?;

                println!("write to texture");
                texture
                    .with_lock(None, |buffer: &mut [u8], _pitch: usize| {
                        assert!(rgb_frame.planes() == 1);
                        rgb_frame.data(0).read_exact(buffer).unwrap();
                    })
                    .unwrap();

                frame_index += 1;
                got_frame = true;
            }
            Ok(got_frame)
        };

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

            if let Some((stream, packet)) = ictx.packets().next() {
                if stream.index() == video_stream_index {
                    decoder.send_packet(&packet)?;
                }
            } else if !sent_eof {
                sent_eof = true;
                decoder.send_eof()?;
            }

            let _pts = receive_and_process_decoded_frame(&mut decoder, &mut texture)?;

            canvas.copy(&texture, None, None).unwrap();

            canvas.present();
            //::std::thread::sleep(Duration::new(0, 1_000_000_000u32 / 60));
        }
    }

    Ok(())
}
