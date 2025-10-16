// src/downloader/dispatcher.rs

use super::job::ResourceDownloader;
use crate::{
    config::ResourceExtractorType,
    error::*,
    extractor::{course, sync_classroom, textbook, ResourceExtractor},
    utils,
};
use anyhow::anyhow;
// --- 修正: 导入缺失的宏 ---
use log::{debug, error, info};
use url::Url;

/// 这部分 `impl` 负责将 URL 或 ID 调度到正确的提取器。
impl ResourceDownloader {
    /// 从 URL 中解析出资源类型和 ID，并返回对应的提取器实例。
    pub(super) fn get_extractor_info(
        &self,
        url_str: &str,
    ) -> AppResult<(Box<dyn ResourceExtractor>, String)> {
        let url = Url::parse(url_str)?;
        debug!("解析 URL: {}", url);
        for (path_key, api_conf) in &self.context.config.api_endpoints {
            if url.path().contains(path_key) {
                debug!("URL 路径匹配 API 端点: '{}'", path_key);
                if let Some(resource_id) =
                    url.query_pairs().find(|(k, _)| k == &api_conf.id_param)
                {
                    let id = resource_id.1.to_string();
                    if utils::is_resource_id(&id) {
                        info!("从 URL 中成功提取到资源 ID: '{}' (类型: {})", id, path_key);
                        return Ok((self.create_extractor(api_conf), id));
                    }
                }
            }
        }
        error!("无法从 URL '{}' 中识别资源类型或提取ID。", url_str);
        Err(AppError::Other(anyhow!(
            "无法识别的URL格式或不支持的资源类型。"
        )))
    }

    /// 根据 API 配置创建具体的提取器实例。
    pub(super) fn create_extractor(
        &self,
        api_conf: &crate::config::ApiEndpointConfig,
    ) -> Box<dyn ResourceExtractor> {
        match api_conf.extractor {
            ResourceExtractorType::Textbook => {
                debug!("创建 TextbookExtractor");
                Box::new(textbook::TextbookExtractor::new(
                    self.context.http_client.clone(),
                    self.context.config.clone(),
                ))
            }
            ResourceExtractorType::Course => {
                let template_key = api_conf
                    .url_template_keys
                    .get("main")
                    .expect("Course API config missing 'main' template key");
                let url_template = self
                    .context
                    .config
                    .url_templates
                    .get(template_key)
                    .expect("URL template not found for key")
                    .clone();
                debug!("创建 CourseExtractor, 使用 URL 模板: {}", url_template);
                Box::new(course::CourseExtractor::new(
                    self.context.http_client.clone(),
                    self.context.config.clone(),
                    url_template,
                ))
            }
            ResourceExtractorType::SyncClassroom => {
                let template_key = api_conf
                    .url_template_keys
                    .get("main")
                    .expect("SyncClassroom API config missing 'main' template key");
                let url_template = self
                    .context
                    .config
                    .url_templates
                    .get(template_key)
                    .expect("URL template not found for key")
                    .clone();
                debug!(
                    "创建 SyncClassroomExtractor, 使用 URL 模板: {}",
                    url_template
                );
                Box::new(sync_classroom::SyncClassroomExtractor::new(
                    self.context.http_client.clone(),
                    self.context.config.clone(),
                    url_template,
                ))
            }
        }
    }
}