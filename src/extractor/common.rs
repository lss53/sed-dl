// src/extractor/common.rs

use crate::{
    client::RobustClient,
    config::AppConfig,
    error::AppResult,
    extractor::{chapter_resolver::ChapterTreeResolver, textbook::TextbookExtractor},
    models::api::Tag,
    // utils,
    DownloadJobContext,
};
use async_trait::async_trait;
use std::{
    path::{
        // Path,
        PathBuf
    },
    sync::Arc,
};

/// 一个Trait，定义了能够构建基于教材和章节的深度嵌套目录的通用能力。
#[async_trait]
pub trait DirectoryBuilder {
    /// 获取资源的标题，用于目录名
    fn get_resource_title(&self) -> &str;
    /// 获取资源的标签列表
    fn get_tags(&self) -> Option<&[Tag]>;
    /// 获取资源的章节路径信息
    fn get_chapter_info(&self) -> Option<(&str, &str)>; // -> Option<(tree_id, chapter_path)>

    /// 构建基础目录的核心实现
    async fn build_base_directory(
        &self,
        context: &DownloadJobContext,
        http_client: Arc<RobustClient>,
        config: Arc<AppConfig>,
    ) -> AppResult<PathBuf> {
        if context.args.flat {
            return Ok(PathBuf::new());
        }

        let chapter_resolver = ChapterTreeResolver::new(http_client.clone(), config.clone());

        // 获取教材路径
        let textbook_path = TextbookExtractor::new(http_client, config)
            .build_resource_path(self.get_tags(), context);

        // 获取章节路径
        let mut full_chapter_path = PathBuf::new();
        if let Some((tree_id, path_str)) = self.get_chapter_info()
            && let Ok(path) = chapter_resolver.get_full_chapter_path(tree_id, path_str).await {
                full_chapter_path = path;
            }


        // 直接将教材路径和章节路径组合起来
        let final_path = textbook_path.join(full_chapter_path);
        Ok(final_path)
    }
}