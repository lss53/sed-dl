// src/extractor/course.rs

use super::{
    ResourceExtractor, chapter_resolver::ChapterTreeResolver, textbook::TextbookExtractor,
};
use crate::{
    DownloadJobContext,
    client::RobustClient,
    config::AppConfig,
    constants,
    error::*,
    models::{
        FileInfo,
        api::{CourseDetailsResponse, CourseResource, TiItem},
    },
    symbols, ui, utils,
};
use async_trait::async_trait;
use log::{debug, info, trace, warn};
use regex::Regex;
use std::sync::LazyLock;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

static REF_INDEX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([\d,*]+)\]$").unwrap());

pub struct CourseExtractor {
    http_client: Arc<RobustClient>,
    config: Arc<AppConfig>,
    chapter_resolver: ChapterTreeResolver,
    url_template: String,
}

#[derive(Debug)]
struct VideoStream<'a> {
    resolution: String,
    ti_item: &'a TiItem,
}

impl CourseExtractor {
    pub fn new(
        http_client: Arc<RobustClient>,
        config: Arc<AppConfig>,
        url_template: String,
    ) -> Self {
        let chapter_resolver = ChapterTreeResolver::new(http_client.clone(), config.clone());
        Self {
            http_client,
            config,
            chapter_resolver,
            url_template,
        }
    }

    async fn get_base_directory(&self, data: &CourseDetailsResponse) -> PathBuf {
        let course_title = &data.global_title.zh_cn;
        let textbook_path = TextbookExtractor::new(self.http_client.clone(), self.config.clone())
            .build_resource_path(data.tag_list.as_deref());

        let mut full_chapter_path = PathBuf::new();
        #[allow(clippy::collapsible_if)]
        if let (Some(tm_info), Some(path_str)) = (
            &data.custom_properties.teachingmaterial_info,
            data.chapter_paths.as_ref().and_then(|p| p.first()),
        ) {
            if let Ok(path) = self
                .chapter_resolver
                .get_full_chapter_path(&tm_info.id, path_str)
                .await
            {
                full_chapter_path = path;
            }
        }


        let course_title_sanitized = utils::sanitize_filename(course_title);
        let parent_path = if full_chapter_path.file_name().and_then(|s| s.to_str())
            == Some(&course_title_sanitized)
        {
            full_chapter_path
                .parent()
                .unwrap_or(&full_chapter_path)
                .to_path_buf()
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
        base_dir: &Path,
        teacher_map: &HashMap<usize, String>,
        selected_qualities: &Option<Vec<String>>,
    ) -> Vec<FileInfo> {
        let title = utils::sanitize_filename(&resource.global_title.zh_cn);
        let type_name = utils::sanitize_filename(
            resource
                .custom_properties
                .alias_name
                .as_deref()
                .unwrap_or(""),
        );
        let teacher = teacher_map
            .get(&index)
            .cloned()
            .unwrap_or_else(|| constants::UNCLASSIFIED_DIR.to_string());

        let mut files = Vec::new();
        if resource.resource_type_code == constants::api::resource_types::ASSETS_VIDEO {
            files.extend(self.process_video_resource(
                resource,
                &title,
                &type_name,
                &teacher,
                base_dir,
                selected_qualities,
            ));
        } else {
            // 将 allow 属性放在它要作用的 if 语句的正上方
            #[allow(clippy::collapsible_if)] 
            if [
                constants::api::resource_types::ASSETS_DOCUMENT,
                constants::api::resource_types::COURSEWARES,
                constants::api::resource_types::LESSON_PLANDESIGN,
            ]
            .contains(&resource.resource_type_code.as_str())
            {
                if let Some(file_info) = self.process_document_resource(
                    resource, &title, &type_name, &teacher, base_dir,
                ) {
                    files.push(file_info);
                }
            }
        }

        files
    }

    fn parse_res_ref_indices(&self, ref_str: &str, total_resources: usize) -> Option<Vec<usize>> {
        REF_INDEX_RE.captures(ref_str).and_then(|caps| {
            caps.get(1).map(|m| {
                if m.as_str() == "*" {
                    (0..total_resources).collect()
                } else {
                    m.as_str()
                        .split(',')
                        .filter_map(|s| s.parse::<usize>().ok())
                        .collect()
                }
            })
        })
    }

    fn get_teacher_map(&self, data: &CourseDetailsResponse) -> HashMap<usize, String> {
        let teacher_id_map: HashMap<_, _> = data
            .teacher_list
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|t| (t.id.as_str(), t.name.as_str()))
            .collect();
        trace!("教师ID映射: {:?}", teacher_id_map);

        let ids_to_names_str = |ids: &[String]| -> String {
            let names: Vec<_> = ids
                .iter()
                .filter_map(|id| teacher_id_map.get(id.as_str()).cloned())
                .collect();
            if names.is_empty() {
                constants::UNCLASSIFIED_DIR.to_string()
            } else {
                names.join(", ")
            }
        };

        let mut resource_teacher_map = HashMap::new();
        let total_resources = data.relations.resources.as_ref().map_or(0, |r| r.len());

        if let Some(relations) = data
            .resource_structure
            .as_ref()
            .and_then(|s| s.relations.as_ref())
        {
            for relation in relations {
                if let (Some(teacher_ids), Some(refs)) =
                    (&relation.custom_properties.teacher_ids, &relation.res_ref)
                {
                    let teacher_str = ids_to_names_str(teacher_ids);
                    let indices: Vec<usize> = refs
                        .iter()
                        .filter_map(|r| self.parse_res_ref_indices(r, total_resources))
                        .flatten()
                        .collect();

                    if !indices.is_empty() {
                        for index in indices {
                            resource_teacher_map.insert(index, teacher_str.clone());
                        }
                    }
                }
            }
            if !resource_teacher_map.is_empty() {
                debug!("从 resource_structure 成功映射教师信息");
                return resource_teacher_map;
            }
        }

        if let Some(top_level_teacher_ids) = data.custom_properties.lesson_teacher_ids.as_deref()
    && !top_level_teacher_ids.is_empty() {
            let teacher_str = ids_to_names_str(top_level_teacher_ids);
            for i in 0..total_resources {
                resource_teacher_map.insert(i, teacher_str.clone());
            }
            debug!("从顶层 custom_properties 成功映射教师信息");
            return resource_teacher_map;
        }

        warn!("未能在 API 响应中找到明确的教师与资源关联信息");
        resource_teacher_map
    }

