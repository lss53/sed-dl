// src/downloader/negotiator.rs

use crate::{
    DownloadJobContext,
    error::AppResult,
    models::{FileInfo, ResourceCategory},
    symbols, ui,
};
use colored::Colorize;
use itertools::Itertools;
use log::{debug, info, warn};
use regex::Regex;
use std::{
    collections::{BTreeSet, HashMap},
    sync::LazyLock,
};

static VIDEO_QUALITY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" \[(\d{3,4})\]").unwrap());

pub struct ItemNegotiator<'a> {
    context: &'a DownloadJobContext,
}

impl<'a> ItemNegotiator<'a> {
    pub fn new(context: &'a DownloadJobContext) -> Self {
        Self { context }
    }

    /// 按视频质量对 FileInfo 列表进行降序排序
    fn sort_videos_by_quality_desc(&self, streams: &mut [FileInfo]) {
        streams.sort_by_key(|f| {
            VIDEO_QUALITY_RE
                .captures(&f.filepath.to_string_lossy())
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<u32>().ok())
                .unwrap_or(0)
        });
        streams.reverse();
    }

    pub fn pre_filter_items(&self, items: Vec<FileInfo>) -> AppResult<Vec<FileInfo>> {
        let items = self.filter_videos_non_interactive(items)?;
        let items = self.filter_audio_non_interactive(items)?;
        Ok(items)
    }

    pub async fn negotiate_video_interactive(
        &self,
        video_items: Vec<FileInfo>,
    ) -> AppResult<Vec<FileInfo>> {
        if video_items.is_empty() {
            return Ok(vec![]);
        }

        let video_groups: Vec<Vec<FileInfo>> = video_items
            .into_iter()
            .sorted_by_key(|f| {
                VIDEO_QUALITY_RE
                    .replace(&f.filepath.to_string_lossy(), "")
                    .trim()
                    .to_string()
            })
            .chunk_by(|f| {
                VIDEO_QUALITY_RE
                    .replace(&f.filepath.to_string_lossy(), "")
                    .trim()
                    .to_string()
            })
            .into_iter()
            .map(|(_, group)| group.collect())
            .collect();

        let mut sorted_qualities: Vec<_> = video_groups
            .iter()
            .flatten()
            .filter_map(|f| {
                VIDEO_QUALITY_RE
                    .captures(&f.filepath.to_string_lossy())
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().to_string())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        // 直接对清晰度字符串列表进行排序。
        // `sort_by_key` 会将字符串解析为数字进行比较，实现数值排序。
        // `reverse()` 将排序结果反转，实现从高到低的降序排列。
        sorted_qualities.sort_by_key(|q_str| q_str.parse::<u32>().unwrap_or(0));
        sorted_qualities.reverse();

        if sorted_qualities.len() <= 1 {
            return Ok(video_groups.into_iter().flatten().collect());
        }

        // 直接按回车即可选择列表中的第一个（也是最好的）选项。
        let default_choice = "1";
        let user_choices = ui::get_user_choices_from_menu(
            &sorted_qualities,
            "检测到多种视频清晰度，请选择",
            default_choice,
        );
        debug!("用户已做出选择: {:?}", user_choices);

        let mut selected_videos = Vec::new();
        for mut group in video_groups {
            self.sort_videos_by_quality_desc(&mut group); // <-- 使用辅助函数

            for choice in &user_choices {
                if let Some(file) = self.select_stream_with_fallback(&group, choice) {
                    if !selected_videos.iter().any(|v: &FileInfo| v.url == file.url) {
                        selected_videos.push(file.clone());
                    }
                }
            }
        }
        Ok(selected_videos)
    }

    fn filter_videos_non_interactive(&self, items: Vec<FileInfo>) -> AppResult<Vec<FileInfo>> {
        let (video_items, mut final_items): (Vec<FileInfo>, Vec<FileInfo>) = items
            .into_iter()
            .partition(|f| f.category == ResourceCategory::Video);
        if video_items.is_empty() {
            return Ok(final_items);
        }

        let selected_quality = &self.context.args.video_quality;
        info!("根据参数选择视频清晰度: {}", selected_quality);

        let quality_is_valid = ["best", "worst"]
            .contains(&selected_quality.to_lowercase().as_str())
            || selected_quality.parse::<u32>().is_ok();
        if !quality_is_valid {
            let msg = format!(
                "无效的视频质量参数: '{}'。请输入纯数字（如 720）或 'best'/'worst'。 将不会下载任何视频。",
                selected_quality
            );
            warn!("{}", msg);
            eprintln!("{} {} {}", *symbols::WARN, "警告:".yellow(), msg);
            return Ok(final_items);
        }

        let original_video_count = video_items.len();
        let selected_videos = video_items
            .into_iter()
            .sorted_by_key(|f| {
                VIDEO_QUALITY_RE
                    .replace(&f.filepath.to_string_lossy(), "")
                    .trim()
                    .to_string()
            })
            .chunk_by(|f| {
                VIDEO_QUALITY_RE
                    .replace(&f.filepath.to_string_lossy(), "")
                    .trim()
                    .to_string()
            })
            .into_iter()
            .filter_map(|(_, group)| {
                let mut streams: Vec<FileInfo> = group.collect();
                self.sort_videos_by_quality_desc(&mut streams); // <-- 使用辅助函数
                self.select_stream_with_fallback(&streams, selected_quality)
                    .cloned()
            })
            .collect::<Vec<FileInfo>>();

        if original_video_count > 0
            && selected_videos.is_empty()
            && !["best", "worst"].contains(&selected_quality.to_lowercase().as_str())
        {
            let msg = format!("在视频列表中未找到您指定的清晰度 '{}'。", selected_quality);
            warn!("{}", msg);
            eprintln!("{} {} {}", *symbols::INFO, "提示:".cyan(), msg);
        }

        final_items.extend(selected_videos);
        Ok(final_items)
    }

    fn select_stream_with_fallback<'b>(
        &self,
        streams: &'b [FileInfo],
        quality: &str,
    ) -> Option<&'b FileInfo> {
        if streams.is_empty() {
            return None;
        }
        match quality.to_lowercase().as_str() {
            "best" => streams.first(),
            "worst" => streams.last(),
            q => q.parse::<u32>().ok().and_then(|target_num| {
                streams.iter().find(|f| {
                    VIDEO_QUALITY_RE
                        .captures(&f.filepath.to_string_lossy())
                        .and_then(|caps| caps.get(1))
                        .and_then(|m| m.as_str().parse::<u32>().ok())
                        .map_or(false, |stream_num| stream_num == target_num)
                })
            }),
        }
    }

    pub fn negotiate_audio_interactive(&self, items: Vec<FileInfo>) -> AppResult<Vec<FileInfo>> {
        if items.is_empty() {
            return Ok(vec![]);
        }
        let (audio_items, mut final_items): (Vec<FileInfo>, Vec<FileInfo>) = items
            .into_iter()
            .partition(|f| f.category == ResourceCategory::Audio);
        if audio_items.is_empty() {
            return Ok(final_items);
        }

        let audio_groups: HashMap<String, Vec<FileInfo>> = audio_items
            .into_iter()
            .map(|f| {
                (
                    f.filepath.with_extension("").to_string_lossy().to_string(),
                    f,
                )
            })
            .into_group_map();
        let sorted_formats: Vec<_> = audio_groups
            .values()
            .flatten()
            .filter_map(|f| {
                f.filepath
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_uppercase())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        if sorted_formats.len() <= 1 {
            final_items.extend(audio_groups.into_values().flatten());
            return Ok(final_items);
        }

        
        let user_choices =
            ui::get_user_choices_from_menu(&sorted_formats, "检测到多种音频格式，请选择", "1");
        let lower_choices: Vec<_> = user_choices.iter().map(|s| s.to_lowercase()).collect();

        for (_, group) in audio_groups {
            for file in group {
                if let Some(ext) = file.filepath.extension().and_then(|e| e.to_str()) {
                    if lower_choices.contains(&ext.to_lowercase()) {
                        final_items.push(file);
                    }
                }
            }
        }
        Ok(final_items)
    }

    fn filter_audio_non_interactive(&self, items: Vec<FileInfo>) -> AppResult<Vec<FileInfo>> {
        let (audio_items, mut final_items): (Vec<FileInfo>, Vec<FileInfo>) = items
            .into_iter()
            .partition(|f| f.category == ResourceCategory::Audio);
        if audio_items.is_empty() {
            return Ok(final_items);
        }

        let selected_format = self.context.args.audio_format.to_lowercase();
        info!("根据参数选择音频格式: {}", selected_format);

        // --- 精简：使用 extend 和 filter ---
        final_items.extend(audio_items.into_iter().filter(|f| {
            f.filepath
                .extension()
                .and_then(|e| e.to_str())
                .map_or(false, |ext| ext.to_lowercase() == selected_format)
        }));

        Ok(final_items)
    }
}

