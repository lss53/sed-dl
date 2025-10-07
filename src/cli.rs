// src/cli.rs

use clap::{command, Parser};
use std::path::PathBuf;

fn workers_in_range(s: &str) -> Result<usize, String> {
    let value: usize = s
        .parse()
        .map_err(|_| format!("`{}` isn't a valid number", s))?;
    if (1..=16).contains(&value) {
        Ok(value)
    } else {
        Err(format!("workers must be between 1 and 16"))
    }
}

#[derive(Parser, Debug, Clone)]
#[command(
    author,
    version,
    about,
    long_about = None,
    arg_required_else_help = true,
    disable_help_flag = true,
    disable_version_flag = true,
    override_usage = "sed-dl <MODE> [OPTIONS]",
)]
#[command(group(
    clap::ArgGroup::new("mode")
        .required(true)
        .args(&["interactive", "url", "id", "batch_file", "token_help"]),
))]
pub struct Cli {
    // --- Mode ---
    /// 启动交互式会话，逐一输入链接
    #[arg(short, long, action = clap::ArgAction::SetTrue, help_heading = "Mode")]
    pub interactive: bool,
    /// 指定要下载的单个资源链接
    #[arg(long, help_heading = "Mode")]
    pub url: Option<String>,
    /// 通过资源ID下载 (需配合 --type 使用)
    #[arg(long, help_heading = "Mode")]
    pub id: Option<String>,
    /// 从文本文件批量下载多个链接或ID
    #[arg(short, long, value_name = "FILE", help_heading = "Mode")]
    pub batch_file: Option<PathBuf>,
    /// 显示如何获取 Access Token 的指南并退出
    #[arg(long, action = clap::ArgAction::SetTrue, help_heading = "Mode")]
    pub token_help: bool,

    // --- Options ---
    /// [非交互] 指定下载项 (如 '1-5,8', 'all')
    #[arg(long, value_name = "SELECTION", help_heading = "Options")]
    pub select: Option<String>,
    /// [ID模式] 指定资源类型
    #[arg(long, help = "有效选项: tchMaterial, qualityCourse, syncClassroom/classActivity", help_heading = "Options")]
    pub r#type: Option<String>,
    /// 提供访问令牌(Access Token)
    #[arg(long, help_heading = "Options")]
    pub token: Option<String>,
    /// 强制重新下载已存在的文件
    #[arg(short, long, action = clap::ArgAction::SetTrue, help_heading = "Options")]
    pub force_redownload: bool,
    /// 选择视频清晰度: 'best', 'worst', 或 '720p' 等
    #[arg(short='q', long, default_value = "best", help_heading = "Options")]
    pub video_quality: String,
    /// 选择音频格式: 'mp3', 'm4a' 等
    #[arg(long, default_value = "mp3", help_heading = "Options")]
    pub audio_format: String,
    /// [批量模式] 为每个任务提供手动选择
    #[arg(long, action = clap::ArgAction::SetTrue, help_heading = "Options")]
    pub prompt_each: bool,
    /// 设置最大并发下载数
    #[arg(short, long, value_parser = workers_in_range, help_heading = "Options")]
    pub workers: Option<usize>,
    /// 设置文件保存目录
    #[arg(short, long, value_name = "DIR", default_value_os_t = PathBuf::from("downloads"), help_heading = "Options")]
    pub output: PathBuf,

    // --- General ---
    /// 显示此帮助信息并退出
    #[arg(short = 'h', long, action = clap::ArgAction::Help, global = true, help_heading = "General")]
    help: Option<bool>,
    /// 显示版本信息并退出
    #[arg(short = 'V', long, action = clap::ArgAction::Version, global = true, help_heading = "General")]
    version: Option<bool>,
}