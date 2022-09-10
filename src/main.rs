extern crate sdl2;

mod player;

use error_stack::Result;
use partial_min_max::{max, min};
use player::PlayerError;
use sdl2::{
    event::{Event, WindowEvent},
    keyboard::Keycode,
    pixels::{Color, PixelFormatEnum},
    render::TextureValueError,
    render::WindowCanvas,
};
use std::{env, fmt, io::prelude::*};

#[derive(Debug)]
pub enum FFplayError {
    PlayerError(error_stack::Report<PlayerError>),
    SDL2InitError(String),
    VideoSubSystemError(String),
    TextureValueError(TextureValueError),
}

impl fmt::Display for FFplayError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FFplayError::PlayerError(player_err) => {
                fmt.write_fmt(format_args!("FFplayError player error: {}", player_err))
            }
            FFplayError::SDL2InitError(err) => {
                fmt.write_fmt(format_args!("FFplayError SDL2 init error: {}", err))
            }
            FFplayError::VideoSubSystemError(err) => {
                fmt.write_fmt(format_args!("FFplayError video subsystem error: {}", err))
            }
            FFplayError::TextureValueError(tex_err) => {
                fmt.write_fmt(format_args!("FFplayError texture value error: {}", tex_err))
            }
        }
    }
}

impl std::error::Error for FFplayError {}

fn main() -> Result<(), FFplayError> {
    let sdl_context = sdl2::init().map_err(FFplayError::SDL2InitError)?;
    let video_subsystem = sdl_context.video().unwrap();

    let mut player = player::Player::new();
    player
        .start(&env::args().nth(1).expect("Cannot open file."))
        .map_err(FFplayError::PlayerError)?;

    let def_window_width: u32 = 800;
    let def_window_height: u32 = 1200;

    println!(
        "create window with {}x{}",
        def_window_width, def_window_height
    );
    let window = video_subsystem
        .window("ffplay", def_window_width, def_window_height)
        .resizable()
        .position_centered()
        .maximized()
        .allow_highdpi()
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

    let handle_window_resize = |canvas: &mut WindowCanvas, video_size: (u32, u32)| {
        let new_window_size = canvas.window().drawable_size();
        let ratio: f64 = min(
            new_window_size.0 as f64 / video_size.0 as f64,
            new_window_size.1 as f64 / video_size.1 as f64,
        );
        let new_w = video_size.0 as f64 * ratio;
        let new_h = video_size.1 as f64 * ratio;

        let new_w_i32 = new_w as i32;
        let new_h_i32 = new_h as i32;
        let new_w_w_i32 = new_window_size.0 as i32;
        let new_w_h_i32 = new_window_size.1 as i32;
        let x = max(
            (max(new_w_i32, new_w_w_i32) - min(new_w_i32, new_w_w_i32)) / 2,
            0_i32,
        );
        let y = max(
            (max(new_h_i32, new_w_h_i32) - min(new_h_i32, new_w_h_i32)) / 2,
            0_i32,
        );

        canvas.set_viewport(sdl2::rect::Rect::new(x, y, new_w as u32, new_h as u32));
    };

    handle_window_resize(&mut canvas, (player.width(), player.height()));

    'running: loop {
        canvas.clear();
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => break 'running,
                Event::Window {
                    timestamp: _,
                    window_id: _,
                    win_event: WindowEvent::Resized(_, _),
                } => {
                    handle_window_resize(&mut canvas, (player.width(), player.height()));
                }
                _ => {}
            }
        }

        let rgb_frame_delay_item = video_queue.take();
        let rgb_frame = rgb_frame_delay_item.data;

        if rgb_frame.is_none() {
            break 'running;
        }

        let rgb_frame = rgb_frame.unwrap();

        texture
            .with_lock(None, |buffer: &mut [u8], _pitch: usize| {
                assert!(rgb_frame.planes() == 1);
                rgb_frame.data(0).read_exact(buffer).unwrap();
            })
            .unwrap();

        canvas.copy(&texture, None, None).unwrap();

        canvas.present();
    }

    player.stop().map_err(FFplayError::PlayerError)?;

    Ok(())
}