// 测试模块
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cli::Cli, downloader::DownloadManager, DownloadJobContext};
    use clap::Parser;
    use std::{
        path::PathBuf,
        sync::{atomic::AtomicBool, Arc},
    };
    use tokio::sync::Mutex as TokioMutex;

    // --- 辅助函数：创建一个用于测试的上下文 ---
    fn create_test_context(args_str: &'static str) -> DownloadJobContext {
        let args = Arc::new(Cli::parse_from(args_str.split_whitespace()));
        let config = Arc::new(crate::config::AppConfig::default());

        DownloadJobContext {
            manager: DownloadManager::new(),
            token: Arc::new(TokioMutex::new("fake-token".to_string())),
            config,
            http_client: Arc::new(
                crate::client::RobustClient::new(Arc::new(crate::config::AppConfig::default()))
                    .unwrap(),
            ),
            args,
            non_interactive: true,
            cancellation_token: Arc::new(AtomicBool::new(false)),
        }
    }

    // --- 辅助函数：创建一些模拟的视频文件信息 ---
    fn create_sample_videos() -> Vec<FileInfo> {
        vec![
            FileInfo {
                filepath: PathBuf::from("video_a [1080].ts"),
                url: "url_1080".to_string(),
                category: ResourceCategory::Video,
                ..Default::default()
            },
            FileInfo {
                filepath: PathBuf::from("video_a [720].ts"),
                url: "url_720".to_string(),
                category: ResourceCategory::Video,
                ..Default::default()
            },
            FileInfo {
                filepath: PathBuf::from("video_a [480].ts"),
                url: "url_480".to_string(),
                category: ResourceCategory::Video,
                ..Default::default()
            },
            // 另一个视频，测试分组
            FileInfo {
                filepath: PathBuf::from("video_b [720].ts"),
                url: "url_b_720".to_string(),
                category: ResourceCategory::Video,
                ..Default::default()
            },
        ]
    }

    // --- 视频过滤测试 ---

    #[test]
    fn test_filter_videos_best() {
        let context = create_test_context("sed-dl --url a --video-quality best");
        let negotiator = ItemNegotiator::new(&context);
        let videos = create_sample_videos();
        let result = negotiator.filter_videos_non_interactive(videos).unwrap();

        assert_eq!(result.len(), 2);
        // 验证为 video_a 选择了 1080p
        assert!(result.iter().any(|f| f.url == "url_1080"));
        // 验证为 video_b 选择了 720p
        assert!(result.iter().any(|f| f.url == "url_b_720"));
    }

    #[test]
    fn test_filter_videos_worst() {
        let context = create_test_context("sed-dl --url a --video-quality worst");
        let negotiator = ItemNegotiator::new(&context);
        let videos = create_sample_videos();
        let result = negotiator.filter_videos_non_interactive(videos).unwrap();

        assert_eq!(result.len(), 2);
        // 验证为 video_a 选择了 480p
        assert!(result.iter().any(|f| f.url == "url_480"));
        // 验证为 video_b 选择了 720p
        assert!(result.iter().any(|f| f.url == "url_b_720"));
    }

    #[test]
    fn test_filter_videos_specific_quality() {
        let context = create_test_context("sed-dl --url a --video-quality 720");
        let negotiator = ItemNegotiator::new(&context);
        let videos = create_sample_videos();
        let result = negotiator.filter_videos_non_interactive(videos).unwrap();

        assert_eq!(result.len(), 2);
        // 验证两个视频都选择了 720p
        assert!(result.iter().any(|f| f.url == "url_720"));
        assert!(result.iter().any(|f| f.url == "url_b_720"));
    }

    #[test]
    fn test_filter_videos_non_existent_quality() {
        let context = create_test_context("sed-dl --url a --video-quality 9999");
        let negotiator = ItemNegotiator::new(&context);
        let videos = create_sample_videos();
        let result = negotiator.filter_videos_non_interactive(videos).unwrap();

        // 当指定的清晰度不存在时，不应该选择任何视频
        assert!(result.is_empty());
    }

    // --- 辅助函数：创建一些模拟的音频文件信息 ---
    fn create_sample_audios() -> Vec<FileInfo> {
        vec![
            FileInfo {
                filepath: PathBuf::from("track1.mp3"),
                category: ResourceCategory::Audio,
                ..Default::default()
            },
            FileInfo {
                filepath: PathBuf::from("track1.m4a"),
                category: ResourceCategory::Audio,
                ..Default::default()
            },
            FileInfo {
                filepath: PathBuf::from("document.pdf"), // 非音频文件
                ..Default::default()
            },
        ]
    }

    // --- 音频过滤测试 ---

    #[test]
    fn test_filter_audio_non_interactive() {
        let context = create_test_context("sed-dl --url a --audio-format mp3");
        let negotiator = ItemNegotiator::new(&context);
        let items = create_sample_audios();
        let result = negotiator.filter_audio_non_interactive(items).unwrap();

        // 应该只剩下 mp3 和 pdf (因为 pdf 不是音频，不会被过滤)
        assert_eq!(result.len(), 2);
        assert!(result
            .iter()
            .any(|f| f.filepath.to_string_lossy() == "track1.mp3"));
        assert!(!result
            .iter()
            .any(|f| f.filepath.to_string_lossy() == "track1.m4a"));
        assert!(result
            .iter()
            .any(|f| f.filepath.to_string_lossy() == "document.pdf"));
    }
}