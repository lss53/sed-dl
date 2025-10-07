// src/extractor/course.rs

use super::{chapter_resolver::ChapterTreeResolver, FileInfo, ResourceExtractor, textbook::TextbookExtractor};
use crate::{client::RobustClient, config::AppConfig, error::*, ui, utils, DownloadJobContext};
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset};
use colored::Colorize;
use regex::Regex;
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
};

pub struct CourseExtractor {
    http_client: Arc<RobustClient>,
    config: Arc<AppConfig>,
    chapter_resolver: ChapterTreeResolver,
    url_template: String,
}

#[derive(Debug)]
struct VideoStream {
    resolution: String,
    ti_item: Value,
}

impl CourseExtractor {
    pub fn new(http_client: Arc<RobustClient>, config: Arc<AppConfig>, url_template: String) -> Self {
        let chapter_resolver = ChapterTreeResolver::new(http_client.clone(), config.clone());
        Self {
            http_client,
            config,
            chapter_resolver,
            url_template,
        }
    }

    async fn get_base_directory(&self, data: &Value) -> PathBuf {
        let course_title = data
            .get("global_title")
            .and_then(|t| t.get("zh-CN"))
            .and_then(|v| v.as_str())
            .unwrap_or("未知课程");
        let textbook_path =
            TextbookExtractor::new(self.http_client.clone(), self.config.clone())
                .build_resource_path(data.get("tag_list"));

        let tree_id = data
            .get("custom_properties")
            .and_then(|p| p.get("teachingmaterial_info"))
            .and_then(|i| i.get("id"))
            .and_then(|v| v.as_str());
        let chapter_path_str = data
            .get("chapter_paths")
            .and_then(|p| p.as_array())
            .and_then(|a| a.get(0))
            .and_then(|v| v.as_str());

        let mut full_chapter_path = PathBuf::new();
        if let (Some(tree_id), Some(path_str)) = (tree_id, chapter_path_str) {
            if let Ok(path) = self
                .chapter_resolver
                .get_full_chapter_path(tree_id, path_str)
                .await
            {
                full_chapter_path = path;
            }
        }

        let course_title_sanitized = utils::sanitize_filename(course_title);
        let parent_path = if full_chapter_path
            .file_name()
            .and_then(|s| s.to_str())
            .map_or(false, |s| utils::sanitize_filename(s) == course_title_sanitized)
        {
            full_chapter_path
                .parent()
                .unwrap_or(&full_chapter_path)
                .to_path_buf()
        } else {
            full_chapter_path
        };

        textbook_path
            .join(parent_path)
            .join(utils::sanitize_filename(course_title))
    }

