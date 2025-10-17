// src/lib.rs

pub mod cli;
pub mod client;
pub mod config;
pub mod constants;
pub mod downloader;
pub mod error;
pub mod extractor;
pub mod models;
pub mod symbols;
pub mod ui;
pub mod utils;

use crate::{
    cli::{Cli, ResourceType},
    client::RobustClient,
    config::AppConfig,
    downloader::{DownloadManager, ResourceDownloader},
    error::{AppError, AppResult},
};
use clap::ValueEnum;
use anyhow::anyhow;
use colored::*;
use log::{debug, info};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::Mutex as TokioMutex;
use url::Url;
use log::warn;
use reqwest::StatusCode;

/// 核心的执行上下文，包含所有任务所需的状态和工具
#[derive(Clone)]
pub struct DownloadJobContext {
    pub manager: DownloadManager,
    pub token: Arc<TokioMutex<String>>,
    pub config: Arc<AppConfig>,
    pub http_client: Arc<RobustClient>,
    pub args: Arc<Cli>,
    pub non_interactive: bool,
    pub cancellation_token: Arc<AtomicBool>,
}

/// 库的公共入口点，由 `main.rs` 调用
pub async fn run_from_cli(args: Arc<Cli>, cancellation_token: Arc<AtomicBool>) -> AppResult<()> {
    debug!("CLI 参数: {:?}", args);
    if args.token_help {
        ui::box_message(
            "获取 Access Token 指南",
            constants::HELP_TOKEN_GUIDE
                .lines()
                .collect::<Vec<_>>()
                .as_slice(),
            |s| s.cyan(),
        );
        println!(
            "\n{} 安全提醒: 请妥善保管你的 Token，不要分享给他人。",
            *symbols::INFO
        );
        return Ok(());
    }

    let config = Arc::new(AppConfig::new(&args)?);
    debug!("加载的应用配置: {:?}", config);

    let (token_opt, source) = config::token::resolve_token(args.token.as_deref());
    if token_opt.is_some() {
        info!("从 {} 加载 Access Token", source);
        println!("\n{} 已从 {} 加载 Access Token。", *symbols::INFO, source);
    } else {
        info!("未找到本地 Access Token");
        println!(
            "\n{}",
            format!(
                "{} 未找到本地 Access Token。",
                *symbols::INFO
            )
            .yellow()
        );
    }
    let token = Arc::new(TokioMutex::new(token_opt.unwrap_or_default()));

    let http_client = Arc::new(RobustClient::new(config.clone())?);

    let context = DownloadJobContext {
        manager: DownloadManager::new(),
        token,
        config: config.clone(),
        http_client,
        args: args.clone(),
        non_interactive: !args.interactive && !args.prompt_each,
        cancellation_token,
    };

    if args.interactive {
        handle_interactive_mode(context).await?;
    } else if let Some(batch_file) = &args.batch_file {
        process_batch_tasks(batch_file, context).await?;
    } else if let Some(url) = &args.url {
        ResourceDownloader::new(context).run(url).await?;
    } else if let Some(id) = &args.id {
        ResourceDownloader::new(context).run_with_id(id).await?;
    };

    Ok(())
}

