// src/extractor/utils.rs

use crate::{
    constants,
    models::{FileInfo, ResourceCategory, api::CourseResource},
};
use itertools::Itertools;
use log::debug;
use regex::Regex;
use std::{
    path::Path,
    sync::LazyLock,
};

static RES_REF_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([\d,\s\*]+)\]$").unwrap());

/// 通用函数：解析资源引用字符串，如 "[0]", "[1,2]", "[*]"
pub fn parse_res_ref_indices(ref_str: &str, total_resources: usize) -> Option<Vec<usize>> {
    RES_REF_RE.captures(ref_str).and_then(|caps| {
        caps.get(1).map(|m| {
            let content = m.as_str().trim();
            if content == "*" {
                (0..total_resources).collect()
            } else {
                content
                    .split(',')
                    .filter_map(|s| s.trim().parse::<usize>().ok())
                    .collect()
            }
        })
    })
}

/// 通用函数：从一个视频资源中提取所有可下载的 m3u8 流
pub fn extract_video_files(
    resource: &CourseResource,
    base_name: &str,
    base_path: &Path,
    teacher_name: &str,
) -> Vec<FileInfo> {
    let mut streams: Vec<FileInfo> = resource
        .ti_items
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|item| item.ti_format == "m3u8")
        .filter_map(|item| {
            item.ti_storages
                .as_ref()
                .and_then(|s| s.first())
                .map(|url| {
                    // 更智能的清晰度提取
                    let quality_str = item
                        .custom_properties
                        .as_ref()
                        .and_then(|p| p.requirements.as_ref())
                        .and_then(|reqs| {
                            reqs.iter().find(|r| r.name == constants::api::video_metadata_keys::HEIGHT)
                        })
                        .map(|h| h.value.as_str())
                        .unwrap_or("未知"); // 找不到则默认为 "未知"，避免歧义

                    // 统一文件名格式为 "[清晰度]"，与后续解析逻辑保持一致
                    let filename =
                        format!("{} [{}] - [{}].ts", base_name, quality_str, teacher_name);

                    let estimated_size = item
                        .custom_properties
                        .as_ref()
                        .and_then(|p| p.requirements.as_ref())
                        .and_then(|reqs| {
                            reqs.iter().find(|r| r.name == constants::api::video_metadata_keys::TOTAL_SIZE)
                        })
                        .and_then(|s| s.value.parse::<u64>().ok());

                    debug!(
                        "M3U8 提取: 文件名='{}', 从JSON提取的预估大小 (total_size): {:?}",
                        filename, estimated_size
                    );

                    FileInfo {
                        filepath: base_path.join(filename),
                        url: url.clone(),
                        ti_md5: item.ti_md5.clone(),
                        ti_size: estimated_size,
                        date: Some(resource.update_time),
                        category: ResourceCategory::Video,
                    }
                })
        })
        .collect();

    // 去重逻辑
    streams.sort_by_key(|s| {
        // 解析不带 'p' 的数字进行排序
        s.filepath
            .to_string_lossy()
            .split('[')
            .nth(1)
            .and_then(|part| part.split(']').next())
            .and_then(|quality| quality.trim().parse::<u32>().ok())
            .unwrap_or(0)
    });
    streams.reverse(); // 高分辨率在前

    streams.into_iter().unique_by(|s| s.url.clone()).collect()
}

/// 通用函数：从一个文档/课件资源中提取唯一的 PDF 文件
pub fn extract_document_file(resource: &CourseResource) -> Option<FileInfo> {
    resource
        .ti_items
        .as_deref()
        .unwrap_or_default()
        .iter()
        // .find(|i| i.ti_file_flag.as_deref() == Some("pdf"))
        .find(|i| i.ti_format.eq_ignore_ascii_case("pdf"))
        .and_then(|pdf_item| {
            pdf_item
                .ti_storages
                .as_ref()
                .and_then(|s| s.first())
                .map(|url| FileInfo {
                    filepath: std::path::PathBuf::new(),
                    url: url.clone(),
                    ti_md5: pdf_item.ti_md5.clone(),
                    ti_size: pdf_item.ti_size,
                    date: Some(resource.update_time),
                    category: ResourceCategory::Document,
                })
        })
}