    async fn negotiate_video_qualities(
        &self,
        resources: &[CourseResource],
        context: &DownloadJobContext,
    ) -> Option<Vec<String>> {
        let mut all_resolutions = BTreeSet::new();
        for res in resources {
            if let Some(streams) = self.get_available_video_streams(res.ti_items.as_deref()) {
                for stream in streams {
                    all_resolutions.insert(stream.resolution);
                }
            }
        }
        if all_resolutions.is_empty() {
            info!("课程中未找到任何视频流。");
            return None;
        }

        let mut sorted_resolutions: Vec<String> = all_resolutions.into_iter().collect();
        sorted_resolutions.sort_by_key(|r| r.replace('p', "").parse::<u32>().unwrap_or(0));
        sorted_resolutions.reverse();
        debug!("课程中所有可用清晰度: {:?}", sorted_resolutions);

        if context.non_interactive
            || context.args.video_quality != constants::DEFAULT_VIDEO_QUALITY
        {
            info!(
                "非交互模式或已指定清晰度，选择: '{}'",
                &context.args.video_quality
            );
            return Some(vec![context.args.video_quality.clone()]);
        }
        let selected = ui::get_user_choices_from_menu(
            &sorted_resolutions,
            "选择视频清晰度",
            constants::DEFAULT_SELECTION,
        );
        if selected.is_empty() {
            None
        } else {
            info!("用户选择的清晰度: {:?}", selected);
            Some(selected)
        }
    }

