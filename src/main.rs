extern crate sdl2;

mod player;

use error_stack::Result;
use sdl2::{
    event::Event,
    keyboard::Keycode,
    pixels::{Color, PixelFormatEnum},
};
use std::{env, io::prelude::*};

fn main() -> Result<(), player::PlayerError> {
    let sdl_context = sdl2::init().unwrap();
    let video_subsystem = sdl_context.video().unwrap();

    let mut player = player::Player::new();
    player.start(&env::args().nth(1).expect("Cannot open file."))?;

    println!("create window with {}x{}", player.width(), player.height());
    let window = video_subsystem
        .window("ffplay", player.width(), player.height())
        .position_centered()
        .build()
        .unwrap();

    let mut canvas = window.into_canvas().build().unwrap();
    canvas.set_draw_color(Color::RGB(0, 0, 0));
    canvas.clear();
    canvas.present();
    let mut event_pump = sdl_context.event_pump().unwrap();

    let texture_creator = canvas.texture_creator();

    let video_queue = player.video_queue();

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

        let pts = rgb_frame.timestamp().unwrap_or(0);
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

    player.stop()?;

    Ok(())
}
