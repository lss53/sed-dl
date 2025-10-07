// src/extractor/mod.rs

use crate::{error::*, DownloadJobContext};
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use std::path::PathBuf;

// 声明子模块
pub mod chapter_resolver;
pub mod course;
pub mod textbook;

/// 描述一个待下载文件的所有信息
#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    pub filepath: PathBuf,
    pub url: String,
    pub ti_md5: Option<String>,
    pub ti_size: Option<u64>,
    pub date: Option<DateTime<FixedOffset>>,
}

/// 所有资源提取器必须实现的通用接口
#[async_trait]
pub trait ResourceExtractor: Send + Sync {
    async fn extract_file_info(
        &self,
        resource_id: &str,
        context: &DownloadJobContext,
    ) -> AppResult<Vec<FileInfo>>;
}