    fn get_available_video_streams<'a>(
        &self,
        ti_items: Option<&'a [TiItem]>,
    ) -> Option<Vec<VideoStream<'a>>> {
        let streams = ti_items?
            .iter()
            .filter_map(|item| {
                if item.ti_format != constants::api::resource_formats::M3U8 {
                    return None;
                }
                let height_str = item
                    .custom_properties
                    .as_ref()?
                    .requirements
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .find(|req| req.name == "Height")?
                    .value
                    .as_str();

                let height = height_str.parse::<u32>().ok()?;
                Some(VideoStream {
                    resolution: format!("{}p", height),
                    ti_item: item,
                })
            })
            .collect::<Vec<_>>();

        if streams.is_empty() {
            None
        } else {
            Some(streams)
        }
    }

    fn select_stream_non_interactive<'a>(
        &self,
        streams: &'a [VideoStream],
        quality: &str,
    ) -> Option<(&'a TiItem, &'a str)> {
        if streams.is_empty() {
            return None;
        }
        if quality == "best" {
            return Some((streams[0].ti_item, &streams[0].resolution));
        }
        if quality == "worst" {
            let worst = streams.last().unwrap();
            return Some((worst.ti_item, &worst.resolution));
        }
        for stream in streams {
            if stream.resolution == quality {
                return Some((stream.ti_item, &stream.resolution));
            }
        }
        warn!(
            "未找到指定清晰度 '{}'，将自动选择最高清晰度 '{}'。",
            quality, streams[0].resolution
        );
        eprintln!(
            "{} 未找到指定清晰度 '{}'，将自动选择最高清晰度 '{}'。",
            *symbols::WARN,
            quality,
            streams[0].resolution
        );
        Some((streams[0].ti_item, &streams[0].resolution))
    }

    fn find_best_document_item<'a>(&self, ti_items: Option<&'a [TiItem]>) -> Option<&'a TiItem> {
        ti_items?
            .iter()
            .find(|i| i.ti_format == constants::api::resource_formats::PDF)
            .or_else(|| ti_items?.iter().find(|i| i.ti_storages.is_some()))
    }

    fn process_video_resource(
        &self,
        resource: &CourseResource,
        title: &str,
        type_name: &str,
        teacher: &str,
        base_dir: &Path,
        selected_qualities: &Option<Vec<String>>,
    ) -> Vec<FileInfo> {
        let Some(qualities) = selected_qualities else {
            return vec![];
        };
        let Some(mut streams) = self.get_available_video_streams(resource.ti_items.as_deref())
        else {
            return vec![];
        };

        streams.sort_by_key(|s| s.resolution.replace('p', "").parse::<u32>().unwrap_or(0));
        streams.reverse();

        qualities
            .iter()
            .filter_map(|quality| {
                let (ti_item, actual_quality) =
                    self.select_stream_non_interactive(&streams, quality)?;
                let url = ti_item.ti_storages.as_ref()?.first()?;

                let base_name = format!("{} - {} [{}]", title, type_name, actual_quality);
                let filename = format!("{} - [{}].ts", base_name, teacher);

                // 从 custom_properties 中解析出真实的视频总大小
                let video_total_size = ti_item
                    .custom_properties
                    .as_ref()
                    .and_then(|props| props.requirements.as_ref())
                    .and_then(|reqs| reqs.iter().find(|r| r.name == "total_size"))
                    .and_then(|r| r.value.parse::<u64>().ok());

                // 在创建 FileInfo 时，使用解析出的 video_total_size
                Some(FileInfo {
                    filepath: base_dir.join(filename),
                    url: url.clone(),
                    ti_md5: None, // 对于M3U8视频，API的MD5无效，设为None
                    ti_size: video_total_size,
                    date: resource.update_time,
                })
            })
            .collect()
    }

    fn process_document_resource(
        &self,
        resource: &CourseResource,
        title: &str,
        type_name: &str,
        teacher: &str,
        base_dir: &Path,
    ) -> Option<FileInfo> {
        let ti_item = self.find_best_document_item(resource.ti_items.as_deref())?;
        let url = ti_item.ti_storages.as_ref()?.first()?;

        let ext = &ti_item.ti_format;
        let filename = format!("{} - {} - [{}].{}", title, type_name, teacher, ext);

        Some(FileInfo {
            filepath: base_dir.join(filename),
            url: url.clone(),
            ti_md5: ti_item.ti_md5.clone(),
            ti_size: ti_item.ti_size,
            date: resource.update_time,
        })
    }
}

#[async_trait]
impl ResourceExtractor for CourseExtractor {
    async fn extract_file_info(
        &self,
        resource_id: &str,
        context: &DownloadJobContext,
    ) -> AppResult<Vec<FileInfo>> {
        info!("开始提取课程资源, ID: {}", resource_id);
        let data: CourseDetailsResponse = self
            .http_client
            .fetch_json(&self.url_template, &[("resource_id", resource_id)])
            .await?;

        let base_dir = self.get_base_directory(&data).await;
        let teacher_map = self.get_teacher_map(&data);
        let all_resources = data.relations.resources.unwrap_or_default();

        if all_resources.is_empty() {
            info!("课程 '{}' 下未找到任何资源。", resource_id);
            println!("{} 未在该课程下找到任何资源。", *symbols::WARN);
            return Ok(vec![]);
        }
        debug!("找到 {} 个相关资源。", all_resources.len());

        let user_selected_qualities = self
            .negotiate_video_qualities(&all_resources, context)
            .await;

        let results: Vec<FileInfo> = all_resources
            .iter()
            .enumerate()
            .flat_map(|(index, resource)| {
                self.process_single_resource(
                    resource,
                    index,
                    &base_dir,
                    &teacher_map,
                    &user_selected_qualities,
                )
            })
            .collect();

        info!("为课程 '{}' 提取到 {} 个文件", resource_id, results.len());
        Ok(results)
    }
}
