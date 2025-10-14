// src/extractor/textbook.rs

use super::ResourceExtractor;
use crate::{
    client::RobustClient,
    config::AppConfig,
    constants,
    error::*,
    models::{
        api::{AudioRelationItem, Tag, TextbookDetailsResponse},
        FileInfo,
    },
    symbols, ui, utils, DownloadJobContext,
};
use async_trait::async_trait;
use log::{debug, info, warn};
use percent_encoding;
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};
use url::Url;

static TEMPLATE_TAGS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    [
        ("zxxxd", "未知学段"),
        ("zxxnj", "未知年级"),
        ("zxxxk", "未知学科"),
        ("zxxbb", "未知版本"),
        ("zxxcc", "未知册"),
    ]
    .iter()
    .cloned()
    .collect()
});

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

    // MODIFIED: Takes a pre-calculated base_path
    fn extract_pdf_info(
        &self,
        data: &TextbookDetailsResponse,
        base_path: &Path, // <-- Receives base path
    ) -> (Vec<FileInfo>, Option<String>) {
        let results: Vec<FileInfo> = data
            .ti_items
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|item| {
                if item.ti_file_flag.as_deref() != Some("source")
                    || item.ti_format != constants::api::resource_formats::PDF
                {
                    return None;
                }

                let url_str = item.ti_storages.as_ref()?.first()?;
                let url = Url::parse(url_str).ok()?;

                let raw_filename = Path::new(url.path()).file_name()?.to_str()?;
                let decoded_filename = percent_encoding::percent_decode(raw_filename.as_bytes())
                    .decode_utf8_lossy()
                    .to_string();

                let name = if self.is_generic_filename(&decoded_filename) {
                    let title = data
                        .global_title
                        .as_ref()
                        .map(|t| t.zh_cn.as_str())
                        .or(data.title.as_deref())
                        .unwrap_or(&data.id);
                    format!("{}.pdf", utils::sanitize_filename(title))
                } else {
                    utils::sanitize_filename(&decoded_filename)
                };

                debug!("提取到PDF文件: '{}' @ '{}'", name, url_str);
                Some(FileInfo {
                    filepath: base_path.join(&name), // <-- Uses the provided base_path
                    url: url_str.clone(),
                    ti_md5: item.ti_md5.clone(),
                    ti_size: item.ti_size,
                    date: data.update_time,
                })
            })
            .collect();

        let textbook_basename = results
            .first()
            .and_then(|fi| Path::new(&fi.filepath).file_stem())
            .map(|s| s.to_string_lossy().to_string());

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
        let url_template = self
            .config
            .url_templates
            .get("TEXTBOOK_AUDIO")
            .expect("配置文件中缺少必需的 'TEXTBOOK_AUDIO' URL 模板");
        let audio_items: Vec<AudioRelationItem> = self
            .http_client
            .fetch_json(url_template, &[("resource_id", resource_id)])
            .await?;

        if audio_items.is_empty() {
            info!("未找到与教材 '{}' 关联的音频文件。", resource_id);
            return Ok(vec![]);
        }

        let available_formats = self.get_available_audio_formats(&audio_items);
        if available_formats.is_empty() {
            return Ok(vec![]);
        }
        debug!("可用音频格式: {:?}", available_formats);

        let selected_formats: Vec<String> = if context.non_interactive
            || context.args.audio_format != constants::DEFAULT_AUDIO_FORMAT
        {
            let preferred = context.config.default_audio_format.to_lowercase();
            let chosen_format = if available_formats.contains(&preferred) {
                preferred
            } else {
                println!(
                    "{} 首选音频格式 '{}' 不可用，将自动选择 '{}'。",
                    *symbols::WARN,
                    preferred,
                    available_formats[0]
                );
                warn!(
                    "首选音频格式 '{}' 不可用, 将选择第一个可用格式 '{}'",
                    preferred, available_formats[0]
                );
                available_formats[0].clone()
            };
            vec![chosen_format]
        } else {
            let options: Vec<String> = available_formats.iter().map(|f| f.to_uppercase()).collect();
            ui::get_user_choices_from_menu(&options, "选择音频格式", "1")
        };
        info!("已选择音频格式: {:?}", selected_formats);

        if selected_formats.is_empty() {
            println!("{} 未选择任何音频格式，跳过音频下载。", *symbols::INFO);
            return Ok(vec![]);
        }

        let audio_path = textbook_basename
            .map(|b| base_path.join(format!("{} - [audio]", b)))
            .unwrap_or(base_path);
        debug!("音频文件将保存至: {:?}", audio_path);

        let total_items = audio_items.len();
        let width = if total_items == 0 {
            1
        } else {
            (total_items as f64).log10() as usize + 1
        };

        let results = audio_items
            .iter()
            .enumerate()
            .flat_map(|(i, item)| {
                let title = &item.global_title.zh_cn;
                let index_prefix = format!("{:0width$}", i + 1, width = width);
                let base_name = format!("[{}] {}", index_prefix, utils::sanitize_filename(title));

                let audio_path_clone = audio_path.clone();

                selected_formats.iter().filter_map(move |format| {
                    let format_lower = format.to_lowercase();
                    let ti = item
                        .ti_items
                        .as_ref()?
                        .iter()
                        .find(|ti| {
                            ti.ti_format == format_lower
                                && ti.ti_file_flag.as_deref() == Some("href")
                        })
                        .or_else(|| {
                            item.ti_items.as_ref()?.iter().find(|ti| {
                                ti.ti_format == format_lower
                                    && ti.ti_storages.as_ref().is_some_and(|s| !s.is_empty())
                            })
                        })?;

                    let url = ti.ti_storages.as_ref()?.first()?;

                    debug!(
                        "提取到音频文件: '{}.{}' @ '{}'",
                        base_name, &format_lower, url
                    );
                    Some(FileInfo {
                        filepath: audio_path_clone.join(format!("{}.{}", base_name, &format_lower)),
                        url: url.clone(),
                        ti_md5: ti.ti_md5.clone(),
                        ti_size: ti.ti_size,
                        date: item.update_time,
                    })
                })
            })
            .collect();

        Ok(results)
    }

    fn get_available_audio_formats(&self, data: &[AudioRelationItem]) -> Vec<String> {
        let mut formats = HashSet::new();
        for item in data {
            if let Some(ti_items) = item.ti_items.as_ref() {
                for ti in ti_items {
                    formats.insert(ti.ti_format.clone());
                }
            }
        }
        let mut sorted_formats: Vec<String> = formats.into_iter().collect();
        sorted_formats.sort();
        sorted_formats
    }

    // MODIFIED: Takes context and checks for --flat flag
    pub(super) fn build_resource_path(
        &self,
        tag_list_val: Option<&[Tag]>,
        context: &DownloadJobContext,
    ) -> PathBuf {
        if context.args.flat {
            return PathBuf::new();
        }

        let mut path_map = TEMPLATE_TAGS.clone();

        if let Some(tags) = tag_list_val {
            for tag in tags {
                let dim_id = &tag.tag_dimension_id;
                let tag_name = &tag.tag_name;
                if path_map.contains_key(dim_id.as_str()) {
                    path_map.insert(dim_id, tag_name);
                }
            }
        }

        let default_values: HashSet<&str> = TEMPLATE_TAGS.values().cloned().collect();
        let components: Vec<String> = ["zxxxd", "zxxnj", "zxxxk", "zxxbb", "zxxcc"]
            .iter()
            .filter_map(|&key| path_map.get(key))
            .filter(|&&val| !default_values.contains(val))
            .map(|&name| utils::sanitize_filename(name))
            .collect();

        if components.is_empty() {
            debug!("无法从标签构建分类路径，使用默认未分类目录");
            PathBuf::from(constants::UNCLASSIFIED_DIR)
        } else {
            let path: PathBuf = components.iter().collect();
            debug!("从标签构建的分类路径: {:?}", path);
            path
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
        info!("开始提取教材资源, ID: {}", resource_id);
        let url_template = self
            .config
            .url_templates
            .get("TEXTBOOK_DETAILS")
            .expect("配置文件中缺少必需的 'TEXTBOOK_DETAILS' URL 模板");
        let data: TextbookDetailsResponse = self
            .http_client
            .fetch_json(url_template, &[("resource_id", resource_id)])
            .await?;

        // MODIFIED: Calculate base_path once and pass it down
        let base_path = self.build_resource_path(data.tag_list.as_deref(), context);

        let (mut pdf_files, textbook_basename) = self.extract_pdf_info(&data, &base_path);

        let audio_files = self
            .extract_audio_info(resource_id, base_path, textbook_basename, context)
            .await?;
        pdf_files.extend(audio_files);

        info!("为教材 '{}' 提取到 {} 个文件", resource_id, pdf_files.len());
        Ok(pdf_files)
    }
}