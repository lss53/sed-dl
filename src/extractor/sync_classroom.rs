// src/extractor/sync_classroom.rs

use super::{common::DirectoryBuilder, ResourceExtractor, utils as extractor_utils};
use crate::{
    client::RobustClient,
    config::AppConfig,
    constants,
    error::*,
    models::{
        api::{CourseResource, SyncClassroomResponse},
        FileInfo,
    },
    ui, utils, DownloadJobContext,
};
use async_trait::async_trait;
use log::{info};
use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
};

pub struct SyncClassroomExtractor {
    http_client: Arc<RobustClient>,
    url_template: String,
}

impl SyncClassroomExtractor {
    pub fn new(
        http_client: Arc<RobustClient>,
        _config: Arc<AppConfig>, // _config 标记为未使用
        url_template: String,
    ) -> Self {
        Self {
            http_client,
            url_template,
        }
    }

    fn process_resource(
        &self,
        resource: &CourseResource,
        base_name_prefix: &str, // 接收课程标题[课时标题]作为前缀
        lesson_path: &Path,    // 接收课时子目录
        teacher_name: &str,
    ) -> Vec<FileInfo> {
        let alias = utils::sanitize_filename(
            resource.custom_properties.alias_name.as_deref().unwrap_or("资源"),
        );

        // 新的文件名基础：课程标题[课时标题] - 资源别名
        let base_name = format!("{} - {}", base_name_prefix, &alias);

        match resource.resource_type_code.as_str() {
            constants::api::resource_types::ASSETS_VIDEO => {
                // 将拼接好的 base_name 传递给下游
                extractor_utils::extract_video_files(resource, &base_name, lesson_path, teacher_name)
            }
            constants::api::resource_types::ASSETS_DOCUMENT
            | constants::api::resource_types::COURSEWARES
            | constants::api::resource_types::LESSON_PLANDESIGN => {
                if let Some(mut file_info) = extractor_utils::extract_document_file(resource) {
                    let filename = format!("{} - [{}].pdf", &base_name, teacher_name);
                    file_info.filepath = lesson_path.join(filename);
                    vec![file_info]
                } else {
                    info!("在资源 '{}' 中未找到可下载的 PDF 版本，跳过。", &resource.global_title.zh_cn);
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
        info!("使用 SyncClassroomExtractor 提取资源, ID: {}", resource_id);
        let data: SyncClassroomResponse = self
            .http_client
            .fetch_json(&self.url_template, &[("resource_id", resource_id)])
            .await?;

        // 1. 调用 Trait 方法，构建课程的根目录 (e.g., .../学科/版本/章节/)
        let base_dir = data.build_base_directory(context, self.http_client.clone(), context.config.clone()).await?;

        let teacher_map: HashMap<&str, &str> = data
            .teacher_list
            .iter()
            .map(|t| (t.id.as_str(), t.name.as_str()))
            .collect();
        
        let all_resources = &data.relations.resources;
        let mut all_files = Vec::new();

        // 获取课程主标题，用于拼接文件名
        let course_main_title = utils::sanitize_filename(data.get_resource_title());

        if let Some(lessons) = data.resource_structure.as_ref().and_then(|rs| rs.relations.as_ref()) {

            for lesson in lessons {
                let lesson_title = &lesson.title;
                
                // 2. 构建课时子目录
                let lesson_path = base_dir.join(utils::sanitize_filename(lesson_title));

                // 3. 构建文件名前缀
                let filename_prefix = format!("{}[{}]", &course_main_title, lesson_title);

                // 4. 教师名获取逻辑
                let teacher_name = lesson
                    .custom_properties
                    .teacher_ids
                    .as_deref()
                    .and_then(|ids| ids.first())
                    .and_then(|id| teacher_map.get(id.as_str()))
                    .map_or("未知教师", |&name| name);

                let indices: Vec<usize> = lesson
                    .res_ref
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|r| extractor_utils::parse_res_ref_indices(r, all_resources.len()))
                    .flatten()
                    .collect();

                for index in indices {
                    if let Some(resource) = all_resources.get(index) {
                        all_files.extend(self.process_resource(
                            resource,
                            &filename_prefix,
                            &lesson_path,
                            teacher_name,
                        ));
                    }
                }
            }
        } else {
            // 如果没有课时结构（异常情况），则将所有资源放在课程根目录下，并给出警告
            // 注意：在这种情况下，API直接在资源层级提供了 teacher_name 字段，
            // 这与在课时结构中通过 teacher_ids 查找的逻辑不同。
            ui::warn("警告: 未找到课时结构，所有文件将放在课程根目录。");
            for resource in all_resources {
                let resource_alias = resource.custom_properties.alias_name.as_deref().unwrap_or("未分类资源");
                let teacher_name = resource.custom_properties.teacher_name.as_deref().unwrap_or("未知教师");
                all_files.extend(self.process_resource(
                    resource,
                    resource_alias,
                    &base_dir,
                    teacher_name,
                ));
            }
        }

        info!("为同步课堂 '{}' 提取到 {} 个文件", resource_id, all_files.len());
        Ok(all_files)
    }
}