extern crate ffmpeg_next as ffmpeg;

use ffmpeg::Error;
use std::{fmt, sync::mpsc::SendError};

#[derive(Debug)]
pub enum FileDecoderError {
    FfmpegError(Error),
    SendError(SendError<bool>),
}

impl fmt::Display for FileDecoderError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileDecoderError::FfmpegError(err) => {
                fmt.write_fmt(format_args!("File decoder error ffmpeg: {}", err))
            }
            FileDecoderError::SendError(err) => {
                fmt.write_fmt(format_args!("File decoder error send error: {}", err))
            }
        }
    }
}

impl std::error::Error for FileDecoderError {}
