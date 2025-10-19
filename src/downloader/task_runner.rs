// src/downloader/task_runner.rs

use super::task_processor::TaskProcessor;
use crate::{DownloadJobContext, error::*, models::*, ui};
use futures::{StreamExt, stream};
use indicatif::{HumanBytes, ProgressBar};
use log::error;
use std::{
    cmp::min,
    sync::{Arc, atomic::Ordering},
};

/// 负责执行一批下载任务，管理并发和进度报告。
pub async fn execute_tasks(context: &DownloadJobContext, tasks: &[FileInfo]) -> AppResult<()> {
    let max_workers = min(context.config.max_workers, tasks.len());
    if max_workers == 0 {
        return Ok(());
    }

    let all_sizes_available = tasks.iter().all(|t| t.ti_size.is_some_and(|s| s > 0));

    // 在所有检查都通过后，才创建并显示进度条
    let main_pbar = setup_progress_bar(tasks, max_workers, all_sizes_available);

    let error_sender = Arc::new(tokio::sync::Mutex::new(None::<AppError>));

    stream::iter(tasks.to_owned())
        .for_each_concurrent(max_workers, |task| {
            run_single_concurrent_task(
                task,
                context.clone(),
                main_pbar.clone(),
                error_sender.clone(),
                all_sizes_available,
            )
        })
        .await;

    main_pbar.finish_and_clear();
    if context
        .cancellation_token
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(AppError::UserInterrupt);
    }
    if let Some(err) = error_sender.lock().await.take() {
        return Err(err);
    }
    Ok(())
}

/// 在并发池中运行的单个任务单元。
async fn run_single_concurrent_task(
    task: FileInfo,
    context: DownloadJobContext,
    main_pbar: ProgressBar,
    error_sender: Arc<tokio::sync::Mutex<Option<AppError>>>,
    use_byte_progress: bool,
) {
    if context.cancellation_token.load(Ordering::Relaxed) || error_sender.lock().await.is_some() {
        return;
    }

    // 创建任务处理器并执行
    let processor = TaskProcessor::new(context.clone());
    let result = processor
        .process(task.clone(), main_pbar.clone(), use_byte_progress)
        .await;

    match result {
        Ok(result) => {
            // 更新统计数据
            match result.status {
                DownloadStatus::Success | DownloadStatus::Resumed => {
                    context.manager.record_success()
                }
                DownloadStatus::Skipped => context.manager.record_skip(
                    &result.filename,
                    result.message.as_deref().unwrap_or("文件已存在"),
                ),
                _ => context
                    .manager
                    .record_failure(&result.filename, result.status),
            }

            // 更新进度条
            if !use_byte_progress {
                main_pbar.inc(1);
            } else if result.status == DownloadStatus::Skipped
                && let Some(skipped_size) = task.ti_size {
                    main_pbar.inc(skipped_size);
                }

            // 打印单项结果
            if result.status != DownloadStatus::Skipped {
                let (symbol, color_fn, default_msg) = result.status.get_display_info();
                let task_name = task.filepath.file_name().unwrap().to_string_lossy();
                let msg = if let Some(err_msg) = result.message {
                    format!(
                        "\n{} {} {}",
                        symbol,
                        task_name,
                        color_fn(format!("失败: {} (详情: {})", default_msg, err_msg).into())
                    )
                } else {
                    format!("{} {}", symbol, task_name)
                };
                main_pbar.println(msg);
            }
        }
        Err(e @ AppError::TokenInvalid) => {
            // 捕获致命的 Token 错误并中止整个批次
            let mut error_lock = error_sender.lock().await;
            if error_lock.is_none() {
                let task_name = task.filepath.to_string_lossy();
                error!("任务 '{}' 因 Token 失效失败，将中止整个批次。", task_name);
                context
                    .manager
                    .record_failure(&task_name, DownloadStatus::TokenError);
                *error_lock = Some(e);
            }
        }
        Err(e) => {
            error!("未捕获的错误在并发循环中: {}", e);
            if !use_byte_progress {
                main_pbar.inc(1);
            }
        }
    }
}

/// 根据任务列表信息，配置并返回一个合适的进度条。
fn setup_progress_bar(tasks: &[FileInfo], max_workers: usize, all_sizes_available: bool) -> ProgressBar {
    if all_sizes_available {
        let total_size: u64 = tasks.iter().filter_map(|t| t.ti_size).sum();
        ui::plain(""); // 产生空行
        ui::info(&format!(
            "开始下载 {} 个文件 (总大小: {}) (并发数: {})...",
            tasks.len(),
            HumanBytes(total_size),
            max_workers
        ));
        
        ui::new_bytes_progress_bar(total_size, "下载")

    } else {
        ui::plain("");
        ui::warn("部分文件大小未知，将按文件数量显示进度。");
        ui::info(&format!(
            "开始下载 {} 个文件 (并发数: {})...",
            tasks.len(),
            max_workers
        ));
        ui::new_tasks_progress_bar(tasks.len() as u64, "下载")
    }
}
