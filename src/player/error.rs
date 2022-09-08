extern crate ffmpeg_next as ffmpeg;

use ffmpeg::Error;
use std::fmt;

#[derive(Debug)]
pub enum PlayerError {
    FfmpegError(Error),
}

impl fmt::Display for PlayerError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayerError::FfmpegError(ffmpeg_err) => {
                fmt.write_fmt(format_args!("PlayerError: {}", ffmpeg_err))
            }
        }
    }
}

impl std::error::Error for PlayerError {}

pub fn to_player_error(err: ffmpeg::util::error::Error) -> PlayerError {
    PlayerError::FfmpegError(err)
}
