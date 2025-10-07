// src/extractor/textbook.rs

use super::{FileInfo, ResourceExtractor};
use crate::{client::RobustClient, config::AppConfig, constants, error::*, ui, utils, DownloadJobContext};
use async_trait::async_trait;
use colored::Colorize;
use percent_encoding;
use regex::Regex;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use url::Url;

pub struct TextbookExtractor {
    http_client: Arc<RobustClient>,
    config: Arc<AppConfig>,
}

impl TextbookExtractor {
    pub fn new(http_client: Arc<RobustClient>, config: Arc<AppConfig>) -> Self {
        Self {
            http_client,
            config,
        }
    }

    fn extract_pdf_info(&self, data: &Value) -> (Vec<FileInfo>, Option<String>) {
        let mut results = vec![];
        let mut textbook_basename = None;

        let base_path = self.build_resource_path(data.get("tag_list"));
        if let Some(items) = data.get("ti_items").and_then(|i| i.as_array()) {
            for item in items {
                if item.get("ti_file_flag").and_then(|f| f.as_str()) == Some("source")
                    && item.get("ti_format").and_then(|f| f.as_str()) == Some("pdf")
                {
                    if let Some(url_str) = item
                        .get("ti_storages")
                        .and_then(|s| s.as_array())
                        .and_then(|a| a.get(0))
                        .and_then(|u| u.as_str())
                    {
                        if let Ok(url) = Url::parse(url_str) {
                            let path = Path::new(url.path());
                            let raw_filename =
                                path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown.pdf");
                            let decoded_filename =
                                percent_encoding::percent_decode(raw_filename.as_bytes())
                                    .decode_utf8_lossy()
                                    .to_string();

                            let name = if self.is_generic_filename(&decoded_filename) {
                                let title = data
                                    .get("global_title")
                                    .and_then(|t| t.get("zh-CN"))
                                    .and_then(|t| t.as_str())
                                    .or_else(|| data.get("title").and_then(|t| t.as_str()))
                                    .unwrap_or_else(|| {
                                        data.get("id")
                                            .and_then(|i| i.as_str())
                                            .unwrap_or("unknown_pdf")
                                    });
                                format!("{}.pdf", utils::sanitize_filename(title))
                            } else {
                                utils::sanitize_filename(&decoded_filename)
                            };

                            if textbook_basename.is_none() {
                                textbook_basename = Some(
                                    Path::new(&name)
                                        .file_stem()
                                        .unwrap()
                                        .to_string_lossy()
                                        .to_string(),
                                );
                            }

                            // .unwrap() is safe here because we construct the JSON value ourselves.
                            results.push(
                                serde_json::from_value::<FileInfo>(json!({
                                    "filepath": base_path.join(&name),
                                    "url": url_str,
                                    "ti_md5": item.get("ti_md5"),
                                    "ti_size": item.get("ti_size"),
                                    "date": data.get("update_time")
                                }))
                                .unwrap(),
                            );
                        } else {
                            eprintln!(
                                "{} {}",
                                "[!]".yellow(),
                                format!("警告: 跳过一个无效的PDF URL: {}", url_str).red()
                            );
                        }
                    }
                }
            }
        }
        (results, textbook_basename)
    }

    fn is_generic_filename(&self, filename: &str) -> bool {
        let patterns = [
            r"^pdf\.pdf$",
            r"^document\.pdf$",
            r"^file\.pdf$",
            r"^\d+\.pdf$",
            r"^[a-f0-9]{32}\.pdf$",
        ];
        patterns.iter().any(|p| {
            Regex::new(p)
                .unwrap()
                .is_match(filename.to_lowercase().as_str())
        })
    }

