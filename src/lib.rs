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
    cli::Cli,
    client::RobustClient,
    config::AppConfig,
    downloader::{DownloadManager, ResourceDownloader},
    error::{AppError, AppResult},
};
use anyhow::anyhow;
use colored::*;
use log::{debug, info};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::Mutex as TokioMutex;
use url::Url;

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
                "{} 未找到本地 Access Token，将在需要时提示输入。",
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

    let prompt_message: &str;
    let help_message: &str;

    if base_context.args.r#type.is_some() {
        let type_name = match base_context.args.r#type.unwrap() {
            crate::cli::ResourceType::TchMaterial => "教材 (tchMaterial)",
            crate::cli::ResourceType::QualityCourse => "精品课 (qualityCourse)",
            crate::cli::ResourceType::SyncClassroom => "同步课堂 (syncClassroom/classActivity)",
        };
        println!("你正处于针对 [{}] 类型的ID下载模式。", type_name.yellow());
        help_message = "在此模式下，你可以逐一输入ID进行下载。";
        prompt_message = "请输入资源ID";
    } else {
        help_message = "在此模式下，你可以逐一输入链接进行下载。";
        prompt_message = "请输入资源链接";
    }

    println!("{}按 {} 可随时退出。", help_message, *symbols::CTRL_C);

    loop {
        match ui::prompt(prompt_message, None) {
            Ok(input) if !input.is_empty() => {
                let context = base_context.clone();
                // 忽略单个任务的错误，以便继续交互模式
                if let Err(e) = process_single_task_cli(&input, context).await {
                    log::error!("交互模式任务 '{}' 失败: {}", input, e);
                    eprintln!("\n{} 处理任务时发生错误: {}", *symbols::ERROR, e);
                }
            }
            Ok(_) => break,                                // 用户输入空行，退出
            Err(_) => return Err(AppError::UserInterrupt), // Ctrl-C
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
