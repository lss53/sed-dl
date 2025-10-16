// src/models/mod.rs

pub mod api;

use crate::error::AppError;
use crate::symbols;
use chrono::{DateTime, FixedOffset};
use colored::{ColoredString, Colorize};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// 1. 定义 DownloadStatus 枚举 (只定义一次)
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

// 2. 为 DownloadStatus 实现 get_display_info
impl DownloadStatus {
    pub fn get_display_info(
        &self,
    ) -> (
        &'static ColoredString,
        fn(ColoredString) -> ColoredString,
        &'static str,
    ) {
        match self {
            DownloadStatus::Success => (&symbols::OK, |s| s.green(), "下载并校验成功"),
            DownloadStatus::Resumed => (&symbols::OK, |s| s.green(), "续传成功，文件有效"),
            DownloadStatus::Skipped => (&symbols::INFO, |s| s.cyan(), "文件已存在，跳过"),
            DownloadStatus::Md5Failed => (&symbols::ERROR, |s| s.red(), "校验失败 (MD5不匹配)"),
            DownloadStatus::SizeFailed => (&symbols::ERROR, |s| s.red(), "校验失败 (大小不匹配)"),
            DownloadStatus::HttpError => (&symbols::ERROR, |s| s.red(), "服务器返回错误"),
            DownloadStatus::NetworkError => (&symbols::ERROR, |s| s.red(), "网络请求失败"),
            DownloadStatus::ConnectionError => (&symbols::ERROR, |s| s.red(), "无法建立连接"),
            DownloadStatus::TimeoutError => (&symbols::WARN, |s| s.yellow(), "网络连接超时"),
            DownloadStatus::MergeError => (&symbols::ERROR, |s| s.red(), "视频分片合并失败"),
            DownloadStatus::KeyError => (&symbols::ERROR, |s| s.red(), "视频解密密钥获取失败"),
            DownloadStatus::TokenError => (&symbols::ERROR, |s| s.red(), "认证失败 (Token无效)"),
            DownloadStatus::IoError => (&symbols::ERROR, |s| s.red(), "本地文件读写错误"),
            DownloadStatus::UnexpectedError => {
                (&symbols::ERROR, |s| s.red(), "发生未预期的程序错误")
            }
        }
    }
}

// 3. 为 DownloadStatus 实现 From<&AppError>
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
            AppError::Io(_) | AppError::TempFilePersist(_) => DownloadStatus::IoError,
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

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DownloadAction {
    Skip,
    Resume,
    DownloadNew,
}

// 4. 定义 ResourceCategory (只定义一次)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceCategory {
    Video,
    Audio,
    Document,
    Other,
}

// 5. 为 ResourceCategory 实现 Default (只实现一次)
impl Default for ResourceCategory {
    fn default() -> Self {
        ResourceCategory::Other
    }
}

// 6. 修正 FileInfo 定义
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileInfo {
    pub filepath: PathBuf,
    pub url: String,
    pub ti_md5: Option<String>,
    pub ti_size: Option<u64>,
    pub date: Option<DateTime<FixedOffset>>,
    #[serde(default)]
    pub category: ResourceCategory,
}

pub struct TokenRetryResult {
    pub remaining_tasks: Option<Vec<FileInfo>>,
    pub should_abort: bool,
}