async fn handle_interactive_mode(base_context: DownloadJobContext) -> AppResult<()> {
    ui::print_header("交互模式");
    let help_message = "在此模式下，你可以逐一输入 链接 或 ID 进行下载。";
    let prompt_message = "请输入资源链接或 ID";
    println!("{}按 {} 可随时退出。", help_message, *symbols::CTRL_C);

    loop {
        match ui::prompt(prompt_message, None) {
            Ok(input) if !input.is_empty() => {
                let input_for_log = input.clone();

                let result: AppResult<()> = if utils::is_resource_id(&input) {
                    // --- ID 处理逻辑，带有智能重试循环 ---
                    'type_selection_loop: loop {
                        let context = base_context.clone();
                        let resource_types = vec![
                            "tchMaterial".to_string(),
                            "qualityCourse".to_string(),
                            "syncClassroom/classActivity".to_string(),
                        ];
                        let choice_str = ui::selection_menu(
                            &resource_types,
                            &format!("检测到ID '{}'，请选择其资源类型", input),
                            "请输入数字选择类型 (直接按回车取消)",
                            "1",
                        );

                        if choice_str.is_empty() {
                            break 'type_selection_loop Ok(()); // 用户取消
                        }

                        let r#type = match choice_str.trim().parse::<usize>() {
                            Ok(idx) if idx > 0 && idx <= resource_types.len() => {
                                ResourceType::from_str(&resource_types[idx - 1], true).unwrap()
                            }
                            _ => {
                                eprintln!("\n{} 无效的选择 '{}'。", *symbols::ERROR, choice_str);
                                continue 'type_selection_loop; // 让用户重新选
                            }
                        };

                        let mut new_context = context;
                        let mut new_args = (*new_context.args).clone();
                        new_args.r#type = Some(r#type);
                        new_context.args = Arc::new(new_args);

                        let download_result = ResourceDownloader::new(new_context).run_with_id(&input).await;

                        match download_result {
                            Ok(_) => {
                                break 'type_selection_loop Ok(()); // 下载成功
                            }
                            Err(AppError::Network(ref req_err)) if req_err.status() == Some(StatusCode::FORBIDDEN) => {
                                warn!("ID '{}' 配合类型 '{:?}' 访问失败 (403)，可能是类型选择错误。", input, r#type);
                                println!("\n{} 访问失败，您选择的资源类型可能不正确。请重新选择。", *symbols::WARN);
                                // 自动继续循环，让用户重新选择
                            }
                            Err(e) => {
                                break 'type_selection_loop Err(e); // 其他错误，直接失败
                            }
                        }
                    }
                } else if Url::parse(&input).is_ok() {
                    let context = base_context.clone();
                    ResourceDownloader::new(context).run(&input).await.map(|_| ())
                } else {
                    Err(AppError::Other(anyhow!("输入 '{}' 既不是有效的链接，也不是有效的ID。", input)))
                };

                if let Err(e) = result {
                    log::error!("交互模式任务 '{}' 失败: {}", input_for_log, e);
                    if !matches!(e, AppError::UserInterrupt) {
                        eprintln!("\n{} 处理任务时发生错误: {}", *symbols::ERROR, e.to_string().red());
                    }
                }
            }
            Ok(_) => break, // 用户输入空行
            Err(_) => return Err(AppError::UserInterrupt), // 用户按 Ctrl+C
        }
    }

    println!("\n{} 退出交互模式。", *symbols::INFO);
    Ok(())
}

async fn process_batch_tasks(batch_file: &Path, base_context: DownloadJobContext) -> AppResult<()> {
    let content = std::fs::read_to_string(batch_file).map_err(|e| {
        log::error!("读取批量文件 '{}' 失败: {}", batch_file.display(), e);
        AppError::from(e)
    })?;

    let tasks: Vec<String> = content
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if tasks.is_empty() {
        log::warn!("批量文件 '{}' 为空或不含有效行。", batch_file.display());
        println!(
            "{} 批量文件 '{}' 为空。",
            *symbols::WARN,
            batch_file.display()
        );
        return Ok(());
    }

    let mut success = 0;
    let mut failed = 0;
    ui::print_header(&format!(
        "开始批量处理任务 (按 {} 可随时退出)",
        *symbols::CTRL_C
    ));
    for (i, task) in tasks.iter().enumerate() {
        if base_context
            .cancellation_token
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(AppError::UserInterrupt);
        }
        ui::print_sub_header(&format!(
            "批量任务 {}/{} - {}",
            i + 1,
            tasks.len(),
            utils::truncate_text(task, 60)
        ));
        let context = base_context.clone();
        match process_single_task_cli(task, context.clone()).await {
            Ok(_) => success += 1,
            Err(e) => {
                failed += 1;
                log::error!("批量任务 '{}' 失败: {}", task, e);
                eprintln!("\n{} 处理任务时发生错误: {}", *symbols::ERROR, e);
            }
        }
    }

    ui::print_header("批量任务报告");
    println!(
        "{} | {} | 总计: {}",
        format!("成功任务: {}", success).green(),
        format!("失败任务: {}", failed).red(),
        tasks.len()
    );
    if failed > 0 {
        Err(AppError::Other(anyhow!("{} 个批量任务执行失败。", failed)))
    } else {
        Ok(())
    }
}

async fn process_single_task_cli(task_input: &str, context: DownloadJobContext) -> AppResult<()> {
    let result: AppResult<bool> = if utils::is_resource_id(task_input) {
        if context.args.r#type.is_none() {
            let msg = format!(
                "任务 '{}' 是一个ID，但未提供 --type 参数，跳过。",
                task_input
            );
            log::error!("{}", msg);
            eprintln!("{} {}", *symbols::ERROR, msg);
            return Err(AppError::Other(anyhow!(msg)));
        }
        ResourceDownloader::new(context)
            .run_with_id(task_input)
            .await
    } else if Url::parse(task_input).is_ok() {
        ResourceDownloader::new(context).run(task_input).await
    } else {
        let msg = format!("跳过无效条目: {}", task_input);
        log::warn!("{}", msg);
        eprintln!("{} {}", *symbols::WARN, msg);
        return Ok(()); // 无效条目不是一个错误，直接返回成功
    };

    result.map(drop) // 如果成功，丢弃bool值，返回()
}
