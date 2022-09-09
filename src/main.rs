extern crate sdl2;

mod player;

use error_stack::Result;
use player::PlayerError;
use sdl2::{
    event::Event,
    keyboard::Keycode,
    pixels::{Color, PixelFormatEnum},
    render::TextureValueError,
};
use std::{env, fmt, io::prelude::*};

#[derive(Debug)]
pub enum FFplayError {
    PlayerError(error_stack::Report<PlayerError>),
    TextureValueError(TextureValueError),
}

impl fmt::Display for FFplayError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FFplayError::PlayerError(player_err) => {
                fmt.write_fmt(format_args!("FFplayError: {}", player_err))
            }
            FFplayError::TextureValueError(tex_err) => {
                fmt.write_fmt(format_args!("FFplayError: {}", tex_err))
            }
        }
    }
}

impl std::error::Error for FFplayError {}

pub fn to_ffplay_error(err: error_stack::Report<PlayerError>) -> FFplayError {
    FFplayError::PlayerError(err)
}

fn main() -> Result<(), FFplayError> {
    let sdl_context = sdl2::init().unwrap();
    let video_subsystem = sdl_context.video().unwrap();

    let mut player = player::Player::new();
    player
        .start(&env::args().nth(1).expect("Cannot open file."))
        .map_err(to_ffplay_error)?;

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

    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGB24, player.width(), player.height())
        .map_err(FFplayError::TextureValueError)?;

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

        if rgb_frame.is_none() {
            break 'running;
        }

        let rgb_frame = rgb_frame.unwrap();

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
    }

    player.stop().map_err(to_ffplay_error)?;

    Ok(())
}
