// src/workflows.rs

use crate::{
    cli::ResourceType,
    constants,
    downloader::ResourceDownloader,
    error::{AppError, AppResult},
    models::{FileInfo, MetadataExtractionResult},
    symbols, ui, utils, DownloadJobContext,
};
use anyhow::anyhow;
use colored::*;
use futures::{stream, StreamExt};
use log::{debug, warn};
use reqwest::StatusCode;
use std::path::PathBuf;
use url::Url;

/// 运行单任务模式（处理 --url 或 --id）
pub(crate) async fn run_single(context: DownloadJobContext) -> AppResult<()> {
    let downloader = ResourceDownloader::new(context.clone());
    // 在单任务模式下，url 或 id 必须存在，这是由 clap 的 arg_required_else_help 保证的
    let task_input = context.args.url.as_deref().or(context.args.id.as_deref()).unwrap();
    
    let metadata_result = downloader.fetch_metadata(task_input).await?;
    let all_files = metadata_result.files;

    print_single_task_filter_summary(
        &context,
        metadata_result.original_count,
        metadata_result.after_ext_filter_count,
        metadata_result.after_version_filter_count,
    );
    
    downloader.process_and_download_items(all_files).await?;
    Ok(())
}


/// 运行交互模式
pub(crate) async fn run_interactive(base_context: DownloadJobContext) -> AppResult<()> {
    ui::print_header("交互模式");
    ui::plain(&format!("在此模式下，你可以逐一输入 链接 或 ID 进行下载。按 {} 可随时退出。", *symbols::CTRL_C));

    loop {
        match ui::prompt("请输入资源链接或 ID", None) {
            Ok(input) if !input.is_empty() => {
                let downloader = ResourceDownloader::new(base_context.clone());
                
                let result = async {
                    let metadata_result = if utils::is_resource_id(&input) {
                        process_id_with_auto_detect(&input, base_context.clone()).await?
                    } else if Url::parse(&input).is_ok() {
                        downloader.fetch_metadata(&input).await?
                    } else {
                        return Err(AppError::UserInputError(format!("输入 '{}' 不是有效链接或ID。", input)));
                    };
                    
                    let all_files = metadata_result.files;

                    print_single_task_filter_summary(
                        &base_context,
                        metadata_result.original_count,
                        metadata_result.after_ext_filter_count,
                        metadata_result.after_version_filter_count,
                    );

                    downloader.process_and_download_items(all_files).await.map(|_|())
                }.await;

                if let Err(e) = result {
                    log::error!("交互模式任务 '{}' 失败: {}", &input, e);
                    if matches!(e, AppError::UserInterrupt) { continue; }
                    let error_message = match e {
                        AppError::Network(req_err) 
                            if req_err.status().is_some_and(|s| s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND) => 
                        {
                            format!("{} {}", *symbols::WARN, "资源不存在，请检查输入的链接或ID是否正确。".yellow())
                        },
                        AppError::Network(req_err) => {
                            let friendly_msg = match req_err.status() {
                                Some(status) => format!("服务器返回了一个错误: {}", status),
                                None => "网络连接错误。".to_string(),
                            };
                            format!("{} {}", *symbols::ERROR, friendly_msg.red())
                        },
                        AppError::UserInputError(msg) => format!("{} {}", *symbols::WARN, msg.yellow()),
                        _ => format!("{} 处理时发生错误: {}", *symbols::ERROR, e.to_string().red()),
                    };
                    eprintln!("\n{}", error_message);
                }
            }
            Ok(_) => break, // 用户输入空行，退出循环
            Err(_) => return Err(AppError::UserInterrupt),
        }
    }
    
    ui::plain("");
    ui::info("退出交互模式。");
    Ok(())
}

