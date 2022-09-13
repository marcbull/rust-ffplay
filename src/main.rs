extern crate sdl2;

#[macro_use]
extern crate derive_new;

mod file_decoder;

use error_stack::Result;
use file_decoder::FileDecoderError;
use log::{debug, info, trace};
use partial_min_max::{max, min};
use sdl2::{
    event::{Event, WindowEvent},
    keyboard::Keycode,
    pixels::{Color, PixelFormatEnum},
    render::TextureValueError,
    render::WindowCanvas,
};
use std::{
    env, fmt,
    io::prelude::*,
    thread,
    time::{Duration, Instant},
};

use crate::file_decoder::VideoData;

#[derive(Debug)]
pub enum FFplayError {
    PlayerError(error_stack::Report<FileDecoderError>),
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
    env_logger::init();
    let sdl_context = sdl2::init().map_err(FFplayError::SDL2InitError)?;
    let video_subsystem = sdl_context.video().unwrap();

    let mut player = file_decoder::FileDecoder::new();
    player
        .start(&env::args().nth(1).expect("Cannot open file."))
        .map_err(FFplayError::PlayerError)?;

    let def_window_width: u32 = 1920;
    let def_window_height: u32 = 1080;

    info!(
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

    let mut paused = false;
    let mut presentation_time = Instant::now();
    let mut video_data_item: Option<VideoData> = None;
    let mut last_pts: u64 = 0;
    let mut seek_serial: u64 = 0;
    let seek_secs: i64 = 10000;
    'running: loop {
        canvas.clear();
        if paused {
            for event in event_pump.wait_iter() {
                match event {
                    Event::Quit { .. }
                    | Event::KeyDown {
                        keycode: Some(Keycode::Escape),
                        ..
                    } => break 'running,
                    Event::KeyDown {
                        keycode: Some(keycode),
                        ..
                    } => match keycode {
                        Keycode::Space => {
                            if paused {
                                presentation_time = Instant::now();
                            }
                            paused = !paused;
                            player
                                .set_paused(paused)
                                .map_err(FFplayError::PlayerError)?;
                            debug!("space pressed paused={}", paused);
                            continue 'running;
                        }
                        Keycode::Left => {
                            let seek_to = last_pts as i64 - seek_secs;
                            debug!("seek to {}", seek_to);
                            seek_serial = player.seek(seek_to);
                            continue 'running;
                        }
                        Keycode::Right => {
                            let seek_to = last_pts as i64 + seek_secs;
                            debug!("seek to {}", seek_to);
                            seek_serial = player.seek(seek_to);
                            continue 'running;
                        }
                        _ => {}
                    },
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
        } else {
            for event in event_pump.poll_iter() {
                match event {
                    Event::Quit { .. }
                    | Event::KeyDown {
                        keycode: Some(Keycode::Escape),
                        ..
                    } => break 'running,
                    Event::KeyDown {
                        keycode: Some(keycode),
                        ..
                    } => match keycode {
                        Keycode::Space => {
                            if paused {
                                presentation_time = Instant::now();
                            }
                            paused = !paused;
                            player
                                .set_paused(paused)
                                .map_err(FFplayError::PlayerError)?;
                            debug!("space pressed paused={}", paused);
                            continue 'running;
                        }
                        Keycode::Left => {
                            let seek_to = last_pts as i64 - seek_secs;
                            debug!("seek to {}", seek_to);
                            seek_serial = player.seek(seek_to);
                            continue 'running;
                        }
                        Keycode::Right => {
                            let seek_to = last_pts as i64 + seek_secs;
                            debug!("seek to {}", seek_to);
                            seek_serial = player.seek(seek_to);
                            continue 'running;
                        }
                        _ => {}
                    },
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
        }

        if paused {
            continue 'running;
        }

        if video_data_item.is_none() {
            trace!("ffplay: get from video queue");
            video_data_item = video_queue.take().data;
            trace!("ffplay: return from get in video queue");
            if video_data_item.is_none() {
                trace!("ffplay: item is none, break running");
                break 'running;
            }
        }

        let video_data = video_data_item.unwrap();

        if video_data.serial == seek_serial {
            let now = Instant::now();
            last_pts = video_data.frame_time;
            let frame_time = Duration::from_millis(video_data.diff_to_prev_frame);
            if presentation_time + frame_time > now {
                let sleep_time = presentation_time + frame_time - now;
                trace!("ffplay: sleep for {:?}", sleep_time);
                thread::sleep(presentation_time + frame_time - now);
            }
            presentation_time += frame_time;

            texture
                .with_lock(None, |buffer: &mut [u8], _pitch: usize| {
                    assert!(video_data.video_frame.planes() == 1);
                    video_data.video_frame.data(0).read_exact(buffer).unwrap();
                })
                .unwrap();

            canvas.copy(&texture, None, None).unwrap();

            trace!(
                "ffplay: present frame with pts {}",
                video_data.video_frame.pts().unwrap_or_default()
            );

            canvas.present();
        } else {
            trace!("ffplay: got frame with old serial");
        }

        video_data_item = None;
    }

    player.stop();

    Ok(())
}
