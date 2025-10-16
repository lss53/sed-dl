// src/extractor/sync_classroom.rs

use super::{utils as extractor_utils, ResourceExtractor};
use crate::{
    client::RobustClient,
    config::AppConfig,
    constants,
    error::*,
    models::{
        api::{CourseResource, SyncClassroomResponse},
        FileInfo,
    },
    utils, DownloadJobContext,
};
use async_trait::async_trait;
use log::info;
use regex::Regex;
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, LazyLock},
};

static RES_REF_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([\d,*]+)\]$").unwrap());

pub struct SyncClassroomExtractor {
    http_client: Arc<RobustClient>,
    url_template: String,
}

impl SyncClassroomExtractor {
    pub fn new(
        http_client: Arc<RobustClient>,
        _config: Arc<AppConfig>,
        url_template: String,
    ) -> Self {
        Self {
            http_client,
            url_template,
        }
    }

    fn parse_res_ref_indices(
        &self,
        ref_str: &str,
        total_resources: usize,
    ) -> Option<Vec<usize>> {
        RES_REF_RE.captures(ref_str).and_then(|caps| {
            caps.get(1).map(|m| {
                if m.as_str() == "*" {
                    (0..total_resources).collect()
                } else {
                    m.as_str()
                        .split(',')
                        .filter_map(|s| s.trim().parse::<usize>().ok())
                        .collect()
                }
            })
        })
    }

    fn process_resource(
        &self,
        resource: &CourseResource,
        base_path: &Path,
        teacher_name: &str,
    ) -> Vec<FileInfo> {
        let title = utils::sanitize_filename(&resource.global_title.zh_cn);
        let alias = utils::sanitize_filename(
            resource
                .custom_properties
                .alias_name
                .as_deref()
                .unwrap_or(""),
        );
        let base_name = format!("{} - {}", &title, &alias);

        match resource.resource_type_code.as_str() {
            constants::api::resource_types::ASSETS_VIDEO => {
                extractor_utils::extract_video_files(resource, &base_name, base_path, teacher_name)
            }
            constants::api::resource_types::ASSETS_DOCUMENT
            | constants::api::resource_types::COURSEWARES
            | constants::api::resource_types::LESSON_PLANDESIGN => {
                if let Some(mut file_info) =
                    extractor_utils::extract_document_file(resource)
                {
                    let filename = format!("{} - [{}].pdf", &base_name, teacher_name);
                    file_info.filepath = base_path.join(filename);
                    vec![file_info]
                } else {
                    info!("在资源 '{}' 中未找到可下载的 PDF 版本，跳过。", &title);
                    vec![]
                }
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl ResourceExtractor for SyncClassroomExtractor {
    async fn extract_file_info(
        &self,
        resource_id: &str,
        context: &DownloadJobContext,
    ) -> AppResult<Vec<FileInfo>> {
        info!(
            "使用 SyncClassroomExtractor 提取资源, ID: {}",
            resource_id
        );
        let data: SyncClassroomResponse = self
            .http_client
            .fetch_json(&self.url_template, &[("resource_id", resource_id)])
            .await?;

        let teacher_map: HashMap<&str, &str> = data
            .teacher_list
            .iter()
            .map(|t| (t.id.as_str(), t.name.as_str()))
            .collect();
        
        // [FIXED] 修正字段名
        let all_resources = &data.relations.resources;

        let mut all_files = Vec::new();

        // [FIXED] 修正对 Option 的访问
        if let Some(lessons) = data.resource_structure.as_ref().and_then(|rs| rs.relations.as_ref()) {
            for lesson in lessons {
                let lesson_path = if context.args.flat {
                    Path::new("").to_path_buf()
                } else {
                    Path::new(&utils::sanitize_filename(&lesson.title)).to_path_buf()
                };

                // [FIXED] 修正对 Option 的访问
                let teacher_name = lesson
                    .custom_properties
                    .teacher_ids
                    .as_deref()
                    .and_then(|ids| ids.first())
                    .and_then(|id| teacher_map.get(id.as_str()))
                    .map_or("未知教师", |&name| name);

                // [FIXED] 修正迭代逻辑
                let indices: Vec<usize> = lesson
                    .res_ref.as_deref().unwrap_or_default()
                    .iter()
                    .filter_map(|r| self.parse_res_ref_indices(r, all_resources.len()))
                    .flatten()
                    .collect();

                for index in indices {
                    if let Some(resource) = all_resources.get(index) {
                        all_files.extend(self.process_resource(resource, &lesson_path, teacher_name));
                    }
                }
            }
        }

        info!(
            "为同步课 '{}' 提取到 {} 个文件",
            resource_id,
            all_files.len()
        );
        Ok(all_files)
    }
}