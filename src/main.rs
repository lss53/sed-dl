// src/main.rs

use clap::{CommandFactory, FromArgMatches};
use colored::*;
use log::{error, info, warn};
use sed_dl::{
    cli::{Cli, LogLevel},
    constants,
    error::AppError,
    run_from_cli, symbols,
};
use std::{
    env,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn init_logger(level: LogLevel) {
    if level == LogLevel::Off {
        return;
    }

    let filter = match level {
        LogLevel::Off => log::LevelFilter::Off,
        LogLevel::Error => log::LevelFilter::Error,
        LogLevel::Warn => log::LevelFilter::Warn,
        LogLevel::Info => log::LevelFilter::Info,
        LogLevel::Debug => log::LevelFilter::Debug,
        LogLevel::Trace => log::LevelFilter::Trace,
    };

    let app_name = clap::crate_name!();
    let log_file_path = match dirs::home_dir() {
        Some(home) => home
            .join(constants::CONFIG_DIR_NAME)
            .join(constants::LOG_FILE_NAME),
        None => {
            eprintln!("警告: 无法获取用户主目录，日志将写入临时目录。");
            env::temp_dir()
                .join(app_name) // 在临时目录下创建一个以程序名命名的子目录
                .join(constants::LOG_FILE_NAME)
        }
    };

    #[allow(clippy::collapsible_if)]
    if let Some(dir) = log_file_path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("警告: 无法创建日志目录 {:?}: {}", dir, e);
        }
    }

    let file_appender = match fern::log_file(&log_file_path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!(
                "警告: 无法打开日志文件 {:?} : {}。将尝试使用备用日志文件。",
                log_file_path, e
            );

            let fallback_path = std::env::temp_dir().join(format!(
                "{}-{}",
                app_name,
                constants::LOG_FALLBACK_FILE_NAME
            ));

            match fern::log_file(&fallback_path) {
                Ok(fb_file) => {
                    warn!("日志将写入备用文件: {:?}", fallback_path);
                    fb_file
                }
                Err(e_fb) => {
                    eprintln!(
                        "错误: 无法创建备用日志文件 {:?}: {}。日志将不会被记录到文件。",
                        fallback_path, e_fb
                    );
                    return;
                }
            }
        }
    };

    let result = fern::Dispatch::new()
        .level(filter)
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{}] [{:<5}] [{}:{}] - {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                record.level(),
                record.target(),
                record.line().unwrap_or(0),
                message
            ))
        })
        .chain(file_appender)
        .apply();

    if let Err(e) = result {
        eprintln!("警告: 日志系统初始化失败: {}", e);
    }
}

#[tokio::main]
async fn main() {
    #[cfg(windows)]
    {
        colored::control::set_virtual_terminal(true).ok();
    }

    // 直接在 format! 宏中使用 clap::crate_name!()
    let after_help = format!(
        "示例:\n  # 启动交互模式 (推荐)\n  {bin} -i\n\n  # 自动下载单个链接中的所有内容\n  {bin} --url \"https://...\"\n\n  # 批量下载并显示调试信息 (日志写入文件)\n  {bin} -b my_links.txt --type tchMaterial --log-level debug\n\n  # 获取 Token 帮助\n  {bin} --token-help",
        bin = clap::crate_name!()
    );

    let cmd = Cli::command()
        .override_usage(format!("{} <MODE> [OPTIONS]", clap::crate_name!()))
        .after_help(after_help);

    let matches = cmd.get_matches();
    let args = Arc::new(Cli::from_arg_matches(&matches).unwrap());

    init_logger(args.log_level);

    let cancellation_token = Arc::new(AtomicBool::new(false));
    let handler_token = cancellation_token.clone();

    tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("无法监听 Ctrl-C 信号: {}", e);
            return;
        }

        if handler_token.load(Ordering::Relaxed) {
            println!("\n第二次中断，强制退出...");
            warn!("用户第二次按下 Ctrl+C，强制退出。");
            std::process::exit(130);
        }

        println!(
            "\n{} 正在停止... 请等待当前任务完成。再按一次 {} 可强制退出。",
            *symbols::WARN,
            *symbols::CTRL_C
        );
        warn!("用户通过 Ctrl+C 请求中断程序。");
        handler_token.store(true, Ordering::Relaxed);
    });

    if let Err(e) = run_from_cli(args, cancellation_token).await {
        match e {
            AppError::UserInterrupt => {
                // 用户中断是预期行为，静默退出，并使用标准退出码 130
                warn!("程序被用户中断。");
                std::process::exit(130);
            }
            AppError::TokenInvalid => {
                error!("程序因Token无效而退出: {}", e);
                eprintln!("\n{} {}", *symbols::ERROR, format!("{}", e).red());
                eprintln!(
                    "{} 请使用 --token-help 命令查看如何获取或更新您的 Access Token。",
                    *symbols::INFO
                );
                std::process::exit(1);
            }
            _ => {
                // 其他所有错误
                error!("程序执行出错: {}", e);
                eprintln!(
                    "\n{} {}",
                    *symbols::ERROR,
                    format!("程序执行出错: {}", e).red()
                );
                std::process::exit(1);
            }
        }
    }

    info!("程序正常退出。");
}
