// src/downloader/negotiator.rs

use crate::{
    error::AppResult,
    models::{FileInfo, ResourceCategory},
    symbols, ui, DownloadJobContext,
};
use colored::Colorize;
use itertools::Itertools;
use log::{debug, info, warn};
use regex::Regex;
use std::{
    collections::{BTreeSet, HashMap},
    sync::LazyLock,
};

static VIDEO_QUALITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r" \[(\d{3,4})\]").unwrap());

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
            VIDEO_QUALITY_RE.captures(&f.filepath.to_string_lossy())
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
        if video_items.is_empty() { return Ok(vec![]); }

        let video_groups: Vec<Vec<FileInfo>> = video_items
            .into_iter()
            .sorted_by_key(|f| VIDEO_QUALITY_RE.replace(&f.filepath.to_string_lossy(), "").trim().to_string())
            .chunk_by(|f| VIDEO_QUALITY_RE.replace(&f.filepath.to_string_lossy(), "").trim().to_string())
            .into_iter()
            .map(|(_, group)| group.collect())
            .collect();
        
        let mut sorted_qualities: Vec<_> = video_groups.iter().flatten()
            .filter_map(|f| VIDEO_QUALITY_RE.captures(&f.filepath.to_string_lossy()).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()))
            .collect::<BTreeSet<_>>().into_iter().collect();

        self.sort_videos_by_quality_desc(&mut sorted_qualities.iter_mut().map(|s| FileInfo { filepath: std::path::PathBuf::from(format!("[{}]", s)), ..Default::default() }).collect::<Vec<_>>());
        sorted_qualities.sort_by_key(|q| q.parse::<u32>().unwrap_or(0));
        sorted_qualities.reverse();

        if sorted_qualities.len() <= 1 {
            return Ok(video_groups.into_iter().flatten().collect());
        }

        let default_choice = sorted_qualities.first().map(|s| s.as_str()).unwrap_or("all");
        let user_choices = ui::get_user_choices_from_menu(&sorted_qualities, "检测到多种视频清晰度，请选择", default_choice);
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
        let (video_items, mut final_items): (Vec<FileInfo>, Vec<FileInfo>) = items.into_iter().partition(|f| f.category == ResourceCategory::Video);
        if video_items.is_empty() { return Ok(final_items); }

        let selected_quality = &self.context.args.video_quality;
        info!("根据参数选择视频清晰度: {}", selected_quality);
        
        let quality_is_valid = ["best", "worst"].contains(&selected_quality.to_lowercase().as_str()) || selected_quality.parse::<u32>().is_ok();
        if !quality_is_valid {
            let msg = format!("无效的视频质量参数: '{}'。请输入纯数字（如 720）或 'best'/'worst'。 将不会下载任何视频。", selected_quality);
            warn!("{}", msg);
            eprintln!("{} {} {}", *symbols::WARN, "警告:".yellow(), msg);
            return Ok(final_items); 
        }

        let original_video_count = video_items.len();
        let selected_videos = video_items.into_iter()
            .sorted_by_key(|f| VIDEO_QUALITY_RE.replace(&f.filepath.to_string_lossy(), "").trim().to_string())
            .chunk_by(|f| VIDEO_QUALITY_RE.replace(&f.filepath.to_string_lossy(), "").trim().to_string())
            .into_iter()
            .filter_map(|(_, group)| {
                let mut streams: Vec<FileInfo> = group.collect();
                self.sort_videos_by_quality_desc(&mut streams); // <-- 使用辅助函数
                self.select_stream_with_fallback(&streams, selected_quality).cloned()
            })
            .collect::<Vec<FileInfo>>();

        if original_video_count > 0 && selected_videos.is_empty() && !["best", "worst"].contains(&selected_quality.to_lowercase().as_str()) {
            let msg = format!("在视频列表中未找到您指定的清晰度 '{}'。", selected_quality);
            warn!("{}", msg);
            eprintln!("{} {} {}", *symbols::INFO, "提示:".cyan(), msg);
        }

        final_items.extend(selected_videos);
        Ok(final_items)
    }

    fn select_stream_with_fallback<'b>(&self, streams: &'b [FileInfo], quality: &str) -> Option<&'b FileInfo> {
        if streams.is_empty() { return None; }
        match quality.to_lowercase().as_str() {
            "best" => streams.first(),
            "worst" => streams.last(),
            q => q.parse::<u32>().ok().and_then(|target_num| {
                streams.iter().find(|f| {
                    VIDEO_QUALITY_RE.captures(&f.filepath.to_string_lossy())
                        .and_then(|caps| caps.get(1))
                        .and_then(|m| m.as_str().parse::<u32>().ok())
                        .map_or(false, |stream_num| stream_num == target_num)
                })
            }),
        }
    }
    
    pub fn negotiate_audio_interactive(&self, items: Vec<FileInfo>) -> AppResult<Vec<FileInfo>> {
        if items.is_empty() { return Ok(vec![]); }
        let (audio_items, mut final_items): (Vec<FileInfo>, Vec<FileInfo>) = items.into_iter().partition(|f| f.category == ResourceCategory::Audio);
        if audio_items.is_empty() { return Ok(final_items); }

        let audio_groups: HashMap<String, Vec<FileInfo>> = audio_items.into_iter().map(|f| (f.filepath.with_extension("").to_string_lossy().to_string(), f)).into_group_map();
        let sorted_formats: Vec<_> = audio_groups.values().flatten()
            .filter_map(|f| f.filepath.extension().and_then(|e| e.to_str()).map(|s| s.to_uppercase()))
            .collect::<BTreeSet<_>>().into_iter().collect();

        if sorted_formats.len() <= 1 {
            final_items.extend(audio_groups.into_values().flatten());
            return Ok(final_items);
        }

        let user_choices = ui::get_user_choices_from_menu(&sorted_formats, "检测到多种音频格式，请选择", "all");
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
        let (audio_items, mut final_items): (Vec<FileInfo>, Vec<FileInfo>) = items.into_iter().partition(|f| f.category == ResourceCategory::Audio);
        if audio_items.is_empty() { return Ok(final_items); }

        let selected_format = self.context.args.audio_format.to_lowercase();
        info!("根据参数选择音频格式: {}", selected_format);

        // --- 精简：使用 extend 和 filter ---
        final_items.extend(audio_items.into_iter().filter(|f| {
            f.filepath.extension().and_then(|e| e.to_str()).map_or(false, |ext| ext.to_lowercase() == selected_format)
        }));

        Ok(final_items)
    }
}