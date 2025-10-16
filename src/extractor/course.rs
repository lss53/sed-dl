// src/extractor/course.rs

use super::{chapter_resolver::ChapterTreeResolver, textbook::TextbookExtractor, utils as extractor_utils, ResourceExtractor};
use crate::{
    client::RobustClient, config::AppConfig, constants, error::*, models::{api::{CourseDetailsResponse, CourseResource}, FileInfo}, symbols, utils, DownloadJobContext,
};
use async_trait::async_trait;
use log::{debug, info, trace, warn};
use regex::Regex;
use std::{collections::HashMap, path::{Path, PathBuf}, sync::{Arc, LazyLock}};

static REF_INDEX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([\d,*]+)\]$").unwrap());

pub struct CourseExtractor {
    http_client: Arc<RobustClient>,
    config: Arc<AppConfig>,
    chapter_resolver: ChapterTreeResolver,
    url_template: String,
}

impl CourseExtractor {
    pub fn new(http_client: Arc<RobustClient>, config: Arc<AppConfig>, url_template: String) -> Self {
        Self {
            http_client: http_client.clone(),
            config: config.clone(),
            chapter_resolver: ChapterTreeResolver::new(http_client, config),
            url_template,
        }
    }

    async fn get_base_directory(&self, data: &CourseDetailsResponse, context: &DownloadJobContext) -> PathBuf {
        if context.args.flat { return PathBuf::new(); }
        let course_title = &data.global_title.zh_cn;
        let textbook_path = TextbookExtractor::new(self.http_client.clone(), self.config.clone()).build_resource_path(data.tag_list.as_deref(), context);
        let mut full_chapter_path = PathBuf::new();
        if let (Some(tm_info), Some(path_str)) = (&data.custom_properties.teachingmaterial_info, data.chapter_paths.as_ref().and_then(|p| p.first())) {
             if let Ok(path) = self.chapter_resolver.get_full_chapter_path(&tm_info.id, path_str).await {
                full_chapter_path = path;
            }
        }
        let course_title_sanitized = utils::sanitize_filename(course_title);
        let parent_path = if full_chapter_path.file_name().and_then(|s| s.to_str()) == Some(&course_title_sanitized) {
            full_chapter_path.parent().unwrap_or_else(|| Path::new("")).to_path_buf()
        } else {
            full_chapter_path
        };
        let final_path = textbook_path.join(parent_path).join(course_title_sanitized);
        debug!("课程 '{}' 的基础目录解析为: {:?}", course_title, final_path);
        final_path
    }

    fn process_single_resource(
        &self,
        resource: &CourseResource,
        index: usize,
        course_title: &str, // <--- 新增参数
        base_dir: &Path,
        teacher_map: &HashMap<usize, String>,
    ) -> Vec<FileInfo> {
        let type_name = utils::sanitize_filename(resource.custom_properties.alias_name.as_deref().unwrap_or(""));
        let teacher = teacher_map.get(&index).cloned().unwrap_or_else(|| constants::UNCLASSIFIED_DIR.to_string());
        let base_name = format!("{} - {}", course_title, &type_name);

        match resource.resource_type_code.as_str() {
            constants::api::resource_types::ASSETS_VIDEO => {
                extractor_utils::extract_video_files(resource, &base_name, base_dir, &teacher)
            }
            constants::api::resource_types::ASSETS_DOCUMENT | 
            constants::api::resource_types::COURSEWARES |
            constants::api::resource_types::LESSON_PLANDESIGN => {
                if let Some(mut file_info) = extractor_utils::extract_document_file(resource) {
                    let filename = format!("{} - [{}].pdf", &base_name, &teacher);
                    file_info.filepath = base_dir.join(filename);
                    vec![file_info]
                } else {
                    info!("在资源 '{}' 中未找到可下载的 PDF 版本，跳过。", &resource.global_title.zh_cn);
                    vec![]
                }
            }
            _ => {
                info!("跳过不支持的资源类型: {}", resource.resource_type_code);
                vec![]
            }
        }
    }