    async fn extract_audio_info(
        &self,
        resource_id: &str,
        base_path: PathBuf,
        textbook_basename: Option<String>,
        context: &DownloadJobContext,
    ) -> AppResult<Vec<FileInfo>> {
        let url_template = self.config.url_templates.get("TEXTBOOK_AUDIO").unwrap();
        let audio_data = self
            .http_client
            .fetch_json(url_template, &[("resource_id", resource_id)])
            .await?;

        if let Some(audio_items) = audio_data.as_array() {
            let available_formats = self.get_available_audio_formats(audio_items);
            if available_formats.is_empty() {
                return Ok(vec![]);
            }

            let selected_formats: Vec<String> = if context.non_interactive
                || &context.args.audio_format != &self.config.default_audio_format
            {
                let preferred = context.args.audio_format.to_lowercase();
                let chosen_format = if available_formats.contains(&preferred) {
                    preferred
                } else {
                    available_formats[0].clone()
                };
                vec![chosen_format]
            } else {
                let options: Vec<String> = available_formats.iter().map(|f| f.to_uppercase()).collect();
                let indices = utils::parse_selection_indices(
                    &ui::selection_menu(&options, "选择音频格式", "支持格式: 1, 3, 2-4, all", "1"), // 改为 "1" 作为默认值
                    options.len(),
                );
                indices
                    .iter()
                    .map(|&i| available_formats[i].clone())
                    .collect()
            };

            if selected_formats.is_empty() {
                println!("{} 未选择任何音频格式，跳过音频下载。", "[i]".cyan());
                return Ok(vec![]);
            }

            let mut results = vec![];
            let audio_path = textbook_basename
                .map(|b| base_path.join(format!("{} - [audio]", b)))
                .unwrap_or(base_path);

            for (i, item) in audio_items.iter().enumerate() {
                let title = item
                    .get("global_title")
                    .and_then(|t| t.get("zh-CN"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知音频");
                let base_name = format!("{:03} {}", i + 1, utils::sanitize_filename(title));

                for format in &selected_formats {
                    if let Some(ti) = item
                        .get("ti_items")
                        .and_then(|v| v.as_array())
                        .and_then(|items| {
                            items.iter().find(|ti| {
                                ti.get("ti_format").and_then(|f| f.as_str()) == Some(format.as_str())
                            })
                        })
                    {
                        if let Some(url) = ti
                            .get("ti_storages")
                            .and_then(|v| v.as_array())
                            .and_then(|a| a.get(0))
                            .and_then(|v| v.as_str())
                        {
                            results.push(
                                serde_json::from_value::<FileInfo>(json!({
                                    "filepath": audio_path.join(format!("{}.{}", base_name, format)),
                                    "url": url,
                                    "ti_md5": ti.get("ti_md5"),
                                    "ti_size": ti.get("ti_size"),
                                    "date": item.get("update_time")
                                }))
                                .unwrap(),
                            );
                        }
                    }
                }
            }
            Ok(results)
        } else {
            Ok(vec![])
        }
    }

    fn get_available_audio_formats(&self, data: &[Value]) -> Vec<String> {
        let mut formats = HashSet::new();
        for item in data {
            if let Some(ti_items) = item.get("ti_items").and_then(|v| v.as_array()) {
                for ti in ti_items {
                    if let Some(format) = ti.get("ti_format").and_then(|v| v.as_str()) {
                        formats.insert(format.to_string());
                    }
                }
            }
        }
        let mut sorted_formats: Vec<String> = formats.into_iter().collect();
        sorted_formats.sort();
        sorted_formats
    }

    pub(super) fn build_resource_path(&self, tag_list_val: Option<&Value>) -> PathBuf {
        let template: HashMap<&str, &str> = [
            ("zxxxd", "未知学段"),
            ("zxxnj", "未知年级"),
            ("zxxxk", "未知学科"),
            ("zxxbb", "未知版本"),
            ("zxxcc", "未知册"),
        ]
        .iter()
        .cloned()
        .collect();
        let mut path_map = template.clone();

        if let Some(tags) = tag_list_val.and_then(|v| v.as_array()) {
            for tag in tags {
                if let (Some(dim_id), Some(tag_name)) = (
                    tag.get("tag_dimension_id").and_then(|v| v.as_str()),
                    tag.get("tag_name").and_then(|v| v.as_str()),
                ) {
                    if path_map.contains_key(dim_id) {
                        path_map.insert(dim_id, tag_name);
                    }
                }
            }
        }

        let default_values: HashSet<&str> = template.values().cloned().collect();
        let components: Vec<String> = ["zxxxd", "zxxnj", "zxxxk", "zxxbb", "zxxcc"]
            .iter()
            .filter_map(|&key| path_map.get(key))
            .filter(|&&val| !default_values.contains(val))
            .map(|&name| utils::sanitize_filename(name))
            .collect();

        if components.is_empty() {
            PathBuf::from(constants::UNCLASSIFIED_DIR)
        } else {
            components.iter().collect()
        }
    }
}

#[async_trait]
impl ResourceExtractor for TextbookExtractor {
    async fn extract_file_info(
        &self,
        resource_id: &str,
        context: &DownloadJobContext,
    ) -> AppResult<Vec<FileInfo>> {
        let url_template = self.config.url_templates.get("TEXTBOOK_DETAILS").unwrap();
        let data = self
            .http_client
            .fetch_json(url_template, &[("resource_id", resource_id)])
            .await?;

        let (mut pdf_files, textbook_basename) = self.extract_pdf_info(&data);
        let base_path = self.build_resource_path(data.get("tag_list"));

        let audio_files = self
            .extract_audio_info(resource_id, base_path, textbook_basename, context)
            .await?;

        pdf_files.extend(audio_files);
        Ok(pdf_files)
    }
}