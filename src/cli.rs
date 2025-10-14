// src/cli.rs

use crate::constants;
use clap::{Parser, ValueEnum, command, crate_version};
use std::path::PathBuf;

/// 定义日志输出级别
#[derive(ValueEnum, Copy, Clone, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// 定义可下载的资源类型
#[derive(ValueEnum, Copy, Clone, Debug, PartialEq, Eq)]
pub enum ResourceType {
    #[value(name = "tchMaterial")]
    TchMaterial,
    #[value(name = "qualityCourse")]
    QualityCourse,
    #[value(name = "syncClassroom/classActivity")]
    SyncClassroom,
}

// command 属性
#[derive(Parser, Debug, Clone)]
#[command(
    version = crate_version!(),
    about,
    long_about = None,
    arg_required_else_help = true,
    disable_help_flag = true,
    disable_version_flag = true,
)]
#[command(group(
    clap::ArgGroup::new("mode")
        .required(true)
        .args(&["interactive", "url", "id", "batch_file", "token_help"]),
))]
pub struct Cli {
    // --- 运行模式 (Mode) ---
    /// 启动交互式会话，逐一输入链接
    #[arg(short, long, action = clap::ArgAction::SetTrue, help_heading = "Mode")]
    pub interactive: bool,
    /// 指定要下载的单个资源链接
    #[arg(long, help_heading = "Mode")]
    pub url: Option<String>,
    /// 通过资源ID下载 (需配合 --type 使用)
    #[arg(long, help_heading = "Mode", requires = "type")]
    pub id: Option<String>,
    /// 从文本文件批量下载多个链接或ID (每行一个)
    #[arg(short, long, value_name = "FILE", help_heading = "Mode", requires = "type")]
    pub batch_file: Option<PathBuf>,
    /// 显示如何获取 Access Token 的指南并退出
    #[arg(long, action = clap::ArgAction::SetTrue, help_heading = "Mode")]
    pub token_help: bool,

    // --- 下载选项 (Options) ---
    /// [非交互模式] 指定下载项 (例如 '1-5,8', 'all')
    #[arg(long, default_value_t = constants::DEFAULT_SELECTION.to_string(), value_name = "SELECTION", help_heading = "Options")]
    pub select: String,
    /// [ID模式] 指定资源类型
    #[arg(long, value_enum, help_heading = "Options")] // 将类型改为 value_enum
    pub r#type: Option<ResourceType>, // 将类型从 String 改为 ResourceType
    /// 提供访问令牌 (Access Token)，优先级最高
    #[arg(long, help_heading = "Options")]
    pub token: Option<String>,
    /// 强制重新下载已存在的文件
    #[arg(short, long, action = clap::ArgAction::SetTrue, help_heading = "Options")]
    pub force_redownload: bool,
    /// 选择视频清晰度: 'best'(最高), 'worst'(最低), 或具体值 '720p' 等
    #[arg(short='q', long, default_value_t = constants::DEFAULT_VIDEO_QUALITY.to_string(), help_heading = "Options")]
    pub video_quality: String,
    /// [教材模式] 选择音频格式: 'mp3', 'm4a' 等
    #[arg(long, default_value_t = constants::DEFAULT_AUDIO_FORMAT.to_string(), help_heading = "Options")]
    pub audio_format: String,
    /// [批量模式] 为文件列表中的每个任务提供手动选择的机会
    #[arg(long, action = clap::ArgAction::SetTrue, help_heading = "Options")]
    pub prompt_each: bool,
    /// 将所有文件下载到输出目录的根路径，不创建额外的子目录
    #[arg(long, action = clap::ArgAction::SetTrue, help_heading = "Options")]
    pub flat: bool,
    /// 设置最大并发下载数
    #[arg(short, long, value_parser = clap::value_parser!(usize), help_heading = "Options")]
    pub workers: Option<usize>,
    /// 设置文件保存目录
    #[arg(short, long, value_name = "DIR", default_value_os_t = PathBuf::from(constants::DEFAULT_SAVE_DIR), help_heading = "Options")]
    pub output: PathBuf,

    // --- 通用选项 (General) ---
    /// 显示此帮助信息并退出
    #[arg(short = 'h', long, action = clap::ArgAction::Help, global = true, help_heading = "General")]
    _help: Option<bool>,
    /// 显示版本信息并退出
    #[arg(short = 'V', long, action = clap::ArgAction::Version, global = true, help_heading = "General")]
    _version: Option<bool>,
    /// (隐藏参数) 设置日志文件的输出级别，用于调试
    #[arg(long, value_enum, default_value_t = LogLevel::Off, global = true, hide = true)]
    pub log_level: LogLevel,
}