    fn parse_res_ref_indices(&self, ref_str: &str, total_resources: usize) -> Option<Vec<usize>> {
        REF_INDEX_RE.captures(ref_str).and_then(|caps| {
            caps.get(1).map(|m| {
                if m.as_str() == "*" { (0..total_resources).collect() } 
                else { m.as_str().split(',').filter_map(|s| s.parse::<usize>().ok()).collect() }
            })
        })
    }

    fn get_teacher_map(&self, data: &CourseDetailsResponse) -> HashMap<usize, String> {
        let teacher_id_map: HashMap<_, _> = data.teacher_list.as_deref().unwrap_or_default().iter().map(|t| (t.id.as_str(), t.name.as_str())).collect();
        trace!("教师ID映射: {:?}", teacher_id_map);
        let ids_to_names_str = |ids: &[String]| -> String {
            let names: Vec<_> = ids.iter().filter_map(|id| teacher_id_map.get(id.as_str()).cloned()).collect();
            if names.is_empty() { constants::UNCLASSIFIED_DIR.to_string() } else { names.join(", ") }
        };
        let mut resource_teacher_map = HashMap::new();
        
        // [FIXED] 直接在 Vec 上调用 .len()
        let total_resources = data.relations.resources.len();
        
        if let Some(relations) = data.resource_structure.as_ref().and_then(|s| s.relations.as_ref()) {
            for relation in relations {
                if let (Some(teacher_ids), Some(refs)) = (&relation.custom_properties.teacher_ids, &relation.res_ref) {
                    let teacher_str = ids_to_names_str(teacher_ids);
                    let indices: Vec<usize> = refs.iter().filter_map(|r| self.parse_res_ref_indices(r, total_resources)).flatten().collect();
                    for index in indices { resource_teacher_map.insert(index, teacher_str.clone()); }
                }
            }
            if !resource_teacher_map.is_empty() {
                debug!("从 resource_structure 成功映射教师信息");
                return resource_teacher_map;
            }
        }
        if let Some(top_level_teacher_ids) = data.custom_properties.lesson_teacher_ids.as_deref().filter(|ids| !ids.is_empty()) {
            let teacher_str = ids_to_names_str(top_level_teacher_ids);
            for i in 0..total_resources { resource_teacher_map.insert(i, teacher_str.clone()); }
            debug!("从顶层 custom_properties 成功映射教师信息");
            return resource_teacher_map;
        }
        warn!("未能在 API 响应中找到明确的教师与资源关联信息");
        resource_teacher_map
    }
}

#[async_trait]
impl ResourceExtractor for CourseExtractor {
    async fn extract_file_info(&self, resource_id: &str, context: &DownloadJobContext) -> AppResult<Vec<FileInfo>> {
        info!("使用 CourseExtractor 提取资源, ID: {}", resource_id);
        let data: CourseDetailsResponse = self.http_client.fetch_json(&self.url_template, &[("resource_id", resource_id)]).await?;
        
        let course_title = utils::sanitize_filename(&data.global_title.zh_cn);
        
        let base_dir = self.get_base_directory(&data, context).await;
        let teacher_map = self.get_teacher_map(&data);
        
        let all_resources = &data.relations.resources;

        if all_resources.is_empty() {
            info!("课程 '{}' 下未找到任何资源。", resource_id);
            println!("{} 未在该课程下找到任何资源。", *symbols::WARN);
            return Ok(vec![]);
        }
        debug!("找到 {} 个相关资源。", all_resources.len());
        let results: Vec<FileInfo> = all_resources.iter().enumerate()
            .flat_map(|(index, resource)| self.process_single_resource(resource, index, &course_title, &base_dir, &teacher_map))
            .collect();
        info!("为课程 '{}' 提取到 {} 个文件", resource_id, results.len());
        Ok(results)
    }
}