/// 运行批量模式
pub(crate) async fn run_batch(batch_file: PathBuf, base_context: DownloadJobContext) -> AppResult<()> {
    let content = std::fs::read_to_string(&batch_file).map_err(AppError::from)?;
    let tasks: Vec<String> = content.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if tasks.is_empty() {
        ui::warn("批量文件为空。");
        return Ok(());
    }

    let downloader = ResourceDownloader::new(base_context.clone());

    ui::print_header(&format!("阶段 1/2: 批量解析任务 (共 {} 个)", tasks.len()));

    // ... (批量模式的其余代码保持不变) ...
    let mut global_filters = Vec::new();
    if let Some(exts) = &base_context.args.filter_ext {
        global_filters.push(format!("扩展名保留: {}", exts.join(",")));
    }
    if base_context.args.video_quality != constants::DEFAULT_VIDEO_QUALITY {
        global_filters.push(format!("视频选择策略: '{}'", base_context.args.video_quality));
    }
    if base_context.args.audio_format != constants::DEFAULT_AUDIO_FORMAT {
        global_filters.push(format!("音频选择策略: '{}'", base_context.args.audio_format));
    }

    if !global_filters.is_empty() {
        ui::info("将应用全局过滤器:");
        for filter in global_filters {
            ui::plain(&format!("    - {}", filter));
        }
        ui::plain("");
    }

    let pbar = ui::new_tasks_progress_bar(tasks.len() as u64, "解析");

    let mut stream = stream::iter(tasks.clone())
        .map(|task| {
            let downloader = downloader.clone();
            let pbar_clone = pbar.clone();
            async move { (task.clone(), downloader.fetch_metadata(&task).await, pbar_clone) }
        })
        .buffer_unordered(base_context.config.max_workers);

    let mut all_files_to_process: Vec<FileInfo> = Vec::new();
    let mut metadata_failed = 0;

    while let Some((task, result, pbar)) = stream.next().await {
        match result {
            Ok(metadata_result) => {
                let files = metadata_result.files;
                let original_count = metadata_result.original_count;
                let ext_filtered_count = metadata_result.after_ext_filter_count;
                let version_filtered_count = metadata_result.after_version_filter_count;

                let final_details_str: String = if original_count == files.len() {
                    format!("找到 {} 个文件", files.len())
                } else {
                    let mut count_chain = vec![original_count];
                    if original_count > ext_filtered_count {
                        count_chain.push(ext_filtered_count);
                    }
                    if ext_filtered_count > version_filtered_count {
                        count_chain.push(version_filtered_count);
                    }
                    
                    format!(
                        "过滤: {}", 
                        count_chain.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(" -> ")
                    )
                };
                
                if files.is_empty() {
                    log::info!("任务 '{}' 未解析到任何文件。", task);
                    pbar.println(format!("{} {} (未找到文件)", *symbols::INFO, utils::truncate_text(&task, 60)));
                } else {
                    pbar.println(format!(
                        "{} {} {}",
                        *symbols::OK,
                        utils::truncate_text(&task, 60),
                        final_details_str
                    ));
                    all_files_to_process.extend(files);
                }
            }
            Err(e) => {
                metadata_failed += 1;
                log::error!("解析任务 '{}' 失败: {}", task, e);
                let error_message = match &e {
                    AppError::Network(req_err) => {
                        if let Some(status) = req_err.status() {
                            format!("网络错误: {}", status)
                        } else {
                            "网络连接失败".to_string()
                        }
                    },
                    _ => e.to_string(),
                };
                pbar.println(format!("{} {} ({})", *symbols::ERROR, utils::truncate_text(&task, 60), error_message));
            }
        }
        pbar.inc(1);
    }
    
    pbar.finish_and_clear();

    if all_files_to_process.is_empty() {
        ui::print_header("任务报告");
        ui::info("未能从任何任务中解析到可下载的文件。");
        return if metadata_failed > 0 {
            Err(AppError::Other(anyhow!("{} 个任务元数据解析失败。", metadata_failed)))
        } else { Ok(()) };
    }

    let successful_tasks_count = tasks.len() - metadata_failed;
    ui::print_header(&format!(
        "阶段 2/2: 批量下载任务 (成功 {} 个任务，共 {} 个文件)",
        successful_tasks_count,
        all_files_to_process.len()
    ));
    downloader.process_and_download_items(all_files_to_process).await?;

    if metadata_failed > 0 {
        let warning_message = format!(
            "额外信息: 在开始下载前，有 {} 个任务的元数据解析失败。",
            metadata_failed
        );
        ui::plain("");
        ui::warn(&warning_message);
    }

    Ok(())
}

// --- 模块内部辅助函数 ---

/// 打印单任务的过滤总结
fn print_single_task_filter_summary(
    context: &DownloadJobContext,
    original_count: usize,
    ext_filtered_count: usize,
    version_filtered_count: usize,
) {
    if original_count > ext_filtered_count {
        if let Some(exts) = &context.args.filter_ext {
            ui::info(&format!(
                "已应用扩展名过滤器 (保留: {}), 文件数量从 {} 个变为 {} 个。",
                exts.join(","), original_count, ext_filtered_count
            ));
        }
    }

    if ext_filtered_count > version_filtered_count {
        let mut filters_applied = Vec::new();
        if context.args.video_quality != constants::DEFAULT_VIDEO_QUALITY {
            filters_applied.push(format!("视频 '{}'", context.args.video_quality));
        }
        if context.args.audio_format != constants::DEFAULT_AUDIO_FORMAT {
            filters_applied.push(format!("音频 '{}'", context.args.audio_format));
        }
        
        if !filters_applied.is_empty() {
            ui::info(&format!(
                "已应用版本选择 (选择: {}), 文件数量从 {} 个变为 {} 个。",
                filters_applied.join(", "), ext_filtered_count, version_filtered_count
            ));
        }
    }
}


/// 自动检测ID对应的资源类型
async fn process_id_with_auto_detect(
    id: &str,
    base_context: DownloadJobContext,
) -> AppResult<MetadataExtractionResult> { 
    let resource_types = [
        ResourceType::TchMaterial,
        ResourceType::QualityCourse,
        ResourceType::SyncClassroom,
    ];
    ui::plain("");
    ui::info("检测到ID，正在检索资源类型...");

    for r#type in resource_types {
        let mut context = base_context.clone();
        let mut new_args = (*context.args).clone();
        new_args.r#type = Some(r#type);
        context.args = std::sync::Arc::new(new_args);

        let downloader = ResourceDownloader::new(context);
        match downloader.fetch_metadata(id).await {
            Ok(result) if !result.files.is_empty() => return Ok(result),
            Ok(_) => debug!("ID '{}' 在类型 '{:?}' 下未找到文件。", id, r#type),
            Err(e @ AppError::TokenInvalid) => return Err(e),
            Err(e) => {
                warn!(
                    "在类型 '{:?}' 下检索ID '{}' 时遇到可恢复错误: {}",
                    r#type, id, e
                );
            }
        }
    }
    Err(AppError::UserInputError(format!(
        "无法为ID '{}' 检索到匹配的资源类型。",
        id
    )))
}