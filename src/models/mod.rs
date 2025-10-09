// src/models/mod.rs

pub mod api;

use crate::error::AppError;
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    pub filepath: PathBuf,
    pub url: String,
    pub ti_md5: Option<String>,
    pub ti_size: Option<u64>,
    pub date: Option<DateTime<FixedOffset>>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DownloadStatus {
    Success,
    Skipped,
    Resumed,
    Md5Failed,
    SizeFailed,
    HttpError,
    NetworkError,
    ConnectionError,
    TimeoutError,
    TokenError,
    IoError,
    MergeError,
    KeyError,
    UnexpectedError,
}

impl From<&AppError> for DownloadStatus {
    fn from(error: &AppError) -> Self {
        match error {
            AppError::TokenInvalid => DownloadStatus::TokenError,
            AppError::Network(err)
            | AppError::NetworkMiddleware(reqwest_middleware::Error::Reqwest(err)) => {
                if err.is_timeout() {
                    DownloadStatus::TimeoutError
                } else if err.is_connect() {
                    DownloadStatus::ConnectionError
                } else if err.is_status() {
                    DownloadStatus::HttpError
                } else {
                    DownloadStatus::NetworkError
                }
            }
            AppError::NetworkMiddleware(_) => DownloadStatus::NetworkError,
            AppError::Io(_) => DownloadStatus::IoError,
            AppError::M3u8Parse(_) | AppError::Merge(_) => DownloadStatus::MergeError,
            AppError::Security(_) => DownloadStatus::KeyError,
            AppError::Validation(msg) => {
                if msg.contains("MD5") {
                    DownloadStatus::Md5Failed
                } else {
                    DownloadStatus::SizeFailed
                }
            }
            _ => DownloadStatus::UnexpectedError,
        }
    }
}


#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub filename: String,
    pub status: DownloadStatus,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadAction {
    Skip,
    Resume,
    DownloadNew,
}

pub struct TokenRetryResult {
    pub remaining_tasks: Option<Vec<FileInfo>>,
    pub should_abort: bool,
}