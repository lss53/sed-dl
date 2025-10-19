// src/main.rs

use clap::{CommandFactory, FromArgMatches};
use colored::*;
use log::{error, info, warn};
use reqwest::StatusCode;
use sed_dl::{
    cli::{Cli, LogLevel},
    constants,
    error::AppError,
    run_from_cli, symbols, ui,
};
use std::{
    env,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
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
    let log_file_path = dirs::home_dir()
        .map(|home| home.join(constants::CONFIG_DIR_NAME).join(constants::LOG_FILE_NAME))
        .unwrap_or_else(|| {
            ui::warn("无法获取用户主目录，日志将写入临时目录。");
            env::temp_dir()
                .join(app_name)
                .join(constants::LOG_FILE_NAME)
        });

    if let Some(dir) = log_file_path.parent()
        && let Err(e) = std::fs::create_dir_all(dir) {
            ui::warn(&format!("无法创建日志目录 {:?}: {}", dir, e));
        }

    let file_appender = match fern::log_file(&log_file_path) {
        Ok(file) => file,
        Err(e) => {
            ui::warn(&format!(
                "无法打开日志文件 {:?} : {}。将尝试使用备用日志文件。",
                log_file_path, e
            ));
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
                    ui::error(&format!(
                        "无法创建备用日志文件 {:?}: {}。日志将不会被记录到文件。",
                        fallback_path, e_fb
                    ));
                    return;
                }
            }
        }
    };

    if let Err(e) = fern::Dispatch::new()
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
        .apply()
    {
        ui::warn(&format!("日志系统初始化失败: {}", e));
    }
}

fn setup_ctrl_c_handler() -> Arc<AtomicBool> {
    let cancellation_token = Arc::new(AtomicBool::new(false));
    let handler_token = cancellation_token.clone();

    tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("无法监听 {} 信号: {}", *symbols::CTRL_C, e);
            return;
        }

        if handler_token.load(Ordering::Relaxed) {
            ui::plain("\n第二次中断，强制退出...");
            warn!("用户第二次按下 {}，强制退出。", *symbols::CTRL_C);
            std::process::exit(130);
        }

        ui::warn(&format!(
            "\n正在停止... 请等待当前任务完成。再按一次 {} 可强制退出。",
            *symbols::CTRL_C
        ));
        warn!("用户通过 {} 请求中断程序。", *symbols::CTRL_C);
        handler_token.store(true, Ordering::Relaxed);
    });

    cancellation_token
}

#[tokio::main]
async fn main() {
    #[cfg(windows)]
    {
        colored::control::set_virtual_terminal(true).ok();
    }

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

    let cancellation_token = setup_ctrl_c_handler();

    if let Err(e) = run_from_cli(args, cancellation_token).await {
        handle_final_error(e);
    }

    info!("程序正常退出。");
}


/// 统一处理程序最终的错误，包括日志记录和向用户显示友好信息。
// handle_final_error 函数中 eprintln! 保持原样，因为它是专门的错误格式化器。
fn handle_final_error(e: AppError) {
    // 用户中断是预期行为，静默退出，并使用标准退出码 130
    if matches!(e, AppError::UserInterrupt) {
        warn!("程序被用户中断。");
        std::process::exit(130);
    }

    // 将错误信息记录到日志
    error!("程序因错误退出: {:?}", e);

    // 根据错误类型，生成不同的友好提示信息
    let (symbol, message, color_fn): (&ColoredString, String, fn(ColoredString) -> ColoredString) = match e {
        AppError::TokenInvalid | AppError::TokenMissing => {
            let msg = format!(
                "{}\n{} 请使用 --token-help 命令查看如何获取或更新的 Access Token。",
                e, *symbols::INFO
            );
            (&symbols::ERROR, msg, |s| s.red())
        }
        AppError::ApiParseFailed { url, source } => {
            let msg = format!(
                "{}\n   - {}: {}\n   - {}: {}\n\n{} 这通常意味着网站的API已更新。请尝试更新本程序或联系开发者。",
                "程序无法理解来自服务器的回应。",
                "请求地址".bold(), url,
                "错误详情".bold(), source,
                *symbols::INFO
            );
            (&symbols::ERROR, msg, |s| s.red())
        }
        // 将 403/404 视为用户输入警告
        AppError::Network(ref req_err)
            if req_err.status().is_some_and(|s| s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND) =>
        {
            let msg = "资源不存在 (链接或ID错误)。\n   - 请仔细检查输入的链接或ID是否正确。\n   - 如果是ID模式，请确认选择的 --type 是否匹配。".to_string();
            (&symbols::WARN, msg, |s| s.yellow())
        }
        // 其他网络错误仍然是严重错误
        AppError::Network(req_err) => {
            let msg = match req_err.status() {
                Some(status) => format!("服务器返回了一个未预期的错误: {}", status),
                None => "网络连接失败。请检查的网络连接和防火墙设置。".to_string(),
            };
            (&symbols::ERROR, msg, |s| s.red())
        }
        // 用户输入错误本身就是警告
        AppError::UserInputError(msg) => (&symbols::WARN, msg, |s| s.yellow()),
        // 对于所有其他类型的错误，视为严重错误
        _ => (&symbols::ERROR, e.to_string(), |s| s.red()),
    };

    eprintln!("\n{} {}", symbol, color_fn(message.into()));

    std::process::exit(1);
}