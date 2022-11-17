extern crate sdl2;

#[macro_use]
extern crate derive_new;

mod file_decoder;

use error_stack::{Context, IntoReport, Result, ResultExt};
use ffmpeg_next::format::{self, Pixel};
use log::{debug, info, trace};
use partial_min_max::{max, min};
use sdl2::{
    event::{Event, WindowEvent},
    keyboard::Keycode,
    pixels::{Color, PixelFormatEnum},
    render::TextureValueError,
    render::{UpdateTextureError, UpdateTextureYUVError, WindowCanvas},
    video::WindowBuildError,
    EventPump, IntegerOrSdlError,
};
use std::{
    env, fmt, thread,
    time::{Duration, Instant},
};

use crate::file_decoder::VideoData;

#[derive(Debug)]
enum SDL2Error {
    Init(String),
    VideoSubsystem(String),
    WindowBuild(WindowBuildError),
    EventPump(String),
    CanvasBuild(IntegerOrSdlError),
    CopyTextureToCanvas(String),
    TextureUpdate(UpdateTextureError),
    TextureUpdateYUV(UpdateTextureYUVError),
    TextureValue(TextureValueError),
}

impl fmt::Display for SDL2Error {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SDL2Error::Init(err) => fmt.write_fmt(format_args!("SDL2 init error: {}", err)),
            SDL2Error::VideoSubsystem(err) => {
                fmt.write_fmt(format_args!("SDL2 video subsystem error: {}", err))
            }
            SDL2Error::WindowBuild(err) => {
                fmt.write_fmt(format_args!("SDL2 window build error: {}", err))
            }
            SDL2Error::EventPump(err) => {
                fmt.write_fmt(format_args!("SDL2 event pump error: {}", err))
            }
            SDL2Error::CanvasBuild(err) => {
                fmt.write_fmt(format_args!("SDL2 canvas build error: {}", err))
            }
            SDL2Error::CopyTextureToCanvas(err) => {
                fmt.write_fmt(format_args!("SDL2 copy texture to canvas error: {}", err))
            }
            SDL2Error::TextureUpdate(err) => {
                fmt.write_fmt(format_args!("SDL2 texture update error: {}", err))
            }
            SDL2Error::TextureUpdateYUV(err) => {
                fmt.write_fmt(format_args!("SDL2 texture update error: {}", err))
            }
            SDL2Error::TextureValue(tex_err) => {
                fmt.write_fmt(format_args!("SDL2 texture value error: {}", tex_err))
            }
        }
    }
}

impl Context for SDL2Error {}

#[derive(Debug)]
struct FFplayError;

impl fmt::Display for FFplayError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.write_str("FFplay error")
    }
}

impl Context for FFplayError {}

enum EventState {
    Quit,
    Pause,
    SeekForward,
    SeekBackward,
    Resize,
}

fn sdl_init(
    window_width: u32,
    window_height: u32,
) -> Result<(WindowCanvas, EventPump), FFplayError> {
    let sdl_context = sdl2::init()
        .map_err(SDL2Error::Init)
        .into_report()
        .change_context(FFplayError)?;
    let video_subsystem = sdl_context
        .video()
        .map_err(SDL2Error::VideoSubsystem)
        .into_report()
        .change_context(FFplayError)?;

    info!("create window with {}x{}", window_width, window_height);
    let window = video_subsystem
        .window("ffplay", window_width, window_height)
        .resizable()
        .position_centered()
        .maximized()
        .allow_highdpi()
        .build()
        .map_err(SDL2Error::WindowBuild)
        .into_report()
        .change_context(FFplayError)?;

    let mut canvas = window
        .into_canvas()
        .build()
        .map_err(SDL2Error::CanvasBuild)
        .into_report()
        .change_context(FFplayError)?;
    canvas.set_draw_color(Color::RGB(0, 0, 0));
    canvas.clear();
    canvas.present();
    let event_pump = sdl_context
        .event_pump()
        .map_err(SDL2Error::EventPump)
        .into_report()
        .change_context(FFplayError)?;

    Ok((canvas, event_pump))
}

fn av_to_sdl_pixel_format_mapper(fmt: &format::Pixel) -> PixelFormatEnum {
    match fmt {
        format::Pixel::YUV420P => PixelFormatEnum::IYUV,
        format::Pixel::YUYV422 => PixelFormatEnum::YUY2,
        format::Pixel::UYVY422 => PixelFormatEnum::UYVY,
        _ => PixelFormatEnum::Unknown,
    }
}

