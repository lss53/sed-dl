// src/models.rs

use crate::{
    cli::Cli, client::RobustClient, config::AppConfig, downloader::DownloadManager,
};
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex as TokioMutex;

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