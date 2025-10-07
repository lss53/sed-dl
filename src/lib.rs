// src/lib.rs

// 声明所有模块为公共模块，供库内外使用
pub mod cli;
pub mod client;
pub mod config;
pub mod constants;
pub mod downloader;
pub mod error;
pub mod extractor;
pub mod ui;
pub mod utils;

// 导入常用类型
use crate::{
    cli::Cli,
    client::RobustClient,
    config::AppConfig,
    downloader::{DownloadManager, ResourceDownloader},
    error::{AppError, AppResult},
};
use anyhow::anyhow;
use colored::*;
use std::{path::Path, sync::Arc};
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
}

/// 库的公共入口点，由 `main.rs` 调用
pub async fn run_from_cli(args: Arc<Cli>) -> AppResult<()> {
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
            "[i]".cyan()
        );
        return Ok(());
    }

    let config = Arc::new(AppConfig::from_args(&args));

    // 参数校验
    if (args.id.is_some() || args.batch_file.is_some()) && args.r#type.is_none() {
        return Err(AppError::Other(anyhow!(
            "使用 --id 或 --batch-file 时，必须提供 --type 参数。"
        )));
    }
    if let Some(t) = &args.r#type {
        if !config.api_endpoints.contains_key(t) {
            let valid_options = config
                .api_endpoints
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return Err(AppError::Other(anyhow!(
                "无效的资源类型 '{}'。有效选项: {}",
                t,
                valid_options
            )));
        }
    }

    // 初始化上下文
    let (token_opt, source) = config::resolve_token(args.token.as_deref());
    if token_opt.is_some() {
        println!("\n{} 已从 {} 加载 Access Token。", "[i]".cyan(), source);
    } else {
        println!(
            "\n{} 未找到本地 Access Token，将在需要时提示输入。",
            "[i]".cyan()
        );
    }
    let token = Arc::new(TokioMutex::new(token_opt.unwrap_or_default()));

    let context = DownloadJobContext {
        manager: DownloadManager::new(),
        token,
        config: config.clone(),
        http_client: Arc::new(RobustClient::new(config.clone())),
        args: args.clone(),
        non_interactive: !args.interactive && !args.prompt_each,
    };

    // 路由到不同的执行模式
    let all_ok = if args.interactive {
        handle_interactive_mode(context).await
    } else if let Some(batch_file) = &args.batch_file {
        process_batch_tasks(batch_file, context).await
    } else if let Some(url) = &args.url {
        ResourceDownloader::new(context).run(url).await?
    } else if let Some(id) = &args.id {
        ResourceDownloader::new(context).run_with_id(id).await?
    } else {
        true // Clap group rule prevents this
    };

    if !all_ok {
        // 使用一个自定义错误来表示有任务失败
        Err(AppError::Other(anyhow!("一个或多个任务执行失败。")))
    } else {
        Ok(())
    }
}

// --- 高层任务处理函数 (仅在库内部使用) ---

async fn handle_interactive_mode(base_context: DownloadJobContext) -> bool {
    ui::print_header("交互模式");
    println!(
        "在此模式下，你可以逐一输入链接进行下载。按 {} 可随时退出。",
        "Ctrl+C".yellow()
    );
    let mut all_tasks_ok = true;
    loop {
        match ui::prompt("请输入资源链接或ID", None) {
            Ok(input) if !input.is_empty() => {
                let context = base_context.clone();
                if !process_single_task_cli(&input, context).await {
                    all_tasks_ok = false;
                }
            }
            Ok(_) => break,  // Empty input exits
            Err(_) => break, // IO error (like Ctrl+C)
        }
    }
    println!("\n{} 退出交互模式。", "[i]".cyan());
    all_tasks_ok
}

async fn process_batch_tasks(batch_file: &Path, base_context: DownloadJobContext) -> bool {
    let content = match std::fs::read_to_string(batch_file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "{} 读取批量文件 '{}' 失败: {}",
                "[X]".red(),
                batch_file.display(),
                e
            );
            return false;
        }
    };
    let tasks: Vec<String> = content
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if tasks.is_empty() {
        println!(
            "{} 批量文件 '{}' 为空。",
            "[!]".yellow(),
            batch_file.display()
        );
        return true;
    }

    let mut success = 0;
    let mut failed = 0;
    ui::print_header(&format!(
        "开始批量处理任务 (按 {} 可随时退出)",
        "Ctrl+C".yellow()
    ));
    for (i, task) in tasks.iter().enumerate() {
        ui::print_sub_header(&format!(
            "批量任务 {}/{} - {}",
            i + 1,
            tasks.len(),
            utils::truncate_text(task, 60)
        ));
        let context = base_context.clone();
        if process_single_task_cli(task, context.clone()).await {
            success += 1;
        } else {
            failed += 1;
        }
    }

    ui::print_header("批量任务报告");
    println!(
        "{} | {} | 总计: {}",
        format!("成功任务: {}", success).green(),
        format!("失败任务: {}", failed).red(),
        tasks.len()
    );
    failed == 0
}

async fn process_single_task_cli(task_input: &str, context: DownloadJobContext) -> bool {
    let result = if utils::is_resource_id(task_input) {
        if context.args.r#type.is_none() {
            eprintln!(
                "{} 任务 '{}' 是一个ID，但未提供 --type 参数，跳过。",
                "[X]".red(),
                task_input
            );
            return false;
        }
        ResourceDownloader::new(context)
            .run_with_id(task_input)
            .await
    } else if Url::parse(task_input).is_ok() {
        ResourceDownloader::new(context).run(task_input).await
    } else {
        eprintln!("{} 跳过无效条目: {}", "[!]".yellow(), task_input);
        return true;
    };

    match result {
        Ok(success) => success,
        Err(e) => {
            eprintln!("\n{} 处理任务时发生错误: {}", "[X]".red(), e);
            false
        }
    }
}