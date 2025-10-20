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
pub mod workflows;

use crate::{
    cli::Cli,
    client::RobustClient,
    config::AppConfig,
    downloader::DownloadManager,
    error::AppResult,
};
use colored::Colorize;
use log::{debug, info};
use std::sync::{atomic::AtomicBool, Arc};
use tokio::sync::Mutex as TokioMutex;

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

pub async fn run_from_cli(args: Arc<Cli>, cancellation_token: Arc<AtomicBool>) -> AppResult<()> {
    debug!("CLI 参数: {:?}", args);
    if args.token_help {
        ui::box_message(
            "获取 Access Token 指南",
            constants::HELP_TOKEN_GUIDE.lines().collect::<Vec<_>>().as_slice(),
            |s| s.cyan(),
        );
        ui::plain("");
        ui::info("安全提醒: 请妥善保管你的 Token。");
        return Ok(());
    }

    let config = Arc::new(AppConfig::new(&args)?);
    debug!("加载的应用配置: {:?}", config);

    let (token_opt, source) = config::token::resolve_token(args.token.as_deref());
    if token_opt.is_some() {
        info!("从 {} 加载 Access Token", source);
        ui::plain("");
        ui::info(&format!("已从 {} 加载 Access Token。", source));
    } else {
        info!("未找到本地 Access Token");
        ui::plain("");
        ui::warn("未找到本地 Access Token。");
    }
    let token = Arc::new(TokioMutex::new(token_opt.unwrap_or_default()));

    let http_client = Arc::new(RobustClient::new(config.clone())?);

    let context = DownloadJobContext {
        manager: DownloadManager::new(),
        token,
        config: config.clone(),
        http_client,
        args: args.clone(),
        non_interactive: !args.interactive,
        cancellation_token,
    };

    // --- 核心分发逻辑 ---
    if args.interactive {
        workflows::run_interactive(context).await?;
    } else if let Some(batch_file) = &args.batch_file {
        workflows::run_batch(batch_file.clone(), context).await?;
    } else {
        workflows::run_single(context).await?;
    };

    Ok(())
}