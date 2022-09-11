extern crate ffmpeg_next as ffmpeg;

use ffmpeg::Error;
use std::fmt;

#[derive(Debug)]
pub enum FileDecoderError {
    FfmpegError(Error),
}

impl fmt::Display for FileDecoderError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileDecoderError::FfmpegError(ffmpeg_err) => {
                fmt.write_fmt(format_args!("PlayerError: {}", ffmpeg_err))
            }
        }
    }
}

impl std::error::Error for FileDecoderError {}
