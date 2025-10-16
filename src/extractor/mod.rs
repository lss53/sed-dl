// src/extractor/mod.rs

pub mod chapter_resolver;
pub mod course;
pub mod textbook;
pub mod sync_classroom; // <--- 新增
mod utils; // <--- 新增

use crate::{error::*, models::FileInfo, DownloadJobContext};
use async_trait::async_trait;

#[async_trait]
pub trait ResourceExtractor: Send + Sync {
    async fn extract_file_info(
        &self,
        resource_id: &str,
        context: &DownloadJobContext,
    ) -> AppResult<Vec<FileInfo>>;
}