    fn get_teacher_map(&self, data: &Value) -> HashMap<usize, String> {
        let teacher_id_map: HashMap<String, String> = data
            .get("teacher_list")
            .and_then(|v| v.as_array())
            .map_or(HashMap::new(), |list| {
                list.iter()
                    .filter_map(|t| {
                        let id = t.get("id")?.as_str()?.to_string();
                        let name = t.get("name")?.as_str()?.to_string();
                        Some((id, name))
                    })
                    .collect()
            });

        let mut resource_teacher_map = HashMap::new();
        if let Some(lessons) = data
            .get("resource_structure")
            .and_then(|s| s.get("relations"))
            .and_then(|r| r.as_array())
        {
            for lesson in lessons {
                let teacher_names: Vec<String> = lesson
                    .get("custom_properties")
                    .and_then(|p| p.get("teacher_ids"))
                    .and_then(|v| v.as_array())
                    .map_or(vec![], |ids| {
                        ids.iter()
                            .filter_map(|id_val| {
                                id_val.as_str()
                                    .and_then(|id_str| teacher_id_map.get(id_str).cloned())
                            })
                            .collect()
                    });
                let teacher_name = if teacher_names.is_empty() {
                    "未知教师".to_string()
                } else {
                    teacher_names.join(", ")
                };

                if let Some(refs) = lesson.get("res_ref").and_then(|v| v.as_array()) {
                    for ref_val in refs {
                        if let Some(ref_str) = ref_val.as_str() {
                            if let Ok(indices) = self.parse_res_ref(ref_str, 1000) {
                                // Assume max resources
                                for index in indices {
                                    resource_teacher_map.insert(index, teacher_name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        resource_teacher_map
    }

    fn parse_res_ref(&self, ref_str: &str, max_index: usize) -> Result<Vec<usize>, ()> {
        if ref_str.contains("[*]") {
            return Ok((0..max_index).collect());
        }
        let re = Regex::new(r"\[([\d,]+)\]$").unwrap();
        if let Some(caps) = re.captures(ref_str) {
            let indices_str = &caps[1];
            let indices: Result<Vec<usize>, _> =
                indices_str.split(',').map(|s| s.parse::<usize>()).collect();
            return indices.map_err(|_| ());
        }
        Err(())
    }

    async fn negotiate_video_qualities(
        &self,
        resources: &[Value],
        context: &DownloadJobContext,
    ) -> Option<Vec<String>> {
        let mut all_resolutions = BTreeSet::new();
        for res in resources {
            if let Some(streams) = self.get_available_video_streams(res.get("ti_items")) {
                for stream in streams {
                    all_resolutions.insert(stream.resolution);
                }
            }
        }

        if all_resolutions.is_empty() {
            return None;
        }
        let mut sorted_resolutions: Vec<String> = all_resolutions.into_iter().collect();
        sorted_resolutions.sort_by_key(|r| r.replace('p', "").parse::<u32>().unwrap_or(0));
        sorted_resolutions.reverse();

        if context.non_interactive || &context.args.video_quality != "best" {
            return Some(vec![context.args.video_quality.clone()]);
        }

        let indices = utils::parse_selection_indices(
            &ui::selection_menu(
                &sorted_resolutions,
                "选择视频清晰度",
                "支持格式: 1, 3, 2-4, all",
                "all",
            ),
            sorted_resolutions.len(),
        );

        if indices.is_empty() {
            None
        } else {
            Some(indices.iter().map(|&i| sorted_resolutions[i].clone()).collect())
        }
    }

    fn get_available_video_streams<'a>(&self, ti_items: Option<&'a Value>) -> Option<Vec<VideoStream>> {
        let streams: Vec<_> = ti_items?
            .as_array()?
            .iter()
            .filter_map(|item| {
                if item.get("ti_format").and_then(Value::as_str)? == "m3u8" {
                    let height_str = item
                        .get("custom_properties")?
                        .get("requirements")?
                        .as_array()?
                        .iter()
                        .find_map(|p| {
                            if p.get("name").and_then(Value::as_str)? == "Height" {
                                p.get("value").and_then(Value::as_str)
                            } else {
                                None
                            }
                        })?;
                    let height = height_str.parse::<u32>().ok()?;
                    Some(VideoStream {
                        resolution: format!("{}p", height),
                        ti_item: item.clone(),
                    })
                } else {
                    None
                }
            })
            .collect();

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
    ) -> Option<(&'a Value, &'a str)> {
        if streams.is_empty() {
            return None;
        }
        if quality == "best" {
            let best_stream = &streams[0];
            return Some((&best_stream.ti_item, &best_stream.resolution));
        }
        if quality == "worst" {
            let worst_stream = streams.last().unwrap();
            return Some((&worst_stream.ti_item, &worst_stream.resolution));
        }

        for stream in streams {
            if stream.resolution == quality {
                return Some((&stream.ti_item, &stream.resolution));
            }
        }
        eprintln!(
            "{} 未找到指定清晰度 '{}'，将自动选择最高清晰度 '{}'。",
            "[!]".yellow(),
            quality,
            streams[0].resolution
        );
        let best_stream = &streams[0];
        Some((&best_stream.ti_item, &best_stream.resolution))
    }

    fn find_best_document_item<'a>(&self, ti_items: Option<&'a Value>) -> Option<&'a Value> {
        let items = ti_items?.as_array()?;
        items
            .iter()
            .find(|i| i.get("ti_format").and_then(Value::as_str) == Some("pdf"))
            .or_else(|| items.iter().find(|i| i.get("ti_storages").is_some()))
    }
}


#[async_trait]
impl ResourceExtractor for CourseExtractor {
    async fn extract_file_info(
        &self,
        resource_id: &str,
        _context: &DownloadJobContext,
    ) -> AppResult<Vec<FileInfo>> {
        let data = self
            .http_client
            .fetch_json(&self.url_template, &[("resource_id", resource_id)])
            .await?;

        let base_dir = self.get_base_directory(&data).await;
        let teacher_map = self.get_teacher_map(&data);

        let all_resources = data
            .get("relations")
            .and_then(|r| r.get("national_course_resource").or(r.get("course_resource")))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if all_resources.is_empty() {
            println!("{} 未在该课程下找到任何资源。", "[!]".yellow());
            return Ok(vec![]);
        }

        let user_selected_qualities = self
            .negotiate_video_qualities(&all_resources, _context)
            .await;
        let mut results = vec![];

        for (index, resource) in all_resources.iter().enumerate() {
            let title = utils::sanitize_filename(
                resource
                    .get("global_title")
                    .and_then(|t| t.get("zh-CN"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知标题"),
            );
            let type_name = utils::sanitize_filename(
                resource
                    .get("custom_properties")
                    .and_then(|p| p.get("alias_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| {
                        resource
                            .get("resource_type_code_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                    }),
            );
            let teacher = teacher_map
                .get(&index)
                .cloned()
                .unwrap_or_else(|| "未知教师".to_string());
            let date: Option<DateTime<FixedOffset>> =
                serde_json::from_value(resource.get("update_time").cloned().unwrap_or(Value::Null))
                    .unwrap_or(None);

            let resource_type = resource.get("resource_type_code").and_then(|v| v.as_str());

            if resource_type == Some("assets_video") {
                if let (Some(qualities), Some(mut streams)) = (
                    user_selected_qualities.as_ref(),
                    self.get_available_video_streams(resource.get("ti_items")),
                ) {
                    streams.sort_by_key(|s| s.resolution.replace('p', "").parse::<u32>().unwrap_or(0));
                    streams.reverse();

                    for quality in qualities {
                        if let Some((ti_item, actual_quality)) = self.select_stream_non_interactive(&streams, quality)
                        {
                            if let Some(url) = ti_item
                                .get("ti_storages")
                                .and_then(|v| v.as_array())
                                .and_then(|a| a.get(0))
                                .and_then(|v| v.as_str())
                            {
                                let base_name = format!("{} - {} [{}]", title, type_name, actual_quality);
                                let filename = format!("{} - [{}].ts", base_name, teacher);

                                results.push(FileInfo {
                                    filepath: base_dir.join(filename),
                                    url: url.to_string(),
                                    ti_md5: ti_item
                                        .get("ti_md5")
                                        .and_then(|v| v.as_str())
                                        .map(String::from),
                                    ti_size: ti_item.get("ti_size").and_then(|v| v.as_u64()),
                                    date,
                                });
                            }
                        }
                    }
                }
            } else if ["assets_document", "coursewares", "lesson_plandesign"]
                .contains(&resource_type.unwrap_or(""))
            {
                if let Some(ti_item) = self.find_best_document_item(resource.get("ti_items")) {
                    if let Some(url) = ti_item
                        .get("ti_storages")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.get(0))
                        .and_then(|v| v.as_str())
                    {
                        let ext = ti_item
                            .get("ti_format")
                            .and_then(|v| v.as_str())
                            .unwrap_or("bin");
                        let filename =
                            format!("{} - {} - [{}].{}", title, type_name, teacher, ext);
                        results.push(FileInfo {
                            filepath: base_dir.join(filename),
                            url: url.to_string(),
                            ti_md5: ti_item
                                .get("ti_md5")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                            ti_size: ti_item.get("ti_size").and_then(|v| v.as_u64()),
                            date,
                        });
                    }
                }
            }
        }
        Ok(results)
    }
}