fn main() -> Result<(), FFplayError> {
    env_logger::init();

    let mut player_builder =
        file_decoder::FileDecoderBuilder::new(env::args().nth(1).expect("Cannot open file."));
    let mut player = player_builder
        .pixel_format(Pixel::YUV420P)
        .build()
        .change_context(FFplayError)?;
    //.map_err(FFplayError::PlayerError)?;

    player.init().change_context(FFplayError)?;
    player.start().change_context(FFplayError)?;

    let def_window_width: u32 = 1920;
    let def_window_height: u32 = 1080;

    let (mut canvas, mut event_pump) = sdl_init(def_window_width, def_window_height)?;

    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(
            av_to_sdl_pixel_format_mapper(&player.pixel_format()),
            player.width(),
            player.height(),
        )
        .map_err(SDL2Error::TextureValue)
        .into_report()
        .change_context(FFplayError)?;

    let video_queue = player.video_queue();

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

    let event_transform = |event: Option<Event>| -> Option<EventState> {
        if let Some(event) = event {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => return Some(EventState::Quit),
                Event::KeyDown {
                    keycode: Some(keycode),
                    ..
                } => match keycode {
                    Keycode::Space => return Some(EventState::Pause),
                    Keycode::Left => return Some(EventState::SeekBackward),
                    Keycode::Right => return Some(EventState::SeekForward),
                    _ => return None,
                },
                Event::Window {
                    timestamp: _,
                    window_id: _,
                    win_event: WindowEvent::Resized(_, _),
                } => return Some(EventState::Resize),
                _ => return None,
            }
        }
        None
    };

    let event_pumper = |wait_for_event: bool, event_pump: &mut EventPump| -> Option<EventState> {
        if wait_for_event {
            event_transform(event_pump.wait_iter().next())
        } else {
            event_transform(event_pump.poll_iter().next())
        }
    };

    // Setup canvas for initial window size:
    handle_window_resize(&mut canvas, (player.width(), player.height()));

    let mut paused = false;
    let mut need_update = false;
    let mut presentation_time = Instant::now();
    let mut video_data_item: Option<VideoData> = None;
    let mut last_pts: u64 = 0;
    let mut seek_serial: u64 = 0;
    let seek_secs: i64 = 20000;
    'running: loop {
        canvas.clear();
        if let Some(event) = event_pumper(paused && !need_update, &mut event_pump) {
            match event {
                EventState::Quit => break 'running,
                EventState::Pause => {
                    if paused {
                        presentation_time = Instant::now();
                    }
                    paused = !paused;
                    debug!("space pressed paused={}", paused);
                    continue 'running;
                }
                EventState::SeekBackward => {
                    let seek_to = last_pts as i64 - seek_secs;
                    debug!("seek to {} (last_pts={})", seek_to, last_pts);
                    last_pts = seek_to as u64;
                    seek_serial = player.seek(seek_to).change_context(FFplayError)?;
                    need_update = true;
                    debug!("seek to {} (serial {})", seek_to, seek_serial);
                    continue 'running;
                }
                EventState::SeekForward => {
                    let seek_to = last_pts as i64 + seek_secs;
                    debug!("seek to {} (last_pts={})", seek_to, last_pts);
                    last_pts = seek_to as u64;
                    seek_serial = player.seek(seek_to).change_context(FFplayError)?;
                    need_update = true;
                    debug!("seek to {} (serial {})", seek_to, seek_serial);
                    continue 'running;
                }
                EventState::Resize => {
                    handle_window_resize(&mut canvas, (player.width(), player.height()));
                }
            }
        }

        if paused && !need_update {
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
            trace!(
                "change last pts from {} to {} (serial={})",
                last_pts,
                video_data.frame_time,
                seek_serial
            );
            last_pts = video_data.frame_time;
            let frame_time = Duration::from_millis(video_data.diff_to_prev_frame);
            if presentation_time + frame_time > now {
                let sleep_time = presentation_time + frame_time - now;
                trace!("ffplay: sleep for {:?}", sleep_time);
                thread::sleep(presentation_time + frame_time - now);
            }
            presentation_time += frame_time;

            if video_data.video_frame.planes() == 1 {
                texture
                    .update(
                        None,
                        video_data.video_frame.data(0),
                        video_data.video_frame.stride(0),
                    )
                    .map_err(SDL2Error::TextureUpdate)
                    .into_report()
                    .change_context(FFplayError)?;
            } else if video_data.video_frame.planes() == 2 {
                let y_plane = video_data.video_frame.data(0);
                let y_stride = video_data.video_frame.stride(0);
                let u_plane = video_data.video_frame.data(1);
                let u_stride = video_data.video_frame.stride(1);
                let v_plane = video_data.video_frame.data(2);
                let v_stride = video_data.video_frame.stride(2);

                texture
                    .update_yuv(
                        None, y_plane, y_stride, u_plane, u_stride, v_plane, v_stride,
                    )
                    .map_err(SDL2Error::TextureUpdateYUV)
                    .into_report()
                    .change_context(FFplayError)?;
            } else {
                assert!(video_data.video_frame.planes() == 3);

                let y_plane = video_data.video_frame.data(0);
                let y_stride = video_data.video_frame.stride(0);
                let u_plane = video_data.video_frame.data(1);
                let u_stride = video_data.video_frame.stride(1);
                let v_plane = video_data.video_frame.data(2);
                let v_stride = video_data.video_frame.stride(2);

                texture
                    .update_yuv(
                        None, y_plane, y_stride, u_plane, u_stride, v_plane, v_stride,
                    )
                    .map_err(SDL2Error::TextureUpdateYUV)
                    .into_report()
                    .change_context(FFplayError)?;
            }

            canvas
                .copy(&texture, None, None)
                .map_err(SDL2Error::CopyTextureToCanvas)
                .into_report()
                .change_context(FFplayError)?;

            trace!(
                "ffplay: present frame with pts {}",
                video_data.video_frame.pts().unwrap_or_default()
            );
            need_update = false;

            canvas.present();
        } else {
            trace!("ffplay: got frame with old serial");
        }

        video_data_item = None;
    }

    player.stop();

    Ok(())
}
