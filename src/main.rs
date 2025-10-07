// src/main.rs

use clap::{CommandFactory, FromArgMatches};
use colored::*;
use sed_dl::{cli::Cli, run_from_cli};
use std::{env, sync::Arc, time::Duration};

#[tokio::main]
async fn main() {
    // 为 Windows 终端启用 ANSI 颜色支持。
    // 仅在 Windows 平台上编译并执行此代码块
    #[cfg(windows)]
    {
        colored::control::set_virtual_terminal(true).ok();
    }
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        println!("\n{} 用户强制中断程序。", "[!]".yellow());
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::process::exit(130);
    });

    let bin_name = env::var("CARGO_BIN_NAME").unwrap_or_else(|_| "sed-dl".to_string());

    let after_help = format!(
        "示例:\n  # 启动交互模式 (推荐)\n  {bin} -i\n\n  # 自动下载单个链接中的所有内容\n  {bin} --url \"https://...\"\n\n  # 批量下载\n  {bin} -b my_links.txt --type syncClassroom/classActivity\n\n  # 获取 Token 帮助\n  {bin} --token-help",
        bin = bin_name
    );

    let cmd = Cli::command().after_help(after_help);

    let args = Arc::new(Cli::from_arg_matches(&cmd.get_matches()).unwrap());


    if let Err(e) = run_from_cli(args).await {
        eprintln!("\n{} {}", "[X]".red(), format!("程序执行出错: {}", e).red());
        std::process::exit(1);
    }
}