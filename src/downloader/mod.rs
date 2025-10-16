// src/downloader/mod.rs

// 1. 声明所有新的私有模块
mod auth;
mod dispatcher;
mod job;
mod m3u8;
mod negotiator;
mod task_processor;
mod task_runner;

// 2. 从子模块中导出公共接口
pub use job::ResourceDownloader;

// 3. 将 DownloadManager 的逻辑移到这里，因为它是一个核心的、共享的状态管理器
use crate::{symbols, ui};
use colored::*;
use log::info;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub struct DownloadStats {
    pub total: usize,
    pub success: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Clone)]
pub struct DownloadManager {
    stats: Arc<Mutex<DownloadStats>>,
    failed_downloads: Arc<Mutex<Vec<(String, String)>>>,
    skipped_downloads: Arc<Mutex<Vec<(String, String)>>>,
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            stats: Arc::new(Mutex::new(DownloadStats::default())),
            failed_downloads: Arc::new(Mutex::new(Vec::new())),
            skipped_downloads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn start_batch(&self, total_tasks: usize) {
        info!("开始新一批下载任务，总数: {}", total_tasks);
        let mut stats = self.stats.lock().unwrap();
        *stats = DownloadStats {
            total: total_tasks,
            ..Default::default()
        };
        self.failed_downloads.lock().unwrap().clear();
        self.skipped_downloads.lock().unwrap().clear();
    }

    pub fn record_success(&self) {
        self.stats.lock().unwrap().success += 1;
    }

    pub fn record_skip(&self, filename: &str, reason: &str) {
        info!("跳过文件 '{}'，原因: {}", filename, reason);
        self.stats.lock().unwrap().skipped += 1;
        self.skipped_downloads
            .lock()
            .unwrap()
            .push((filename.to_string(), reason.to_string()));
    }

    pub fn record_failure(&self, filename: &str, status: crate::models::DownloadStatus) {
        log::error!("文件 '{}' 下载失败，状态: {:?}", filename, status);
        self.stats.lock().unwrap().failed += 1;
        let (_, _, msg) = status.get_display_info();
        self.failed_downloads
            .lock()
            .unwrap()
            .push((filename.to_string(), msg.to_string()));
    }

    pub fn reset_token_failures(&self, filenames_to_reset: &[String]) {
        let mut failed_downloads = self.failed_downloads.lock().unwrap();
        let original_len = failed_downloads.len();
        failed_downloads.retain(|(name, _)| !filenames_to_reset.contains(name));
        let removed_count = original_len - failed_downloads.len();
        if removed_count > 0 {
            info!("重置了 {} 个因Token失败的任务", removed_count);
            self.stats.lock().unwrap().failed -= removed_count;
        }
    }

    pub fn get_stats(&self) -> DownloadStats {
        self.stats.lock().unwrap().clone()
    }

    pub fn did_all_succeed(&self) -> bool {
        self.stats.lock().unwrap().failed == 0
    }

    pub fn print_report(&self) {
        let stats = self.get_stats();
        let skipped = self.skipped_downloads.lock().unwrap();
        let failed = self.failed_downloads.lock().unwrap();
        info!(
            "下载报告: Total={}, Success={}, Skipped={}, Failed={}",
            stats.total, stats.success, stats.skipped, stats.failed
        );

        if !skipped.is_empty() || !failed.is_empty() {
            ui::print_sub_header("下载详情报告");
            if !skipped.is_empty() {
                println!("\n{} 跳过的文件 ({}个):", *symbols::INFO, stats.skipped);
                print_grouped_report(&skipped, |s| s.cyan());
            }
            if !failed.is_empty() {
                println!("\n{} 失败的文件 ({}个):", *symbols::ERROR, stats.failed);
                print_grouped_report(&failed, |s| s.red());
            }
        }
        ui::print_sub_header("任务总结");
        if stats.total > 0 && stats.success == stats.total - stats.skipped {
            println!(
                "{} 所有 {} 个任务均已成功 ({} 个已跳过)。",
                *symbols::OK,
                stats.total,
                stats.skipped
            );
        } else {
            let summary = format!(
                "{} | {} | {}",
                format!("成功: {}", stats.success).green(),
                format!("失败: {}", stats.failed).red(),
                format!("跳过: {}", stats.skipped).yellow()
            );
            println!("{}", summary);
        }
    }
}

// 模块内的私有辅助函数
fn print_grouped_report(
    items: &[(String, String)],
    color_fn: fn(ColoredString) -> ColoredString,
) {
    let mut grouped: HashMap<&String, Vec<&String>> = HashMap::new();
    for (filename, reason) in items {
        grouped.entry(reason).or_default().push(filename);
    }
    let mut sorted_reasons: Vec<_> = grouped.keys().collect();
    sorted_reasons.sort();
    for reason in sorted_reasons {
        println!("  - {}", color_fn(format!("原因: {}", reason).into()));
        let mut filenames = grouped.get(reason).unwrap().clone();
        filenames.sort();
        for filename in filenames {
            println!("    - {}", filename);
        }
    }
}
    