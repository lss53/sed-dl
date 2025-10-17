// src/extractor/textbook.rs

use super::ResourceExtractor;
use crate::{
    DownloadJobContext,
    client::RobustClient,
    config::AppConfig,
    constants,
    error::*,
    models::{
        FileInfo, ResourceCategory,
        api::{AudioRelationItem, Tag, TextbookDetailsResponse},
    },
    utils,
};
use async_trait::async_trait;
use itertools::Itertools;
use log::{debug, info};
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

    fn extract_pdf_info(
        &self,
        data: &TextbookDetailsResponse,
        base_path: &Path,
    ) -> (Vec<FileInfo>, Option<String>) {
        let results: Vec<FileInfo> = data
            .ti_items
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|item| {
                if !item.ti_format.eq_ignore_ascii_case(constants::api::resource_formats::PDF) {
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
                    filepath: base_path.join(&name),
                    url: url_str.clone(),
                    ti_md5: item.ti_md5.clone(),
                    ti_size: item.ti_size,
                    date: Some(data.update_time),
                    category: ResourceCategory::Document,
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
    ) -> AppResult<Vec<FileInfo>> {
        let url_template = self
            .config
            .url_templates
            .get("TEXTBOOK_AUDIO")
            .expect("TEXTBOOK_AUDIO URL template not found");
        let audio_items: Vec<AudioRelationItem> = self
            .http_client
            .fetch_json(url_template, &[("resource_id", resource_id)])
            .await?;
        if audio_items.is_empty() {
            info!("未找到与教材 '{}' 关联的音频文件。", resource_id);
            return Ok(vec![]);
        }
        let audio_path = textbook_basename
            .map(|b| base_path.join(format!("{} - [audio]", b)))
            .unwrap_or(base_path);
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

                if let Some(ti_items) = &item.ti_items {
                    let grouped_by_format = ti_items.iter().into_group_map_by(|ti| &ti.ti_format);
                    grouped_by_format
                        .into_iter()
                        .filter_map(|(format, group)| {
                            let downloadable_group: Vec<_> = group
                                .into_iter()
                                .filter(|ti| ti.ti_file_flag.as_deref() != Some("source"))
                                .collect();
                            let best_ti = downloadable_group
                                .iter()
                                .find(|ti| {
                                    ti.ti_file_flag
                                        .as_deref()
                                        .is_some_and(|f| !f.contains("clip"))
                                })
                                .or_else(|| downloadable_group.first())
                                .copied()?;
                            let url = best_ti.ti_storages.as_ref()?.first()?;
                            Some(FileInfo {
                                filepath: audio_path_clone
                                    .join(format!("{}.{}", base_name, format)),
                                url: url.clone(),
                                ti_md5: best_ti.ti_md5.clone(),
                                ti_size: best_ti.ti_size,
                                date: Some(item.update_time),
                                category: ResourceCategory::Audio,
                            })
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![]
                }
            })
            .collect();
        Ok(results)
    }

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
                if path_map.contains_key(tag.tag_dimension_id.as_str()) {
                    path_map.insert(&tag.tag_dimension_id, &tag.tag_name);
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
            .expect("TEXTBOOK_DETAILS URL template not found");
        let data: TextbookDetailsResponse = self
            .http_client
            .fetch_json(url_template, &[("resource_id", resource_id)])
            .await?;
        let base_path = self.build_resource_path(data.tag_list.as_deref(), context);
        let (mut pdf_files, textbook_basename) = self.extract_pdf_info(&data, &base_path);
        let audio_files = self
            .extract_audio_info(resource_id, base_path, textbook_basename)
            .await?;
        pdf_files.extend(audio_files);
        info!("为教材 '{}' 提取到 {} 个文件", resource_id, pdf_files.len());
        debug!("Extractor 返回的原始文件列表 (共 {} 项):", pdf_files.len());
        for (i, item) in pdf_files.iter().enumerate() {
            debug!("  [{:03}] Path: {:?}, URL: {}", i, item.filepath, item.url);
        }
        Ok(pdf_files)
    }
}
