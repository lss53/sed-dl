// src/extractor/mod.rs

pub mod chapter_resolver;
pub mod course;
pub mod sync_classroom; // <--- 新增
pub mod textbook;
mod utils; // <--- 新增

use crate::{DownloadJobContext, error::*, models::FileInfo};
use async_trait::async_trait;

#[async_trait]
pub trait ResourceExtractor: Send + Sync {
    async fn extract_file_info(
        &self,
        resource_id: &str,
        context: &DownloadJobContext,
    ) -> AppResult<Vec<FileInfo>>;
}
