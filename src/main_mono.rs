// --- 1. crate 导入与模块定义 ---
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyInit, KeyIvInit};
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, FixedOffset};
use clap::{command, CommandFactory, FromArgMatches, Parser, ValueEnum};
use colored::*;
use dashmap::DashMap;
use futures::{stream, StreamExt};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use log::{debug, error, info, trace, warn};
use md5::{Digest, Md5};
use regex::Regex;
use reqwest::{header, IntoUrl, Response, StatusCode};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{
    policies::ExponentialBackoff, DefaultRetryableStrategy, RetryTransientMiddleware,
};
use serde::{de::DeserializeOwned, Deserialize};
use std::{
    cmp::min,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    env,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::Duration,
};
use thiserror::Error;
use tokio::sync::Mutex as TokioMutex;
use url::Url;

// --- 2. 错误与结果类型定义 ---
#[derive(Error, Debug)]
pub enum AppError {
    #[error("认证失败 (Token 无效或已过期)")]
    TokenInvalid,
    #[error("网络请求失败: {0}")]
    Network(#[from] reqwest::Error),
    #[error("网络中间件错误: {0}")]
    NetworkMiddleware(#[from] reqwest_middleware::Error),
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON 解析错误: {0}")]
    Json(#[from] serde_json::Error),
    #[error("URL 解析错误: {0}")]
    Url(#[from] url::ParseError),
    #[error("Base64 解码错误: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("M3U8 解析错误: {0}")]
    M3u8Parse(String),
    #[error("视频分片合并失败: {0}")]
    Merge(String),
    #[error("文件校验失败: {0}")]
    Validation(String),
    #[error("安全错误: {0}")]
    Security(String),
    #[error("用户中断")]
    UserInterrupt,
    #[error("未知错误: {0}")]
    Other(#[from] anyhow::Error),
}

type AppResult<T> = Result<T, AppError>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

// --- 3. 常量与符号定义 ---
mod constants {
    pub const UI_WIDTH: usize = 88;
    pub const FILENAME_TRUNCATE_LENGTH: usize = 65;
    pub const MAX_FILENAME_BYTES: usize = 200;
    pub const CONFIG_DIR_NAME: &str = concat!(".", clap::crate_name!());
    pub const CONFIG_FILE_NAME: &str = "config.json";
    pub const LOG_FILE_NAME: &str = "app.log";
    pub const LOG_FALLBACK_FILE_NAME: &str = "fallback.log";
    pub const DEFAULT_SAVE_DIR: &str = "downloads";
    pub const UNCLASSIFIED_DIR: &str = "未分类资源";
    pub const DEFAULT_AUDIO_FORMAT: &str = "mp3";
    pub const DEFAULT_VIDEO_QUALITY: &str = "best";
    pub const DEFAULT_SELECTION: &str = "all";
    pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36";
    pub const HELP_TOKEN_GUIDE: &str = r#"
1. 登录平台: 使用 Chrome / Edge / Firefox 浏览器登录。
   (登录地址: https://auth.smartedu.cn/uias/login)
2. 打开开发者工具:
   - 在 Windows / Linux 上: 按 F12 或 Ctrl+Shift+I
   - 在 macOS 上: 按 Cmd+Opt+I (⌘⌥I)
3. 切换到“控制台” (Console) 标签页。
4. 复制并粘贴以下代码到控制台，然后按 Enter 运行：
----------------------------------------------
copy(
  JSON.parse(
    JSON.parse(
      localStorage.getItem(
        Object.keys(localStorage)
          .find(i => i.startsWith("ND_UC_AUTH"))
      )
    ).value
  ).access_token
)
----------------------------------------------
5. 此时 Token 已自动复制到剪贴板，可以直接粘贴使用。"#;
}

mod symbols {
    use super::*;
    pub static OK: LazyLock<ColoredString> = LazyLock::new(|| "[OK]".green());
    pub static ERROR: LazyLock<ColoredString> = LazyLock::new(|| "[X]".red());
    pub static INFO: LazyLock<ColoredString> = LazyLock::new(|| "[i]".cyan());
    pub static WARN: LazyLock<ColoredString> = LazyLock::new(|| "[!]".yellow());
    pub static CTRL_C: LazyLock<ColoredString> = LazyLock::new(|| "Ctrl+C".yellow());
}

mod api {
    pub mod types {
        pub const TCH_MATERIAL: &str = "tchMaterial";
        pub const QUALITY_COURSE: &str = "qualityCourse";
        pub const SYNC_CLASSROOM: &str = "syncClassroom/classActivity";
    }
    pub mod resource_formats {
        pub const PDF: &str = "pdf";
        pub const M3U8: &str = "m3u8";
    }
    pub mod resource_types {
        pub const ASSETS_VIDEO: &str = "assets_video";
        pub const ASSETS_DOCUMENT: &str = "assets_document";
        pub const COURSEWARES: &str = "coursewares";
        pub const LESSON_PLANDESIGN: &str = "lesson_plandesign";
    }
}

// --- 4. API 模型定义 (强类型) ---
mod models {
    pub mod api {
        use super::*;
        // --- 通用结构体 ---
        #[derive(Deserialize, Debug, Clone)]
        pub struct GlobalTitle {
            #[serde(rename = "zh-CN")]
            pub zh_cn: String,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct Requirement {
            pub name: String,
            pub value: String,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct TiItemCustomProperties {
            pub requirements: Option<Vec<Requirement>>,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct TiItem {
            pub ti_format: String,
            pub ti_storages: Option<Vec<String>>,
            pub ti_md5: Option<String>,
            pub ti_size: Option<u64>,
            #[serde(default)]
            pub ti_file_flag: Option<String>,
            pub custom_properties: Option<TiItemCustomProperties>,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct Tag {
            pub tag_dimension_id: String,
            pub tag_name: String,
        }

        // --- 课程 (Course) API 响应结构体 ---
        #[derive(Deserialize, Debug, Clone)]
        pub struct CourseDetailsResponse {
            pub global_title: GlobalTitle,
            pub tag_list: Option<Vec<Tag>>,
            pub custom_properties: CourseCustomProperties,
            pub chapter_paths: Option<Vec<String>>,
            pub teacher_list: Option<Vec<Teacher>>,
            pub relations: Relations,
            pub resource_structure: Option<ResourceStructure>,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct CourseCustomProperties {
            pub teachingmaterial_info: Option<TeachingMaterialInfo>,
            pub lesson_teacher_ids: Option<Vec<String>>,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct TeachingMaterialInfo {
            pub id: String,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct Teacher {
            pub id: String,
            pub name: String,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct Relations {
            #[serde(alias = "national_course_resource", alias = "course_resource")]
            pub resources: Option<Vec<CourseResource>>,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct CourseResource {
            pub global_title: GlobalTitle,
            pub custom_properties: CourseResourceCustomProperties,
            pub update_time: Option<DateTime<FixedOffset>>,
            pub resource_type_code: String,
            pub ti_items: Option<Vec<TiItem>>,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct CourseResourceCustomProperties {
            pub alias_name: Option<String>,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct ResourceStructure {
            pub relations: Option<Vec<ResourceStructureRelation>>,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct ResourceStructureRelation {
            // 移除了未使用的字段 title (或者可以改为 _title)
            // pub title: String,
            pub res_ref: Option<Vec<String>>,
            pub custom_properties: ResourceStructureRelationCustomProperties,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct ResourceStructureRelationCustomProperties {
            pub teacher_ids: Option<Vec<String>>,
        }

        // --- 教材 (Textbook) API 响应结构体 ---
        #[derive(Deserialize, Debug, Clone)]
        pub struct TextbookDetailsResponse {
            pub id: String,
            pub title: Option<String>,
            pub global_title: Option<GlobalTitle>,
            pub ti_items: Option<Vec<TiItem>>,
            pub tag_list: Option<Vec<Tag>>,
            pub update_time: Option<DateTime<FixedOffset>>,
        }

        #[derive(Deserialize, Debug, Clone)]
        pub struct AudioRelationItem {
            pub global_title: GlobalTitle,
            pub ti_items: Option<Vec<TiItem>>,
            pub update_time: Option<DateTime<FixedOffset>>,
        }
    }

    use super::*;
    #[derive(Debug, Clone, Deserialize)]
    pub struct FileInfo {
        pub filepath: PathBuf,
        pub url: String,
        pub ti_md5: Option<String>,
        pub ti_size: Option<u64>,
        pub date: Option<DateTime<FixedOffset>>,
    }

    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    pub enum DownloadStatus {
        Success, Skipped, Resumed, Md5Failed, SizeFailed, HttpError, NetworkError,
        ConnectionError, TimeoutError, TokenError, IoError, MergeError, KeyError, UnexpectedError,
    }

    impl From<&AppError> for DownloadStatus {
        fn from(error: &AppError) -> Self {
            match error {
                AppError::TokenInvalid => DownloadStatus::TokenError,
                AppError::Network(err) | AppError::NetworkMiddleware(reqwest_middleware::Error::Reqwest(err)) => {
                    if err.is_timeout() { DownloadStatus::TimeoutError }
                    else if err.is_connect() { DownloadStatus::ConnectionError }
                    else if err.is_status() { DownloadStatus::HttpError }
                    else { DownloadStatus::NetworkError }
                }
                AppError::NetworkMiddleware(_) => DownloadStatus::NetworkError,
                AppError::Io(_) => DownloadStatus::IoError,
                AppError::M3u8Parse(_) | AppError::Merge(_) => DownloadStatus::MergeError,
                AppError::Security(_) => DownloadStatus::KeyError,
                AppError::Validation(msg) => {
                    if msg.contains("MD5") { DownloadStatus::Md5Failed } else { DownloadStatus::SizeFailed }
                }
                _ => DownloadStatus::UnexpectedError,
            }
        }
    }

    #[derive(Debug, Clone)]
    pub struct DownloadResult {
        pub filename: String,
        pub status: DownloadStatus,
        pub message: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DownloadAction {
        Skip, Resume, DownloadNew,
    }

    pub struct TokenRetryResult {
        pub remaining_tasks: Option<Vec<FileInfo>>,
        pub should_abort: bool,
    }
}

// --- 5. 辅助函数与工具 ---
mod utils {
    use super::*;
    pub static UUID_PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[a-f0-9]{8}-([a-f0-9]{4}-){3}[a-f0-9]{12}$").unwrap());
    static ILLEGAL_CHARS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[\\/*?:"<>|]"#).unwrap());
    static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

    pub fn is_resource_id(text: &str) -> bool {
        UUID_PATTERN.is_match(text)
    }

    pub fn sanitize_filename(name: &str) -> String {
        let original_name = name.trim();
        if original_name.is_empty() { return "unknown".to_string(); }

        let stem = Path::new(original_name)
            .file_stem()
            .unwrap_or_else(|| OsStr::new(original_name))
            .to_string_lossy()
            .to_uppercase();
        let windows_reserved = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];

        let mut name = if windows_reserved.contains(&stem.as_ref()) {
            format!("_{}", original_name)
        } else {
            original_name.to_string()
        };

        name = ILLEGAL_CHARS_RE.replace_all(&name, " ").into_owned();
        name = WHITESPACE_RE.replace_all(&name, " ").trim().to_string();
        name = name.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
        if name.is_empty() { return "unnamed".to_string(); }

        if name.as_bytes().len() > constants::MAX_FILENAME_BYTES {
            if let (Some(stem_part), Some(ext)) = (Path::new(&name).file_stem(), Path::new(&name).extension()) {
                let stem_part_str = stem_part.to_string_lossy();
                let ext_str = format!(".{}", ext.to_string_lossy());
                let max_stem_bytes = constants::MAX_FILENAME_BYTES.saturating_sub(ext_str.as_bytes().len());
                let truncated_stem = safe_truncate_utf8(&stem_part_str, max_stem_bytes);
                name = format!("{}{}", truncated_stem, ext_str);
            } else {
                name = safe_truncate_utf8(&name, constants::MAX_FILENAME_BYTES).to_string();
            }
        }
        name
    }

    fn safe_truncate_utf8(s: &str, max_bytes: usize) -> &str {
        if s.len() <= max_bytes { return s; }
        let mut i = max_bytes;
        while i > 0 && !s.is_char_boundary(i) { i -= 1; }
        &s[..i]
    }

    pub fn truncate_text(text: &str, max_width: usize) -> String {
        let mut width = 0;
        let mut end_pos = 0;
        for (i, c) in text.char_indices() {
            width += if c.is_ascii() { 1 } else { 2 };
            if width > max_width.saturating_sub(3) {
                end_pos = i;
                break;
            }
        }
        if end_pos == 0 { text.to_string() } else { format!("{}...", &text[..end_pos]) }
    }

    pub fn parse_selection_indices(selection_str: &str, total_items: usize) -> Vec<usize> {
        if selection_str.to_lowercase() == "all" { return (0..total_items).collect(); }
        let mut indices = BTreeSet::new();
        for part in selection_str.split(',').map(|s| s.trim()) {
            if part.is_empty() { continue; }
            if let Some(range_part) = part.split_once('-') {
                if let (Ok(start), Ok(end)) = (range_part.0.parse::<usize>(), range_part.1.parse::<usize>()) {
                    if start == 0 || end == 0 { continue; }
                    let (min, max) = (start.min(end), start.max(end));
                    for i in min..=max {
                        if i > 0 && i <= total_items { indices.insert(i - 1); }
                    }
                }
            } else if let Ok(num) = part.parse::<usize>() {
                if num > 0 && num <= total_items { indices.insert(num - 1); }
            }
        }
        indices.into_iter().collect()
    }

    pub fn calculate_file_md5(path: &Path) -> AppResult<String> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut hasher = Md5::new();
        let mut buffer = [0; 8192];
        loop {
            let bytes_read = reader.read(&mut buffer)?;
            if bytes_read == 0 { break; }
            hasher.update(&buffer[..bytes_read]);
        }
        let result = hasher.finalize();
        Ok(format!("{:x}", result))
    }

    pub fn secure_join_path(base_dir: &Path, relative_path: &Path) -> AppResult<PathBuf> {
        let resolved_base = dunce::canonicalize(base_dir).with_context(|| format!("基础目录 '{:?}' 不存在或无法访问", base_dir))?;
        let mut final_path = resolved_base.clone();
        for component in relative_path.components() {
            match component {
                Component::Normal(part) => final_path.push(part),
                Component::ParentDir => return Err(AppError::Security("检测到路径遍历 '..' ".to_string())),
                _ => continue,
            }
        }
        if !final_path.starts_with(&resolved_base) {
            return Err(AppError::Security(format!("路径遍历攻击检测: '{:?}'", relative_path)));
        }
        Ok(final_path)
    }
}

// --- 6. UI 与交互 ---
mod ui {
    use super::*;
    pub fn print_header(title: &str) {
        println!("\n{}", "═".repeat(constants::UI_WIDTH));
        println!(" {}", title.cyan().bold());
        println!("{}", "═".repeat(constants::UI_WIDTH));
    }

    pub fn print_sub_header(title: &str) {
        println!("\n--- {} ---", title.bold());
    }

    pub fn box_message(title: &str, content: &[&str], color_func: fn(ColoredString) -> ColoredString) {
        println!("\n┌{}┐", "─".repeat(constants::UI_WIDTH - 2));
        println!("  {}", color_func(title.bold()));
        println!("├{}┤", "─".repeat(constants::UI_WIDTH - 2));
        for line in content {
            println!("  {}", line);
        }
        println!("└{}┘", "─".repeat(constants::UI_WIDTH - 2));
    }

    pub fn prompt(message: &str, default: Option<&str>) -> io::Result<String> {
        let default_str = default.map_or("".to_string(), |d| format!(" (默认: {})", d));
        print!("\n>>> {}{}: ", message, default_str);
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim().to_string();
        if input.is_empty() {
            Ok(default.unwrap_or("").to_string())
        } else {
            Ok(input)
        }
    }

    pub fn confirm(question: &str, default_yes: bool) -> bool {
        let options = if default_yes { "(Y/n)" } else { "(y/N)" };
        loop {
            match prompt(&format!("{} {} (按 {} 取消)", question, options, *symbols::CTRL_C), None) {
                Ok(choice) => {
                    let choice = choice.to_lowercase();
                    if choice == "y" { return true; }
                    if choice == "n" { return false; }
                    if choice.is_empty() { return default_yes; }
                    println!("{}", "无效输入，请输入 'y' 或 'n'。".red());
                }
                Err(_) => return false,
            }
        }
    }

    pub fn selection_menu(
        options: &[String], title: &str, instructions: &str, default_choice: &str,
    ) -> String {
        println!("\n┌{}┐", "─".repeat(constants::UI_WIDTH - 2));
        println!("  {}", title.cyan().bold());
        println!("├{}┤", "─".repeat(constants::UI_WIDTH - 2));
        let pad = options.len().to_string().len();
        for (i, option) in options.iter().enumerate() {
            println!("  [{}] {}", format!("{:<pad$}", i + 1, pad = pad).yellow(), option);
        }
        println!("├{}┤", "─".repeat(constants::UI_WIDTH - 2));
        println!("  {} (按 {} 可取消)", instructions, *symbols::CTRL_C);
        println!("└{}┘", "─".repeat(constants::UI_WIDTH - 2));
        prompt("请输入你的选择", Some(default_choice)).unwrap_or_default()
    }

    pub fn prompt_hidden(message: &str) -> io::Result<String> {
        print!("\n>>> {}: ", message);
        io::stdout().flush()?;
        rpassword::read_password()
    }
    
    pub fn get_user_choices_from_menu(
        options: &[String], title: &str, default_choice: &str,
    ) -> Vec<String> {
        if options.is_empty() { return vec![]; }
        let user_input = selection_menu(options, title, "支持格式: 1, 3, 2-4, all", default_choice);
        utils::parse_selection_indices(&user_input, options.len())
            .into_iter().map(|i| options[i].clone()).collect()
    }
}

// --- 7. 配置与 Token 管理 ---
mod config {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    struct LocalConfig {
        accesstoken: Option<String>,
    }

    fn get_config_path() -> AppResult<PathBuf> {
        let path = dirs::home_dir()
            .ok_or_else(|| AppError::Other(anyhow!("无法获取用户主目录")))?
            .join(constants::CONFIG_DIR_NAME)
            .join(constants::CONFIG_FILE_NAME);
        Ok(path)
    }

    pub fn save_token(token: &str) -> AppResult<()> {
        if token.is_empty() { return Ok(()); }
        let config_path = get_config_path()?;
        let config_dir = config_path.parent().ok_or_else(|| anyhow!("无法获取配置文件的父目录"))?;
        fs::create_dir_all(config_dir).with_context(|| format!("创建配置目录 '{}' 失败", config_dir.display()))?;

        let mut config: LocalConfig = fs::read_to_string(&config_path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or(LocalConfig { accesstoken: None });
        config.accesstoken = Some(token.to_string());
        let json_content = serde_json::to_string_pretty(&config).context("序列化Token配置失败")?;
        fs::write(&config_path, json_content).with_context(|| format!("保存Token到 '{}' 失败", config_path.display()))?;
        info!("用户已将 Token 保存至配置文件: {}", config_path.display());
        println!("{} Token已成功保存至: {}", *symbols::INFO, config_path.display());
        Ok(())
    }

    pub fn load_token_from_config() -> Option<String> {
        let config_path = get_config_path().ok()?;
        if !config_path.is_file() {
            debug!("配置文件 {:?} 不存在", config_path);
            return None;
        }
        fs::read_to_string(&config_path)
            .ok()
            .and_then(|content| serde_json::from_str::<LocalConfig>(&content).ok())
            .and_then(|config| config.accesstoken)
    }

    pub fn resolve_token(cli_token: Option<&str>) -> (Option<String>, String) {
        if let Some(token) = cli_token {
            if !token.is_empty() {
                debug!("使用来自命令行参数的 Token");
                return (Some(token.to_string()), "命令行参数".to_string());
            }
        }
        if let Ok(token) = env::var("ACCESS_TOKEN") {
            if !token.is_empty() {
                debug!("使用来自环境变量 ACCESS_TOKEN 的 Token");
                return (Some(token), "环境变量 (ACCESS_TOKEN)".to_string());
            }
        }
        if let Some(token) = load_token_from_config() {
            if !token.is_empty() {
                debug!("使用来自本地配置文件的 Token");
                return (Some(token), "本地Token文件".to_string());
            }
        }
        debug!("未在任何位置找到可用的 Token");
        (None, "未找到".to_string())
    }
}

// --- 8. 核心数据结构与类型 ---
/// 全局应用配置
#[derive(Debug, Clone)]
pub struct AppConfig {
    max_workers: usize,
    default_audio_format: String,
    server_prefixes: Vec<String>,
    user_agent: String,
    connect_timeout: Duration,
    timeout: Duration,
    max_retries: u32,
    api_endpoints: HashMap<String, ApiEndpointConfig>,
    url_templates: HashMap<String, String>,
}

impl AppConfig {
    fn from_args(args: &Cli) -> Self {
        let default_config = Self::default();
        AppConfig {
            max_workers: args.workers.unwrap_or(default_config.max_workers),
            default_audio_format: args.audio_format.clone(),
            ..default_config
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        let url_templates = HashMap::from([
            ("TEXTBOOK_DETAILS".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/resources/tch_material/details/{resource_id}.json".into()),
            ("TEXTBOOK_AUDIO".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrs/resources/{resource_id}/relation_audios.json".into()),
            ("COURSE_QUALITY".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/resources/{resource_id}.json".into()),
            ("COURSE_SYNC".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/national_lesson/resources/details/{resource_id}.json".into()),
            ("CHAPTER_TREE".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/national_lesson/trees/{tree_id}.json".into()),
        ]);

        let api_endpoints = HashMap::from([
            (
                api::types::TCH_MATERIAL.into(),
                ApiEndpointConfig {
                    id_param: "contentId".into(),
                    extractor: ResourceExtractorType::Textbook,
                    url_template_keys: HashMap::from([("textbook".into(), "TEXTBOOK_DETAILS".into()), ("audio".into(), "TEXTBOOK_AUDIO".into())]),
                },
            ),
            (
                api::types::QUALITY_COURSE.into(),
                ApiEndpointConfig {
                    id_param: "courseId".into(),
                    extractor: ResourceExtractorType::Course,
                    url_template_keys: HashMap::from([("main".into(), "COURSE_QUALITY".into())]),
                },
            ),
            (
                api::types::SYNC_CLASSROOM.into(),
                ApiEndpointConfig {
                    id_param: "activityId".into(),
                    extractor: ResourceExtractorType::Course,
                    url_template_keys: HashMap::from([("main".into(), "COURSE_SYNC".into())]),
                },
            ),
        ]);

        Self {
            max_workers: 5,
            default_audio_format: constants::DEFAULT_AUDIO_FORMAT.into(),
            server_prefixes: vec!["s-file-1".into(), "s-file-2".into(), "s-file-3".into()],
            user_agent: constants::USER_AGENT.into(),
            connect_timeout: Duration::from_secs(5),
            timeout: Duration::from_secs(15),
            max_retries: 3,
            api_endpoints,
            url_templates,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiEndpointConfig {
    id_param: String,
    extractor: ResourceExtractorType,
    url_template_keys: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
pub enum ResourceExtractorType {
    Textbook,
    Course,
}

/// 下载任务上下文
#[derive(Clone)]
struct DownloadJobContext {
    manager: downloader::DownloadManager,
    token: Arc<TokioMutex<String>>,
    config: Arc<AppConfig>,
    http_client: Arc<RobustClient>,
    args: Arc<Cli>,
    non_interactive: bool,
    cancellation_token: Arc<AtomicBool>,
}

// --- 9. HTTP 客户端 ---
#[derive(Clone)]
pub struct RobustClient {
    client: ClientWithMiddleware,
    config: Arc<AppConfig>,
}

impl RobustClient {
    pub fn new(config: Arc<AppConfig>) -> AppResult<Self> {
        let retry_policy = ExponentialBackoff::builder().build_with_max_retries(config.max_retries);
        let client = ClientBuilder::new(
            reqwest::Client::builder()
                .user_agent(config.user_agent.clone())
                .connect_timeout(config.connect_timeout)
                .timeout(config.timeout)
                .pool_max_idle_per_host(config.max_workers * 3)
                .build()?,
        )
        .with(RetryTransientMiddleware::new_with_policy_and_strategy(
            retry_policy,
            DefaultRetryableStrategy,
        ))
        .build();
        debug!("RobustClient created with max_retries={}", config.max_retries);
        Ok(Self { client, config })
    }

    pub async fn get<T: IntoUrl>(&self, url: T) -> AppResult<Response> {
        let url_str = url.as_str().to_owned();
        debug!("HTTP GET: {}", url_str);
        let res = self.client.get(url_str).send().await?;
        let status = res.status();

        if status.is_success() {
            Ok(res)
        } else {
            warn!("HTTP request to {} resulted in status code: {}", res.url(), status);
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                warn!("Status code indicates invalid token.");
                Err(AppError::TokenInvalid)
            } else {
                let err = res.error_for_status().unwrap_err();
                Err(AppError::Network(err))
            }
        }
    }

    pub async fn fetch_json<T: DeserializeOwned>(
        &self, url_template: &str, params: &[(&str, &str)],
    ) -> AppResult<T> {
        let mut last_error = None;
        for prefix in &self.config.server_prefixes {
            let mut url = url_template.replace("{prefix}", prefix);
            for (key, val) in params {
                url = url.replace(&format!("{{{}}}", key), val);
            }
            match self.get(&url).await {
                Ok(res) => {
                    let text = res.text().await?;
                    trace!("Raw JSON response from {}: {}", url, text);
                    return serde_json::from_str(&text).map_err(AppError::from);
                }
                Err(e) => {
                    warn!("服务器 '{}' 请求失败: {:?}", prefix, e);
                    last_error = Some(e);
                }
            }
        }
        error!("所有服务器均请求失败 for template: {}", url_template);
        match last_error {
            Some(err) => Err(err),
            None => Err(AppError::Other(anyhow!("所有服务器均请求失败，且没有配置服务器前缀"))),
        }
    }
}

// --- 10. 资源提取器 ---
mod extractor {
    use super::{models::{api::*, FileInfo}, *};
    use percent_encoding;

    #[async_trait]
    pub trait ResourceExtractor: Send + Sync {
        async fn extract_file_info(
            &self, resource_id: &str, context: &DownloadJobContext,
        ) -> AppResult<Vec<FileInfo>>;
    }
    
    static REF_INDEX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([\d,*]+)\]$").unwrap());

    pub struct TextbookExtractor {
        http_client: Arc<RobustClient>,
        config: Arc<AppConfig>,
    }
    impl TextbookExtractor {
        pub fn new(http_client: Arc<RobustClient>, config: Arc<AppConfig>) -> Self {
            Self { http_client, config }
        }

        fn extract_pdf_info(&self, data: &TextbookDetailsResponse) -> (Vec<FileInfo>, Option<String>) {
            let base_path = self.build_resource_path(data.tag_list.as_deref());
            let results: Vec<FileInfo> = data.ti_items.as_deref().unwrap_or_default()
                .iter()
                .filter_map(|item| {
                    if item.ti_file_flag.as_deref() != Some("source") || item.ti_format != api::resource_formats::PDF {
                        return None;
                    }
                    let url_str = item.ti_storages.as_ref()?.get(0)?;
                    let url = Url::parse(url_str).ok()?;
                    let raw_filename = Path::new(url.path()).file_name()?.to_str()?;
                    let decoded_filename = percent_encoding::percent_decode(raw_filename.as_bytes()).decode_utf8_lossy().to_string();
                    let name = if self.is_generic_filename(&decoded_filename) {
                        let title = data.global_title.as_ref().map(|t| t.zh_cn.as_str())
                            .or(data.title.as_deref())
                            .unwrap_or(&data.id);
                        format!("{}.pdf", utils::sanitize_filename(title))
                    } else {
                        utils::sanitize_filename(&decoded_filename)
                    };

                    debug!("提取到PDF文件: '{}' @ '{}'", name, url_str);
                    Some(FileInfo {
                        filepath: base_path.join(&name), url: url_str.clone(),
                        ti_md5: item.ti_md5.clone(), ti_size: item.ti_size, date: data.update_time,
                    })
                })
                .collect();
            let textbook_basename = results.first()
                .and_then(|fi| Path::new(&fi.filepath).file_stem())
                .map(|s| s.to_string_lossy().to_string());
            (results, textbook_basename)
        }

        fn is_generic_filename(&self, filename: &str) -> bool {
            let patterns = [r"^pdf\.pdf$", r"^document\.pdf$", r"^file\.pdf$", r"^\d+\.pdf$", r"^[a-f0-9]{32}\.pdf$"];
            patterns.iter().any(|p| Regex::new(p).unwrap().is_match(filename.to_lowercase().as_str()))
        }

        async fn extract_audio_info(
            &self, resource_id: &str, base_path: PathBuf, textbook_basename: Option<String>, context: &DownloadJobContext,
        ) -> AppResult<Vec<FileInfo>> {
            let url_template = self.config.url_templates.get("TEXTBOOK_AUDIO").unwrap();
            let audio_items: Vec<AudioRelationItem> = self.http_client.fetch_json(url_template, &[("resource_id", resource_id)]).await?;

            if audio_items.is_empty() {
                info!("未找到与教材 '{}' 关联的音频文件。", resource_id);
                return Ok(vec![]);
            }
            let available_formats = self.get_available_audio_formats(&audio_items);
            if available_formats.is_empty() { return Ok(vec![]); }
            debug!("可用音频格式: {:?}", available_formats);
            
            let selected_formats: Vec<String> = if context.non_interactive || &context.args.audio_format != constants::DEFAULT_AUDIO_FORMAT {
                let preferred = context.config.default_audio_format.to_lowercase();
                let chosen_format = if available_formats.contains(&preferred) {
                    preferred
                } else {
                    println!("{} 首选音频格式 '{}' 不可用，将自动选择 '{}'。", *symbols::WARN, preferred, available_formats[0]);
                    warn!("首选音频格式 '{}' 不可用, 将选择第一个可用格式 '{}'", preferred, available_formats[0]);
                    available_formats[0].clone()
                };
                vec![chosen_format]
            } else {
                let options: Vec<String> = available_formats.iter().map(|f| f.to_uppercase()).collect();
                ui::get_user_choices_from_menu(&options, "选择音频格式", "1")
            };
            info!("已选择音频格式: {:?}", selected_formats);
            
            if selected_formats.is_empty() {
                println!("{} 未选择任何音频格式，跳过音频下载。", *symbols::INFO);
                return Ok(vec![]);
            }
            
            let audio_path = textbook_basename.map(|b| base_path.join(format!("{} - [audio]", b))).unwrap_or(base_path);
            debug!("音频文件将保存至: {:?}", audio_path);

            let results = audio_items.iter().enumerate().flat_map(|(i, item)| {
                    let title = &item.global_title.zh_cn;
                    let base_name = format!("{:03} {}", i + 1, utils::sanitize_filename(title));
                    let audio_path_clone = audio_path.clone();
                    
                    selected_formats.iter().filter_map(move |format| {
                        let format_lower = format.to_lowercase();
                        let ti = item.ti_items.as_ref()?
                            .iter().find(|ti| ti.ti_format == format_lower)?;
                        let url = ti.ti_storages.as_ref()?.get(0)?;
                        debug!("提取到音频文件: '{}.{}' @ '{}'", base_name, &format_lower, url);
                        Some(FileInfo {
                            filepath: audio_path_clone.join(format!("{}.{}", base_name, &format_lower)),
                            url: url.clone(), ti_md5: ti.ti_md5.clone(),
                            ti_size: ti.ti_size, date: item.update_time,
                        })
                    })
                }).collect();
            Ok(results)
        }

        fn get_available_audio_formats(&self, data: &[AudioRelationItem]) -> Vec<String> {
            let mut formats = HashSet::new();
            for item in data {
                if let Some(ti_items) = item.ti_items.as_ref() {
                    for ti in ti_items {
                        formats.insert(ti.ti_format.clone());
                    }
                }
            }
            let mut sorted_formats: Vec<String> = formats.into_iter().collect();
            sorted_formats.sort();
            sorted_formats
        }

        pub(super) fn build_resource_path(&self, tag_list_val: Option<&[Tag]>) -> PathBuf {
            let template: HashMap<&str, &str> = [("zxxxd", "未知学段"), ("zxxnj", "未知年级"), ("zxxxk", "未知学科"), ("zxxbb", "未知版本"), ("zxxcc", "未知册")].iter().cloned().collect();
            let mut path_map = template.clone();
            if let Some(tags) = tag_list_val {
                for tag in tags {
                    if path_map.contains_key(tag.tag_dimension_id.as_str()) {
                        path_map.insert(&tag.tag_dimension_id, &tag.tag_name);
                    }
                }
            }
            let default_values: HashSet<&str> = template.values().cloned().collect();
            let components: Vec<String> = ["zxxxd", "zxxnj", "zxxxk", "zxxbb", "zxxcc"].iter()
                .filter_map(|&key| path_map.get(key))
                .filter(|&&val| !default_values.contains(val))
                .map(|&name| utils::sanitize_filename(name))
                .collect();

            if components.is_empty() {
                debug!("无法从标签构建分类路径，使用默认未分类目录");
                PathBuf::from(constants::UNCLASSIFIED_DIR)
            } else {
                let path: PathBuf = components.iter().collect();
                debug!("从标签构建的分类路径: {:?}", path);
                path
            }
        }
    }

    #[async_trait]
    impl ResourceExtractor for TextbookExtractor {
        async fn extract_file_info(&self, resource_id: &str, context: &DownloadJobContext) -> AppResult<Vec<FileInfo>> {
            info!("开始提取教材资源, ID: {}", resource_id);
            let url_template = self.config.url_templates.get("TEXTBOOK_DETAILS").unwrap();
            let data: TextbookDetailsResponse = self.http_client.fetch_json(url_template, &[("resource_id", resource_id)]).await?;
            let (mut pdf_files, textbook_basename) = self.extract_pdf_info(&data);
            let base_path = self.build_resource_path(data.tag_list.as_deref());
            let audio_files = self.extract_audio_info(resource_id, base_path, textbook_basename, context).await?;
            pdf_files.extend(audio_files);
            info!("为教材 '{}' 提取到 {} 个文件", resource_id, pdf_files.len());
            Ok(pdf_files)
        }
    }

    pub struct CourseExtractor {
        http_client: Arc<RobustClient>,
        config: Arc<AppConfig>,
        chapter_resolver: ChapterTreeResolver,
        url_template: String,
    }

    #[derive(Debug)]
    struct VideoStream<'a> { resolution: String, ti_item: &'a TiItem }

    impl CourseExtractor {
        pub fn new(http_client: Arc<RobustClient>, config: Arc<AppConfig>, url_template: String) -> Self {
            let chapter_resolver = ChapterTreeResolver::new(http_client.clone(), config.clone());
            Self { http_client, config, chapter_resolver, url_template }
        }

        async fn get_base_directory(&self, data: &CourseDetailsResponse) -> PathBuf {
            let course_title = &data.global_title.zh_cn;
            let textbook_path = TextbookExtractor::new(self.http_client.clone(), self.config.clone()).build_resource_path(data.tag_list.as_deref());
            let mut full_chapter_path = PathBuf::new();
            if let Some(tm_info) = &data.custom_properties.teachingmaterial_info {
                if let Some(path_str) = data.chapter_paths.as_ref().and_then(|p| p.get(0)) {
                     if let Ok(path) = self.chapter_resolver.get_full_chapter_path(&tm_info.id, path_str).await {
                        full_chapter_path = path;
                    }
                }
            }
            let course_title_sanitized = utils::sanitize_filename(course_title);
            let parent_path = if full_chapter_path.file_name().and_then(|s| s.to_str()) == Some(&course_title_sanitized) {
                full_chapter_path.parent().unwrap_or(&full_chapter_path).to_path_buf()
            } else {
                full_chapter_path
            };
            let final_path = textbook_path.join(parent_path).join(course_title_sanitized);
            debug!("课程 '{}' 的基础目录解析为: {:?}", course_title, final_path);
            final_path
        }
        
        fn parse_res_ref_indices(&self, ref_str: &str, total_resources: usize) -> Option<Vec<usize>> {
            REF_INDEX_RE.captures(ref_str).and_then(|caps| {
                caps.get(1).map(|m| {
                    if m.as_str() == "*" { (0..total_resources).collect() }
                    else { m.as_str().split(',').filter_map(|s| s.parse::<usize>().ok()).collect() }
                })
            })
        }
        
        fn get_teacher_map(&self, data: &CourseDetailsResponse) -> HashMap<usize, String> {
            let teacher_id_map: HashMap<_, _> = data.teacher_list.as_deref().unwrap_or_default().iter().map(|t| (t.id.as_str(), t.name.as_str())).collect();
            trace!("教师ID映射: {:?}", teacher_id_map);
            let ids_to_names_str = |ids: &[String]| -> String {
                let names: Vec<_> = ids.iter().filter_map(|id| teacher_id_map.get(id.as_str()).cloned()).collect();
                if names.is_empty() { constants::UNCLASSIFIED_DIR.to_string() } else { names.join(", ") }
            };
            let mut resource_teacher_map = HashMap::new();
            let total_resources = data.relations.resources.as_ref().map_or(0, |r| r.len());

            if let Some(relations) = data.resource_structure.as_ref().and_then(|s| s.relations.as_ref()) {
                for relation in relations {
                    if let (Some(teacher_ids), Some(refs)) = (&relation.custom_properties.teacher_ids, &relation.res_ref) {
                        let teacher_str = ids_to_names_str(teacher_ids);
                        let indices: Vec<usize> = refs.iter().filter_map(|r| self.parse_res_ref_indices(r, total_resources)).flatten().collect();
                        if !indices.is_empty() {
                            for index in indices { resource_teacher_map.insert(index, teacher_str.clone()); }
                        }
                    }
                }
                if !resource_teacher_map.is_empty() {
                    debug!("从 resource_structure 成功映射教师信息"); return resource_teacher_map;
                }
            }
            
            if let Some(top_level_teacher_ids) = data.custom_properties.lesson_teacher_ids.as_deref() {
                if !top_level_teacher_ids.is_empty() {
                    let teacher_str = ids_to_names_str(top_level_teacher_ids);
                    for i in 0..total_resources { resource_teacher_map.insert(i, teacher_str.clone()); }
                    debug!("从顶层 custom_properties 成功映射教师信息"); return resource_teacher_map;
                }
            }
            warn!("未能在 API 响应中找到明确的教师与资源关联信息");
            resource_teacher_map
        }

        async fn negotiate_video_qualities(&self, resources: &[CourseResource], context: &DownloadJobContext) -> Option<Vec<String>> {
            let mut all_resolutions = BTreeSet::new();
            for res in resources {
                if let Some(streams) = self.get_available_video_streams(res.ti_items.as_deref()) {
                    for stream in streams { all_resolutions.insert(stream.resolution); }
                }
            }
            if all_resolutions.is_empty() { info!("课程中未找到任何视频流。"); return None; }
            let mut sorted_resolutions: Vec<String> = all_resolutions.into_iter().collect();
            sorted_resolutions.sort_by_key(|r| r.replace('p', "").parse::<u32>().unwrap_or(0));
            sorted_resolutions.reverse();
            debug!("课程中所有可用清晰度: {:?}", sorted_resolutions);
            if context.non_interactive || &context.args.video_quality != constants::DEFAULT_VIDEO_QUALITY {
                info!("非交互模式或已指定清晰度，选择: '{}'", &context.args.video_quality);
                return Some(vec![context.args.video_quality.clone()]);
            }
            let selected = ui::get_user_choices_from_menu(&sorted_resolutions, "选择视频清晰度", constants::DEFAULT_SELECTION);
            if selected.is_empty() { None } else { info!("用户选择的清晰度: {:?}", selected); Some(selected) }
        }

        fn get_available_video_streams<'a>(&self, ti_items: Option<&'a [TiItem]>) -> Option<Vec<VideoStream<'a>>> {
            let streams = ti_items?.iter()
                .filter_map(|item| {
                    if item.ti_format != api::resource_formats::M3U8 { return None; }
                    let height_str = item.custom_properties.as_ref()?.requirements.as_deref().unwrap_or_default()
                        .iter().find(|req| req.name == "Height")?.value.as_str();
                    let height = height_str.parse::<u32>().ok()?;
                    Some(VideoStream { resolution: format!("{}p", height), ti_item: item })
                }).collect::<Vec<_>>();
            if streams.is_empty() { None } else { Some(streams) }
        }

        fn select_stream_non_interactive<'a>(&self, streams: &'a [VideoStream], quality: &str) -> Option<(&'a TiItem, &'a str)> {
            if streams.is_empty() { return None; }
            if quality == "best" { return Some((streams[0].ti_item, &streams[0].resolution)); }
            if quality == "worst" { let worst = streams.last().unwrap(); return Some((worst.ti_item, &worst.resolution)); }
            for stream in streams { if stream.resolution == quality { return Some((stream.ti_item, &stream.resolution)); } }
            warn!("未找到指定清晰度 '{}'，将自动选择最高清晰度 '{}'。", quality, streams[0].resolution);
            eprintln!("{} 未找到指定清晰度 '{}'，将自动选择最高清晰度 '{}'。", *symbols::WARN, quality, streams[0].resolution);
            Some((streams[0].ti_item, &streams[0].resolution))
        }

        fn find_best_document_item<'a>(&self, ti_items: Option<&'a [TiItem]>) -> Option<&'a TiItem> {
            ti_items?.iter().find(|i| i.ti_format == api::resource_formats::PDF).or_else(|| ti_items?.iter().find(|i| i.ti_storages.is_some()))
        }

        fn process_video_resource(&self, resource: &CourseResource, title: &str, type_name: &str, teacher: &str, base_dir: &Path, selected_qualities: &Option<Vec<String>>) -> Vec<FileInfo> {
            let Some(qualities) = selected_qualities else { return vec![] };
            let Some(mut streams) = self.get_available_video_streams(resource.ti_items.as_deref()) else { return vec![] };
            streams.sort_by_key(|s| s.resolution.replace('p', "").parse::<u32>().unwrap_or(0)); streams.reverse();
            qualities.iter().filter_map(|quality| {
                let (ti_item, actual_quality) = self.select_stream_non_interactive(&streams, quality)?;
                let url = ti_item.ti_storages.as_ref()?.get(0)?;
                let base_name = format!("{} - {} [{}]", title, type_name, actual_quality);
                let filename = format!("{} - [{}].ts", base_name, teacher);
                Some(FileInfo { filepath: base_dir.join(filename), url: url.clone(), ti_md5: ti_item.ti_md5.clone(), ti_size: ti_item.ti_size, date: resource.update_time })
            }).collect()
        }

        fn process_document_resource(&self, resource: &CourseResource, title: &str, type_name: &str, teacher: &str, base_dir: &Path) -> Option<FileInfo> {
            let ti_item = self.find_best_document_item(resource.ti_items.as_deref())?;
            let url = ti_item.ti_storages.as_ref()?.get(0)?;
            let ext = &ti_item.ti_format;
            let filename = format!("{} - {} - [{}].{}", title, type_name, teacher, ext);
            Some(FileInfo { filepath: base_dir.join(filename), url: url.clone(), ti_md5: ti_item.ti_md5.clone(), ti_size: ti_item.ti_size, date: resource.update_time })
        }
    }

    #[async_trait]
    impl ResourceExtractor for CourseExtractor {
        async fn extract_file_info(&self, resource_id: &str, context: &DownloadJobContext) -> AppResult<Vec<FileInfo>> {
            info!("开始提取课程资源, ID: {}", resource_id);
            let data: CourseDetailsResponse = self.http_client.fetch_json(&self.url_template, &[("resource_id", resource_id)]).await?;
            let base_dir = self.get_base_directory(&data).await;
            let teacher_map = self.get_teacher_map(&data);
            let all_resources = data.relations.resources.unwrap_or_default();
            if all_resources.is_empty() {
                info!("课程 '{}' 下未找到任何资源。", resource_id);
                println!("{} 未在该课程下找到任何资源。", *symbols::WARN);
                return Ok(vec![]);
            }
            debug!("找到 {} 个相关资源。", all_resources.len());
            let user_selected_qualities = self.negotiate_video_qualities(&all_resources, context).await;
            let results: Vec<FileInfo> = all_resources.iter().enumerate().flat_map(|(index, resource)| {
                    let title = utils::sanitize_filename(&resource.global_title.zh_cn);
                    let type_name = utils::sanitize_filename(resource.custom_properties.alias_name.as_deref().unwrap_or(""));
                    let teacher = teacher_map.get(&index).cloned().unwrap_or_else(|| constants::UNCLASSIFIED_DIR.to_string());
                    let mut files = Vec::new();
                    if resource.resource_type_code == api::resource_types::ASSETS_VIDEO {
                        files.extend(self.process_video_resource(resource, &title, &type_name, &teacher, &base_dir, &user_selected_qualities));
                    } else if [api::resource_types::ASSETS_DOCUMENT, api::resource_types::COURSEWARES, api::resource_types::LESSON_PLANDESIGN].contains(&resource.resource_type_code.as_str()) {
                        if let Some(file_info) = self.process_document_resource(resource, &title, &type_name, &teacher, &base_dir) {
                            files.push(file_info);
                        }
                    }
                    files
                }).collect();
            info!("为课程 '{}' 提取到 {} 个文件", resource_id, results.len());
            Ok(results)
        }
    }

    /// 章节树解析器，带缓存
    pub struct ChapterTreeResolver {
        http_client: Arc<RobustClient>,
        config: Arc<AppConfig>,
        cache: DashMap<String, serde_json::Value>,
    }

    impl ChapterTreeResolver {
        pub fn new(http_client: Arc<RobustClient>, config: Arc<AppConfig>) -> Self {
            Self { http_client, config, cache: DashMap::new() }
        }

        async fn get_tree_data(&self, tree_id: &str) -> AppResult<serde_json::Value> {
            if let Some(entry) = self.cache.get(tree_id) {
                debug!("章节树缓存命中: {}", tree_id);
                return Ok(entry.value().clone());
            }
            debug!("章节树缓存未命中，从网络获取: {}", tree_id);
            let url_template = self.config.url_templates.get("CHAPTER_TREE").unwrap();
            let data: serde_json::Value = self.http_client.fetch_json(url_template, &[("tree_id", tree_id)]).await?;
            self.cache.insert(tree_id.to_string(), data.clone());
            Ok(data)
        }

        pub async fn get_full_chapter_path(&self, tree_id: &str, chapter_path_str: &str) -> AppResult<PathBuf> {
            let tree_data = self.get_tree_data(tree_id).await?;
            let lesson_node_id = chapter_path_str.split('/').last().unwrap_or("");
            debug!("在树 '{}' 中查找节点 '{}' 的完整路径", tree_id, lesson_node_id);
            let nodes_to_search =
                if let Some(nodes) = tree_data.get("child_nodes").and_then(|v| v.as_array()) { nodes }
                else if let Some(nodes) = tree_data.as_array() { nodes }
                else { warn!("章节树 '{}' 结构未知或为空", tree_id); return Ok(PathBuf::new()); };

            if let Some(path) = self.find_path_in_tree(nodes_to_search, lesson_node_id, vec![]) {
                let path_buf: PathBuf = path.iter().collect();
                debug!("找到完整章节路径: {:?}", path_buf);
                Ok(path_buf)
            } else {
                warn!("在树 '{}' 中未找到节点 '{}' 的路径", tree_id, lesson_node_id);
                Ok(PathBuf::new())
            }
        }

        fn find_path_in_tree<'a>(&self, nodes: &'a [serde_json::Value], target_id: &str, current_path: Vec<String>) -> Option<Vec<String>> {
            for node in nodes {
                let title = node.get("title").and_then(|v| v.as_str()).unwrap_or("未知章节");
                let mut new_path = current_path.clone();
                new_path.push(utils::sanitize_filename(title));
                if node.get("id").and_then(|v| v.as_str()) == Some(target_id) {
                    return Some(new_path);
                }
                if let Some(child_nodes) = node.get("child_nodes").and_then(|v| v.as_array()) {
                    if let Some(found_path) = self.find_path_in_tree(child_nodes, target_id, new_path) {
                        return Some(found_path);
                    }
                }
            }
            None
        }
    }
}

// --- 11. 下载管理与执行 ---
mod downloader {
    use super::{models::{DownloadAction, DownloadResult, DownloadStatus, FileInfo, TokenRetryResult}, *};

    #[derive(Clone, Default)]
    pub struct DownloadStats {
        total: usize, success: usize, skipped: usize, failed: usize,
    }

    #[derive(Clone)]
    pub struct DownloadManager {
        stats: Arc<Mutex<DownloadStats>>,
        failed_downloads: Arc<Mutex<Vec<(String, String)>>>,
        skipped_downloads: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl DownloadManager {
        pub fn new() -> Self {
            Self {
                stats: Arc::new(Mutex::new(DownloadStats::default())),
                failed_downloads: Arc::new(Mutex::new(Vec::new())),
                skipped_downloads: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub fn start_batch(&self, total_tasks: usize) {
            info!("开始新一批下载任务，总数: {}", total_tasks);
            let mut stats = self.stats.lock().unwrap();
            *stats = DownloadStats { total: total_tasks, ..Default::default() };
            self.failed_downloads.lock().unwrap().clear();
            self.skipped_downloads.lock().unwrap().clear();
        }

        pub fn record_success(&self) { self.stats.lock().unwrap().success += 1; }
        pub fn record_skip(&self, filename: &str, reason: &str) {
            info!("跳过文件 '{}'，原因: {}", filename, reason);
            self.stats.lock().unwrap().skipped += 1;
            self.skipped_downloads.lock().unwrap().push((filename.to_string(), reason.to_string()));
        }
        pub fn record_failure(&self, filename: &str, status: DownloadStatus) {
            error!("文件 '{}' 下载失败，状态: {:?}", filename, status);
            self.stats.lock().unwrap().failed += 1;
            let (_, _, msg) = get_status_display_info(status);
            self.failed_downloads.lock().unwrap().push((filename.to_string(), msg.to_string()));
        }
        pub fn reset_token_failures(&self, filenames_to_reset: &[String]) {
            let mut failed_downloads = self.failed_downloads.lock().unwrap();
            let original_len = failed_downloads.len();
            failed_downloads.retain(|(name, _)| !filenames_to_reset.contains(name));
            let removed_count = original_len - failed_downloads.len();
            if removed_count > 0 {
                info!("重置了 {} 个因Token失败的任务", removed_count);
                self.stats.lock().unwrap().failed -= removed_count;
            }
        }
        pub fn get_stats(&self) -> DownloadStats { self.stats.lock().unwrap().clone() }
        pub fn did_all_succeed(&self) -> bool { self.stats.lock().unwrap().failed == 0 }

        pub fn print_report(&self) {
            let stats = self.get_stats();
            let skipped = self.skipped_downloads.lock().unwrap();
            let failed = self.failed_downloads.lock().unwrap();
            info!("下载报告: Total={}, Success={}, Skipped={}, Failed={}", stats.total, stats.success, stats.skipped, stats.failed);

            if !skipped.is_empty() || !failed.is_empty() {
                ui::print_sub_header("下载详情报告");
                if !skipped.is_empty() { println!("\n{} 跳过的文件 ({}个):", *symbols::INFO, stats.skipped); print_grouped_report(&skipped); }
                if !failed.is_empty() { println!("\n{} 失败的文件 ({}个):", *symbols::ERROR, stats.failed); print_grouped_report(&failed); }
            }
            ui::print_sub_header("任务总结");
            if stats.success == 0 && stats.failed == 0 && stats.skipped > 0 {
                println!("{} 所有 {} 个文件均已存在且有效，无需操作。", *symbols::OK, stats.total);
            } else {
                let summary = format!("{} | {} | {}", format!("成功: {}", stats.success).green(), format!("失败: {}", stats.failed).red(), format!("跳过: {}", stats.skipped).yellow());
                println!("{}", summary);
            }

            fn print_grouped_report(items: &[(String, String)]) {
                let mut grouped: HashMap<&String, Vec<&String>> = HashMap::new();
                for (filename, reason) in items { grouped.entry(reason).or_default().push(filename); }
                let mut sorted_reasons: Vec<_> = grouped.keys().collect();
                sorted_reasons.sort();
                for reason in sorted_reasons {
                    println!("  - 原因: {}", reason);
                    let mut filenames = grouped.get(reason).unwrap().clone();
                    filenames.sort();
                    for filename in filenames { println!("    - {}", filename); }
                }
            }
        }
    }

    pub fn get_status_display_info(status: DownloadStatus) -> (&'static ColoredString, fn(ColoredString) -> ColoredString, &'static str) {
        match status {
            DownloadStatus::Success => (&symbols::OK, |s| s.green(), "下载并校验成功"),
            DownloadStatus::Resumed => (&symbols::OK, |s| s.green(), "续传成功，文件有效"),
            DownloadStatus::Skipped => (&symbols::INFO, |s| s.cyan(), "文件已存在，跳过"),
            DownloadStatus::Md5Failed => (&symbols::ERROR, |s| s.red(), "校验失败 (MD5不匹配)"),
            DownloadStatus::SizeFailed => (&symbols::ERROR, |s| s.red(), "校验失败 (大小不匹配)"),
            DownloadStatus::HttpError => (&symbols::ERROR, |s| s.red(), "服务器返回错误"),
            DownloadStatus::NetworkError => (&symbols::ERROR, |s| s.red(), "网络请求失败"),
            DownloadStatus::ConnectionError => (&symbols::ERROR, |s| s.red(), "无法建立连接"),
            DownloadStatus::TimeoutError => (&symbols::WARN, |s| s.yellow(), "网络连接超时"),
            DownloadStatus::MergeError => (&symbols::ERROR, |s| s.red(), "视频分片合并失败"),
            DownloadStatus::KeyError => (&symbols::ERROR, |s| s.red(), "视频解密密钥获取失败"),
            DownloadStatus::TokenError => (&symbols::ERROR, |s| s.red(), "认证失败 (Token无效)"),
            DownloadStatus::IoError => (&symbols::ERROR, |s| s.red(), "本地文件读写错误"),
            DownloadStatus::UnexpectedError => (&symbols::ERROR, |s| s.red(), "发生未预期的程序错误"),
        }
    }

    pub struct ResourceDownloader {
        context: DownloadJobContext,
        m_progress: MultiProgress,
    }

    impl ResourceDownloader {
        pub fn new(context: DownloadJobContext) -> Self {
            Self { context, m_progress: MultiProgress::new() }
        }

        pub async fn run(&self, url: &str) -> AppResult<bool> {
            info!("开始处理 URL: {}", url);
            let (extractor, resource_id) = self.get_extractor_info(url)?;
            self.prepare_and_run(extractor, &resource_id).await
        }

        pub async fn run_with_id(&self, resource_id: &str) -> AppResult<bool> {
            info!("开始处理 ID: {}", resource_id);
            let r#type = self.context.args.r#type.as_ref().unwrap();
            let api_conf = self.context.config.api_endpoints.get(r#type).unwrap();
            let extractor = self.create_extractor(api_conf);
            self.prepare_and_run(extractor, resource_id).await
        }

        async fn prepare_and_run(&self, extractor: Box<dyn extractor::ResourceExtractor>, resource_id: &str) -> AppResult<bool> {
            let base_output_dir = self.context.args.output.clone();
            fs::create_dir_all(&base_output_dir)?;
            let absolute_path = dunce::canonicalize(&base_output_dir)?;
            info!("文件将保存到目录: \"{}\"", absolute_path.display());
            println!("\n{} 文件将保存到目录: \"{}\"", *symbols::INFO, absolute_path.display());

            let mut all_file_items = extractor.extract_file_info(resource_id, &self.context).await?;
            if all_file_items.is_empty() {
                println!("\n{} 未能提取到任何可下载的文件信息。", *symbols::INFO);
                return Ok(true);
            }
            debug!("提取到 {} 个文件项。", all_file_items.len());

            for item in &mut all_file_items {
                item.filepath = utils::secure_join_path(&base_output_dir, &item.filepath)?;
            }
            let indices = if all_file_items.len() == 1 { vec![0] } else { self.display_files_and_prompt_selection(&all_file_items)? };

            if indices.is_empty() {
                println!("\n{} 未选择任何文件，任务结束。", *symbols::INFO);
                return Ok(true);
            }
            let tasks_to_run: Vec<FileInfo> = indices.into_iter().map(|i| all_file_items[i].clone()).collect();
            // tasks_to_run.sort_by_key(|item| item.ti_size.unwrap_or(0));
            debug!("最终确定了 {} 个下载任务。", tasks_to_run.len());

            let mut tasks_to_attempt = tasks_to_run.clone();
            self.context.manager.start_batch(tasks_to_attempt.len());

            loop {
                match self.execute_download_tasks(&tasks_to_attempt).await {
                    Ok(_) => break,
                    Err(AppError::TokenInvalid) => {
                        warn!("下载任务因 Token 失效而中断。");
                        if self.context.non_interactive {
                            error!("非交互模式下 Token 失效，无法继续。");
                            return Err(AppError::TokenInvalid);
                        }
                        let retry_result = self.handle_token_failure_and_retry(&tasks_to_run).await?;
                        if retry_result.should_abort {
                            info!("用户选择中止任务。");
                            return Ok(false);
                        }
                        if let Some(remaining) = retry_result.remaining_tasks {
                            tasks_to_attempt = remaining;
                        } else { break; }
                    }
                    Err(e) => { error!("执行下载任务时发生不可恢复的错误: {}", e); return Err(e); }
                }
            }
            self.context.manager.print_report();
            Ok(self.context.manager.did_all_succeed())
        }

        fn get_extractor_info(&self, url_str: &str) -> AppResult<(Box<dyn extractor::ResourceExtractor>, String)> {
            let url = Url::parse(url_str)?;
            debug!("解析 URL: {}", url);
            for (path_key, api_conf) in &self.context.config.api_endpoints {
                if url.path().contains(path_key) {
                    debug!("URL 路径匹配 API 端点: '{}'", path_key);
                    if let Some(resource_id) = url.query_pairs().find(|(k, _)| k == &api_conf.id_param) {
                        let id = resource_id.1.to_string();
                        if utils::is_resource_id(&id) {
                            info!("从 URL 中成功提取到资源 ID: '{}' (类型: {})", id, path_key);
                            return Ok((self.create_extractor(api_conf), id));
                        }
                    }
                }
            }
            error!("无法从 URL '{}' 中识别资源类型或提取ID。", url_str);
            Err(AppError::Other(anyhow!("无法识别的URL格式或不支持的资源类型。")))
        }

        fn create_extractor(&self, api_conf: &ApiEndpointConfig) -> Box<dyn extractor::ResourceExtractor> {
            match api_conf.extractor {
                ResourceExtractorType::Textbook => {
                    debug!("创建 TextbookExtractor");
                    Box::new(extractor::TextbookExtractor::new(self.context.http_client.clone(), self.context.config.clone()))
                }
                ResourceExtractorType::Course => {
                    let template_key = api_conf.url_template_keys.get("main").expect("Course API config missing 'main' template key");
                    let url_template = self.context.config.url_templates.get(template_key).expect("URL template not found for key").clone();
                    debug!("创建 CourseExtractor, 使用 URL 模板: {}", url_template);
                    Box::new(extractor::CourseExtractor::new(self.context.http_client.clone(), self.context.config.clone(), url_template))
                }
            }
        }

        async fn handle_token_failure_and_retry(&self, initial_tasks: &[FileInfo]) -> AppResult<TokenRetryResult> {
            ui::box_message("认证失败", &["当前 Access Token 已失效或无权限访问。", "输入 '2' 可以查看获取 Token 的详细指南。"], |s| s.red());
            loop {
                let prompt_msg = format!("选择操作: [1] 输入新 Token  [2] 查看帮助 (按 {} 中止)", *symbols::CTRL_C);
                match ui::prompt(&prompt_msg, Some("1")) {
                    Ok(choice) if choice == "1" => {
                        match ui::prompt_hidden("请输入新 Token (输入不可见，完成后按回车)") {
                            Ok(new_token) if !new_token.is_empty() => {
                                info!("用户输入了新的 Token。");
                                *self.context.token.lock().await = new_token.clone();
                                if ui::confirm("是否保存此新 Token 以便后续使用?", false) {
                                    if let Err(e) = config::save_token(&new_token) {
                                        error!("尝试保存新Token时失败: {}", e);
                                        eprintln!("{} 保存新Token失败: {}", *symbols::WARN, e);
                                    }
                                }
                                break;
                            }
                            _ => println!("{}", "Token 不能为空。".yellow()),
                        }
                    }
                    Ok(choice) if choice == "2" => { ui::box_message("获取 Access Token 指南", constants::HELP_TOKEN_GUIDE.lines().collect::<Vec<_>>().as_slice(), |s| s.cyan()); }
                    Ok(_) => continue,
                    Err(_) => { warn!("用户在 Token 提示处中断。"); return Ok(TokenRetryResult { remaining_tasks: None, should_abort: true }); }
                }
            }
            println!("\n{} Token 已更新。正在检查剩余任务...", *symbols::INFO);
            let mut remaining_tasks = vec![];
            let mut remaining_filenames = vec![];
            for task in initial_tasks {
                let (action, _, _) = Self::prepare_download_action(task, &self.context.args)?;
                if action != DownloadAction::Skip {
                    remaining_tasks.push(task.clone());
                    remaining_filenames.push(task.filepath.to_string_lossy().into_owned());
                }
            }
            if remaining_tasks.is_empty() {
                info!("所有任务均已完成，无需重试。");
                println!("{} 所有任务均已完成，无需重试。", *symbols::OK);
                return Ok(TokenRetryResult { remaining_tasks: None, should_abort: false });
            }
            self.context.manager.reset_token_failures(&remaining_filenames);
            info!("准备重试剩余的 {} 个任务。", remaining_tasks.len());
            println!("{} 准备重试剩余的 {} 个任务...", *symbols::INFO, remaining_tasks.len());
            Ok(TokenRetryResult { remaining_tasks: Some(remaining_tasks), should_abort: false })
        }

        fn display_files_and_prompt_selection(&self, items: &[FileInfo]) -> AppResult<Vec<usize>> {
            let options: Vec<String> = items.iter().map(|item| {
                let date_str = item.date.map_or("[ 日期未知 ]".to_string(), |d| format!("[{}]", d.format("%Y-%m-%d")));
                let filename_str = utils::truncate_text(&item.filepath.file_name().unwrap().to_string_lossy(), constants::FILENAME_TRUNCATE_LENGTH);
                format!("{} {}", date_str, filename_str)
            }).collect();
            let user_input = if self.context.non_interactive {
                self.context.args.select.clone()
            } else {
                ui::selection_menu(&options, "文件下载列表", "支持格式: 1, 3, 2-4, all", &self.context.args.select)
            };
            let indices = utils::parse_selection_indices(&user_input, items.len());
            debug!("用户选择的下载索引: {:?}", indices);
            Ok(indices)
        }

        async fn execute_download_tasks(&self, tasks: &[FileInfo]) -> AppResult<()> {
            let max_workers = min(self.context.config.max_workers, tasks.len());
            if max_workers == 0 { return Ok(()); }
            println!("\n{} 开始下载 {} 个文件 (并发数: {})...", *symbols::INFO, tasks.len(), max_workers);

            let task_id_counter = Arc::new(AtomicU64::new(0));
            let active_tasks = Arc::new(Mutex::new(BTreeMap::<u64, String>::new()));
            let main_pbar_style = ProgressStyle::with_template("{prefix:7.bold.cyan} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos:>3}/{len:3} ({percent:>3}%) [ETA: {eta}]").expect("进度条模板无效").progress_chars("#>-");
            let main_pbar = self.m_progress.add(ProgressBar::new(tasks.len() as u64));
            main_pbar.set_style(main_pbar_style.clone());
            main_pbar.set_prefix("总进度");
            main_pbar.enable_steady_tick(Duration::from_millis(100));

            let error_sender = Arc::new(TokioMutex::new(None::<AppError>));
            let tasks_stream = stream::iter(tasks.to_owned());
            let update_msg = |pbar: &ProgressBar, tasks: &Mutex<BTreeMap<u64, String>>| {
                let guard = tasks.lock().unwrap();
                pbar.set_message(guard.values().cloned().collect::<Vec<String>>().join(", "));
            };
            update_msg(&main_pbar, &active_tasks);

            tasks_stream.for_each_concurrent(max_workers, |task| {
                let context = self.context.clone();
                let main_pbar = main_pbar.clone();
                let m_progress = self.m_progress.clone();
                let error_sender = error_sender.clone();
                let task_id_counter = task_id_counter.clone();
                let active_tasks = active_tasks.clone();
                async move {
                    if context.cancellation_token.load(Ordering::Relaxed) { return; }
                    if error_sender.lock().await.is_some() {
                        trace!("检测到全局错误，提前中止新任务的启动。");
                        return;
                    }
                    let task_id = task_id_counter.fetch_add(1, Ordering::Relaxed);
                    let task_name = task.filepath.file_name().unwrap().to_string_lossy().to_string();
                    active_tasks.lock().unwrap().insert(task_id, utils::truncate_text(&task_name, 30));
                    update_msg(&main_pbar, &active_tasks);

                    match Self::process_single_task(task.clone(), context.clone()).await {
                        Ok(result) => {
                            match result.status {
                                DownloadStatus::Success | DownloadStatus::Resumed => context.manager.record_success(),
                                DownloadStatus::Skipped => context.manager.record_skip(&result.filename, result.message.as_deref().unwrap_or("文件已存在")),
                                _ => context.manager.record_failure(&result.filename, result.status),
                            }
                            if result.status != DownloadStatus::Skipped {
                                let (symbol, _, default_msg) = get_status_display_info(result.status);
                                let msg = if let Some(err_msg) = result.message {
                                    format!("\n{} 任务 '{}' 失败: {} (详情: {})", symbol, task_name, default_msg, err_msg)
                                } else {
                                    format!("{} {}", symbol, task_name)
                                };
                                let _ = m_progress.println(msg);
                            }
                        }
                        Err(e @ AppError::TokenInvalid) => {
                            let mut error_lock = error_sender.lock().await;
                            if error_lock.is_none() {
                                error!("任务 '{}' 因 Token 失效失败，将中止整个批次。", task_name);
                                context.manager.record_failure(&task.filepath.to_string_lossy(), DownloadStatus::TokenError);
                                *error_lock = Some(e);
                            }
                        }
                        Err(e) => { error!("未捕获的错误在并发循环中: {}", e); }
                    }
                    main_pbar.inc(1);
                    active_tasks.lock().unwrap().remove(&task_id);
                    update_msg(&main_pbar, &active_tasks);
                }
            }).await;
            main_pbar.finish_and_clear();

            if self.context.cancellation_token.load(Ordering::Relaxed) { return Err(AppError::UserInterrupt); }
            if let Some(err) = error_sender.lock().await.take() { return Err(err); }
            Ok(())
        }

        async fn process_single_task(item: FileInfo, context: DownloadJobContext) -> AppResult<DownloadResult> {
            debug!("开始处理单个下载任务: {:?}", item.filepath);
            let attempt_result: AppResult<DownloadResult> = async {
                if let Some(parent) = item.filepath.parent() { fs::create_dir_all(parent)?; }
                let (action, resume_bytes, reason) = Self::prepare_download_action(&item, &context.args)?;
                debug!("文件 '{:?}' 的下载操作为: {:?} (resume_bytes: {}, reason: '{}')", item.filepath, action, resume_bytes, reason);

                if action == DownloadAction::Skip {
                    return Ok(DownloadResult {
                        filename: item.filepath.file_name().unwrap().to_string_lossy().to_string(),
                        status: DownloadStatus::Skipped, message: Some(reason),
                    });
                }
                let is_m3u8 = item.url.ends_with(api::resource_formats::M3U8);
                let download_status = if is_m3u8 {
                    M3u8Downloader::new(context.clone()).download(&item).await?
                } else {
                    Self::download_standard_file(&item, resume_bytes, &context).await?
                };
                let final_status = if download_status == DownloadStatus::Success || download_status == DownloadStatus::Resumed {
                    Self::finalize_and_validate(&item, is_m3u8)?
                } else { download_status };
                Ok(DownloadResult {
                    filename: item.filepath.file_name().unwrap().to_string_lossy().to_string(),
                    status: final_status, message: None,
                })
            }.await;
            match attempt_result {
                Ok(result) => Ok(result),
                Err(e @ AppError::TokenInvalid) => Err(e),
                Err(e) => {
                    error!("处理任务 '{:?}' 时发生错误: {}", item.filepath, e);
                    Ok(DownloadResult {
                        filename: item.filepath.file_name().unwrap().to_string_lossy().to_string(),
                        status: DownloadStatus::from(&e), message: Some(e.to_string()),
                    })
                }
            }
        }

        fn prepare_download_action(item: &FileInfo, args: &Cli) -> AppResult<(DownloadAction, u64, String)> {
            if !item.filepath.exists() { return Ok((DownloadAction::DownloadNew, 0, "文件不存在".to_string())); }
            if args.force_redownload {
                info!("用户强制重新下载文件: {:?}", item.filepath);
                return Ok((DownloadAction::DownloadNew, 0, "强制重新下载".to_string()));
            }
            if item.url.ends_with(api::resource_formats::M3U8) {
                info!("视频文件 {:?} 已存在，跳过下载。使用 -f 强制重下。", item.filepath);
                return Ok((DownloadAction::Skip, 0, "视频已存在 (使用 -f 强制重下)".to_string()));
            }
            match Self::validate_file(&item.filepath, item.ti_md5.as_deref(), item.ti_size) {
                Ok(_) => {
                    info!("文件 {:?} 已存在且校验通过，跳过。使用 -f 强制重下。", item.filepath);
                    Ok((DownloadAction::Skip, 0, "文件已存在且校验通过 (使用 -f 强制重下)".to_string()))
                }
                Err(e) => {
                    warn!("文件 '{:?}' 存在但校验失败: {}", item.filepath, e);
                    let actual_size = item.filepath.metadata()?.len();
                    if let Some(expected_size) = item.ti_size {
                        if actual_size > 0 && actual_size < expected_size {
                            info!("文件 '{:?}' 不完整 ({} / {} bytes)，将进行续传。", item.filepath, actual_size, expected_size);
                            return Ok((DownloadAction::Resume, actual_size, "文件不完整，尝试续传".to_string()));
                        }
                    }
                    println!("{} {} - 文件已存在但校验失败，将重新下载。", *symbols::WARN, item.filepath.display());
                    info!("文件 '{:?}' 存在但校验失败，将重新下载。", item.filepath);
                    Ok((DownloadAction::DownloadNew, 0, "文件校验失败".to_string()))
                }
            }
        }

        fn finalize_and_validate(item: &FileInfo, is_m3u8: bool) -> AppResult<DownloadStatus> {
            debug!("对文件 '{:?}' 进行最终校验", item.filepath);
            if !item.filepath.exists() || item.filepath.metadata()?.len() == 0 {
                error!("下载完成但文件 '{:?}' 不存在或为空。", item.filepath);
                return Ok(DownloadStatus::SizeFailed);
            }
            if is_m3u8 { return Ok(DownloadStatus::Success); }
            match Self::validate_file(&item.filepath, item.ti_md5.as_deref(), item.ti_size) {
                Ok(_) => { debug!("文件 '{:?}' 校验成功。", item.filepath); Ok(DownloadStatus::Success) }
                Err(e @ AppError::Validation(_)) => { error!("文件 '{:?}' 最终校验失败: {}", item.filepath, e); Err(e) }
                Err(e) => Err(e),
            }
        }

        fn validate_file(filepath: &Path, expected_md5: Option<&str>, expected_size: Option<u64>) -> AppResult<()> {
            if let Some(size) = expected_size {
                let actual_size = filepath.metadata()?.len();
                if actual_size != size {
                    trace!("大小校验失败 for '{:?}': expected {}, got {}", filepath, size, actual_size);
                    return Err(AppError::Validation("大小不匹配".to_string()));
                }
            }
            if let Some(md5) = expected_md5 {
                let actual_md5 = utils::calculate_file_md5(filepath)?;
                if !actual_md5.eq_ignore_ascii_case(md5) {
                    trace!("MD5校验失败 for '{:?}': expected {}, got {}", filepath, md5, actual_md5);
                    return Err(AppError::Validation("MD5不匹配".to_string()));
                }
            }
            Ok(())
        }

        async fn download_standard_file(item: &FileInfo, resume_from: u64, context: &DownloadJobContext) -> AppResult<DownloadStatus> {
            let mut current_resume_from = resume_from;
            loop {
                let mut url = Url::parse(&item.url)?;
                let token = context.token.lock().await;
                if !token.is_empty() { url.query_pairs_mut().append_pair("accessToken", &token); }
                let mut request_builder = context.http_client.client.get(url.clone());
                if current_resume_from > 0 {
                    debug!("尝试从 {} 字节处续传: {}", current_resume_from, url);
                    request_builder = request_builder.header(header::RANGE, format!("bytes={}-", current_resume_from));
                }
                drop(token);
                let res = request_builder.send().await?;
                if res.status() == StatusCode::RANGE_NOT_SATISFIABLE {
                    warn!("续传点 {} 无效，将从头开始下载: {}", current_resume_from, &item.filepath.display());
                    println!("{} 续传点无效，将从头开始下载: {}", *symbols::WARN, &item.filepath.display());
                    current_resume_from = 0;
                    if item.filepath.exists() { fs::remove_file(&item.filepath)?; }
                    continue;
                }
                if let Err(e) = res.error_for_status_ref() {
                    let status = e.status();
                    if status == Some(StatusCode::UNAUTHORIZED) || status == Some(StatusCode::FORBIDDEN) {
                        return Err(AppError::TokenInvalid);
                    }
                    return Err(AppError::from(e));
                }
                let mut file = if current_resume_from > 0 {
                    OpenOptions::new().write(true).append(true).open(&item.filepath)?
                } else {
                    File::create(&item.filepath)?
                };
                let mut stream = res.bytes_stream();
                while let Some(chunk_res) = stream.next().await { file.write_all(&chunk_res?)?; }
                return Ok(if current_resume_from > 0 { DownloadStatus::Resumed } else { DownloadStatus::Success });
            }
        }
    }

    struct M3u8Downloader { context: DownloadJobContext }
    impl M3u8Downloader {
        fn new(context: DownloadJobContext) -> Self { Self { context } }
        async fn download(&self, item: &FileInfo) -> AppResult<DownloadStatus> {
            info!("开始下载 M3U8 视频: {}", item.filepath.display());
            let mut url = Url::parse(&item.url)?;
            let token = self.context.token.lock().await;
            if !token.is_empty() { url.query_pairs_mut().append_pair("accessToken", &token); } drop(token);
            let (key, iv, playlist) = self.get_m3u8_key_and_playlist(url.clone()).await?;
            if playlist.segments.is_empty() {
                error!("M3U8文件 '{}' 不含分片", item.url);
                return Err(AppError::M3u8Parse("M3U8文件不含分片".to_string()));
            }
            info!("M3U8 包含 {} 个分片。 解密密钥: {}, IV: {}", playlist.segments.len(), if key.is_some() { "有" } else { "无" }, iv.as_deref().unwrap_or("无"));
            let segment_urls: Vec<String> = playlist.segments.iter().map(|s| s.uri.clone()).collect();
            let decryptor = if let (Some(key), Some(iv_hex)) = (key, iv) {
                let iv_bytes = hex::decode(iv_hex.trim_start_matches("0x")).map_err(|e| AppError::M3u8Parse(format!("无效的IV十六进制值: {}", e)))?;
                Some(Aes128CbcDec::new_from_slices(&key, &iv_bytes).map_err(|e| AppError::Security(format!("AES解密器初始化失败: {}", e)))?)
            } else { None };
            let temp_dir = tempfile::Builder::new().prefix("m3u8_dl_").tempdir()?;
            debug!("为M3U8下载创建临时目录: {:?}", temp_dir.path());
            self.download_segments_with_retry(&url, &segment_urls, temp_dir.path(), decryptor).await?;
            info!("所有分片下载完成，开始合并...");
            self.merge_ts_segments(temp_dir.path(), segment_urls.len(), &item.filepath)?;
            info!("分片合并完成 -> {}", item.filepath.display());
            Ok(DownloadStatus::Success)
        }
        async fn download_segments_with_retry(&self, base_url: &Url, urls: &[String], temp_path: &Path, decryptor: Option<Aes128CbcDec>) -> AppResult<()> {
            let mut failed_indices: Vec<usize> = (0..urls.len()).collect();
            for attempt in 0..=self.context.config.max_retries {
                if failed_indices.is_empty() { break; }
                if attempt > 0 {
                    warn!("第 {} 次重试下载 {} 个失败的分片...", attempt, failed_indices.len());
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                let stream = stream::iter(failed_indices.clone()).map(|i| {
                        let url_res = base_url.join(&urls[i]);
                        let ts_path = temp_path.join(format!("{:05}.ts", i));
                        let client = self.context.http_client.clone();
                        let decryptor = decryptor.clone();
                        tokio::spawn(async move {
                            let url = match url_res { Ok(url) => url, Err(e) => return (i, Err(AppError::from(e))) };
                            match Self::download_ts_segment(client, url, &ts_path, decryptor).await {
                                Ok(_) => (i, Ok(())),
                                Err(e) => { trace!("分片 #{} 下载失败: {}", i, e); (i, Err(e)) }
                            }
                        })
                    }).buffer_unordered(self.context.config.max_workers * 2);
                let results: Vec<_> = stream.collect().await;
                failed_indices = results.into_iter().filter_map(|handle_res| {
                        match handle_res {
                            Ok((_index, Ok(_))) => None, Ok((index, Err(_))) => Some(index),
                            Err(_) => { error!("一个下载任务 panic 或被取消"); None }
                        }
                    }).collect();
            }
            if !failed_indices.is_empty() {
                error!("{} 个分片最终下载失败", failed_indices.len());
                return Err(AppError::Merge(format!("{} 个分片最终下载失败", failed_indices.len())));
            }
            Ok(())
        }
        async fn download_ts_segment(client: Arc<RobustClient>, url: Url, ts_path: &Path, decryptor: Option<Aes128CbcDec>) -> AppResult<()> {
            let res = client.get(url).await?;
            let data = res.bytes().await?;
            let final_data = if let Some(d) = decryptor {
                d.decrypt_padded_vec_mut::<Pkcs7>(&data).map_err(|e| AppError::Security(format!("分片解密失败: {}", e)))?
            } else { data.to_vec() };
            fs::write(ts_path, &final_data)?;
            Ok(())
        }
        fn merge_ts_segments(&self, temp_dir: &Path, num_segments: usize, output_path: &Path) -> AppResult<()> {
            let temp_output_path = output_path.with_extension("tmp");
            let mut writer = BufWriter::new(File::create(&temp_output_path)?);
            for i in 0..num_segments {
                let ts_path = temp_dir.join(format!("{:05}.ts", i));
                if !ts_path.exists() {
                    let filename = ts_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "未知分片".to_string());
                    return Err(AppError::Merge(format!("丢失视频分片: {}", filename)));
                }
                let mut reader = File::open(ts_path)?; io::copy(&mut reader, &mut writer)?;
            }
            writer.flush()?; fs::rename(temp_output_path, output_path)?;
            Ok(())
        }
        async fn get_m3u8_key_and_playlist(&self, m3u8_url: Url) -> AppResult<(Option<Vec<u8>>, Option<String>, m3u8_rs::MediaPlaylist)> {
            debug!("获取并解析 M3U8 文件: {}", m3u8_url);
            let playlist_text = self.context.http_client.get(m3u8_url.clone()).await?.text().await?;
            trace!("M3U8 内容: {}", playlist_text);
            let playlist = m3u8_rs::parse_playlist_res(playlist_text.as_bytes()).map_err(|e| AppError::M3u8Parse(e.to_string()))?;
            let m3u8_rs::Playlist::MediaPlaylist(media) = playlist else {
                return Err(AppError::M3u8Parse("预期的M3U8文件不是媒体播放列表".to_string()));
            };
            let Some((uri, iv)) = media.segments.iter().find_map(|seg| seg.key.as_ref().and_then(|k|
                if let m3u8_rs::Key { uri: Some(uri), iv, .. } = k { Some((uri.clone(), iv.clone())) } else { None }
            )) else {
                debug!("M3U8 未加密");
                return Ok((None, None, media));
            };
            debug!("在M3U8中找到加密信息. Key URI: {}, IV: {:?}", uri, iv);
            let key_url = m3u8_url.join(&uri)?;
            let nonce_url = format!("{}/signs", key_url);
            debug!("获取 nonce from: {}", nonce_url);
            let signs_data: serde_json::Value = self.context.http_client.get(&nonce_url).await?.json().await?;
            let nonce = signs_data.get("nonce").and_then(|v| v.as_str()).ok_or_else(|| AppError::M3u8Parse("密钥服务器响应中未找到 'nonce'".to_string()))?;
            trace!("获取到 nonce: {}", nonce);
            let key_filename = key_url.path_segments().and_then(|s| s.last()).ok_or_else(|| AppError::M3u8Parse(format!("无法从密钥URL中提取文件名: {}", key_url)))?;
            let sign_material = format!("{}{}", nonce, key_filename);
            let mut hasher = Md5::new(); hasher.update(sign_material.as_bytes());
            let result = hasher.finalize();
            let sign = &format!("{:x}", result)[..16];
            debug!("计算得到 sign: {}", sign);
            let final_key_url = format!("{}?nonce={}&sign={}", key_url, nonce, sign);
            debug!("获取最终密钥 from: {}", final_key_url);
            let key_data: serde_json::Value = self.context.http_client.get(&final_key_url).await?.json().await?;
            let encrypted_key_b64 = key_data.get("key").and_then(|v| v.as_str()).ok_or_else(|| AppError::M3u8Parse("密钥服务器响应中未找到加密密钥 'key'".to_string()))?;
            let encrypted_key = BASE64.decode(encrypted_key_b64)?;
            type EcbDec = ecb::Decryptor<aes::Aes128>;
            let cipher = EcbDec::new(sign.as_bytes().into());
            let decrypted_key = cipher.decrypt_padded_vec_mut::<Pkcs7>(&encrypted_key).map_err(|e| AppError::Security(format!("AES密钥解密失败: {}", e)))?;
            debug!("密钥解密成功");
            Ok((Some(decrypted_key), iv, media))
        }
    }
}

// --- 12. 命令行接口 (CLI) ---
#[derive(ValueEnum, Copy, Clone, Debug, PartialEq, Eq)]
enum LogLevel { Off, Error, Warn, Info, Debug, Trace }

#[derive(Parser, Debug, Clone)]
#[command(
    about, long_about = None, arg_required_else_help = true,
    disable_help_flag = true, disable_version_flag = true,
)]
#[command(group(clap::ArgGroup::new("mode").required(true)
    .args(&["interactive", "url", "id", "batch_file", "token_help"]),
))]
struct Cli {
    // Mode
    #[arg(short, long, action = clap::ArgAction::SetTrue, help_heading = "Mode")]
    interactive: bool,
    #[arg(long, help_heading = "Mode")]
    url: Option<String>,
    #[arg(long, help_heading = "Mode")]
    id: Option<String>,
    #[arg(short, long, value_name = "FILE", help_heading = "Mode")]
    batch_file: Option<PathBuf>,
    #[arg(long, action = clap::ArgAction::SetTrue, help_heading = "Mode")]
    token_help: bool,
    // Options
    #[arg(long, default_value_t = constants::DEFAULT_SELECTION.to_string(), value_name = "SELECTION", help_heading = "Options")]
    select: String,
    #[arg(long, help = "有效选项: tchMaterial, qualityCourse, syncClassroom/classActivity", help_heading = "Options")]
    r#type: Option<String>,
    #[arg(long, help_heading = "Options")]
    token: Option<String>,
    #[arg(short, long, action = clap::ArgAction::SetTrue, help_heading = "Options")]
    force_redownload: bool,
    #[arg(short='q', long, default_value_t = constants::DEFAULT_VIDEO_QUALITY.to_string(), help_heading = "Options")]
    video_quality: String,
    #[arg(long, default_value_t = constants::DEFAULT_AUDIO_FORMAT.to_string(), help_heading = "Options")]
    audio_format: String,
    #[arg(long, action = clap::ArgAction::SetTrue, help_heading = "Options")]
    prompt_each: bool,
    #[arg(short, long, value_parser = clap::value_parser!(usize), help_heading = "Options")]
    workers: Option<usize>,
    #[arg(short, long, value_name = "DIR", default_value_os_t = PathBuf::from(constants::DEFAULT_SAVE_DIR), help_heading = "Options")]
    output: PathBuf,
    // General
    #[arg(short = 'h', long, action = clap::ArgAction::Help, global = true, help_heading = "General")]
    help: Option<bool>,
    #[arg(short = 'V', long, action = clap::ArgAction::Version, global = true, help_heading = "General")]
    version: Option<bool>,
    #[arg(long, value_enum, default_value_t = LogLevel::Off, global = true, hide = true)]
    log_level: LogLevel,
}

// --- 13. 主程序与执行流程 ---
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

    // 使用 clap::crate_name!() 宏获取程序名，避免硬编码
    let app_name = clap::crate_name!();

    // 优先使用标准配置目录
    let log_file_path = match dirs::home_dir() {
        Some(home) => home
            .join(constants::CONFIG_DIR_NAME)
            .join(constants::LOG_FILE_NAME),
        // 如果无法获取主目录，则回退到临时目录
        None => {
            eprintln!("警告: 无法获取用户主目录，日志将写入临时目录。");
            env::temp_dir()
                .join(app_name) // 在临时目录下创建一个以程序名命名的子目录
                .join(constants::LOG_FILE_NAME)
        }
    };
    
    // 确保日志目录存在
    if let Some(dir) = log_file_path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("警告: 无法创建日志目录 {:?}: {}", dir, e);
            // 即使目录创建失败，也尝试继续，fern 可能会在根目录创建文件
        }
    }

    // 尝试创建主日志文件
    let file_appender = match fern::log_file(&log_file_path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!(
                "警告: 无法打开主日志文件 {:?} : {}。将尝试使用备用日志文件。",
                log_file_path, e
            );
            
            // 构建备用日志文件路径，文件名包含程序名以保证唯一性
            let fallback_path = std::env::temp_dir().join(format!(
                "{}-{}",
                app_name,
                constants::LOG_FALLBACK_FILE_NAME
            ));

            match fern::log_file(&fallback_path) {
                Ok(fb_file) => {
                    // 使用 log crate 记录警告信息，如果日志系统后续成功初始化，这条信息会被记录下来
                    warn!("日志将写入备用文件: {:?}", fallback_path); 
                    fb_file
                },
                Err(e_fb) => {
                    eprintln!(
                        "错误: 无法创建主日志和备用日志文件 {:?}: {}。日志将不会被记录到文件。",
                        fallback_path, e_fb
                    );
                    return; // 彻底失败，直接返回
                }
            }
        }
    };

    // 配置并应用日志格式
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

fn validate_cli_args(args: &Cli, config: &AppConfig) -> AppResult<()> {
    if (args.id.is_some() || args.batch_file.is_some()) && args.r#type.is_none() {
        return Err(AppError::Other(anyhow!("使用 --id 或 --batch-file 时，必须提供 --type 参数。")));
    }
    if let Some(t) = &args.r#type {
        if !config.api_endpoints.contains_key(t) {
            let valid_options = config.api_endpoints.keys().cloned().collect::<Vec<_>>().join(", ");
            return Err(AppError::Other(anyhow!("无效的资源类型 '{}'。有效选项: {}", t, valid_options)));
        }
    }
    Ok(())
}

async fn run_from_cli(args: Arc<Cli>, cancellation_token: Arc<AtomicBool>) -> AppResult<()> {
    debug!("CLI 参数: {:?}", args);
    if args.token_help {
        ui::box_message("获取 Access Token 指南", constants::HELP_TOKEN_GUIDE.lines().collect::<Vec<_>>().as_slice(), |s| s.cyan());
        println!("\n{} 安全提醒: 请妥善保管你的 Token，不要分享给他人。", *symbols::INFO);
        return Ok(());
    }
    let config = Arc::new(AppConfig::from_args(&args));
    debug!("加载的应用配置: {:?}", config);
    validate_cli_args(&args, &config)?;
    let (token_opt, source) = config::resolve_token(args.token.as_deref());
    if token_opt.is_some() {
        info!("从 {} 加载 Access Token", source);
        println!("\n{} 已从 {} 加载 Access Token。", *symbols::INFO, source);
    } else {
        info!("未找到本地 Access Token");
        println!("\n{}", format!("{} 未找到本地 Access Token，将在需要时提示输入。", *symbols::INFO).yellow());
    }
    let token = Arc::new(TokioMutex::new(token_opt.unwrap_or_default()));
    let http_client = Arc::new(RobustClient::new(config.clone())?);
    let context = DownloadJobContext {
        manager: downloader::DownloadManager::new(), token, config: config.clone(), http_client,
        args: args.clone(), non_interactive: !args.interactive && !args.prompt_each, cancellation_token,
    };

    if args.interactive { handle_interactive_mode(context).await?; }
    else if let Some(batch_file) = &args.batch_file { process_batch_tasks(batch_file, context).await?; }
    else if let Some(url) = &args.url { downloader::ResourceDownloader::new(context).run(url).await?; }
    else if let Some(id) = &args.id { downloader::ResourceDownloader::new(context).run_with_id(id).await?; };
    Ok(())
}

async fn handle_interactive_mode(base_context: DownloadJobContext) -> AppResult<()> {
    ui::print_header("交互模式");
    println!("在此模式下，你可以逐一输入链接进行下载。按 {} 可随时退出。", *symbols::CTRL_C);
    loop {
        match ui::prompt("请输入资源链接或ID", None) {
            Ok(input) if !input.is_empty() => {
                let context = base_context.clone();
                if let Err(e) = process_single_task_cli(&input, context).await {
                     log::error!("交互模式任务 '{}' 失败: {}", input, e);
                     eprintln!("\n{} 处理任务时发生错误: {}", *symbols::ERROR, e);
                }
            }
            Ok(_) => break, // 用户输入空行，退出
            Err(_) => return Err(AppError::UserInterrupt),
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
    let tasks: Vec<String> = content.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if tasks.is_empty() {
        log::warn!("批量文件 '{}' 为空或不含有效行。", batch_file.display());
        println!("{} 批量文件 '{}' 为空。", *symbols::WARN, batch_file.display());
        return Ok(());
    }
    let mut success = 0; let mut failed = 0;
    ui::print_header(&format!("开始批量处理任务 (按 {} 可随时退出)", *symbols::CTRL_C));
    for (i, task) in tasks.iter().enumerate() {
        if base_context.cancellation_token.load(std::sync::atomic::Ordering::Relaxed) {
             return Err(AppError::UserInterrupt);
        }
        ui::print_sub_header(&format!("批量任务 {}/{} - {}", i + 1, tasks.len(), utils::truncate_text(task, 60)));
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
    println!("{} | {} | 总计: {}", format!("成功任务: {}", success).green(), format!("失败任务: {}", failed).red(), tasks.len());
    if failed > 0 { Err(AppError::Other(anyhow!("{} 个批量任务执行失败。", failed))) } else { Ok(()) }
}

async fn process_single_task_cli(task_input: &str, context: DownloadJobContext) -> AppResult<()> {
    let result: AppResult<bool> = if utils::is_resource_id(task_input) {
        if context.args.r#type.is_none() {
            let msg = format!("任务 '{}' 是一个ID，但未提供 --type 参数，跳过。", task_input);
            log::error!("{}", msg); eprintln!("{} {}", *symbols::ERROR, msg);
            return Err(AppError::Other(anyhow!(msg)));
        }
        downloader::ResourceDownloader::new(context).run_with_id(task_input).await
    } else if Url::parse(task_input).is_ok() {
        downloader::ResourceDownloader::new(context).run(task_input).await
    } else {
        let msg = format!("跳过无效条目: {}", task_input);
        log::warn!("{}", msg); eprintln!("{} {}", *symbols::WARN, msg);
        return Ok(());
    };
    result.map(|_| ())
}

#[tokio::main]
async fn main() {
    #[cfg(windows)] { colored::control::set_virtual_terminal(true).ok(); }
    let after_help = format!("示例:\n  # 启动交互模式 (推荐)\n  {bin} -i\n\n  # 自动下载单个链接中的所有内容\n  {bin} --url \"https://...\"\n\n  # 批量下载并显示调试信息 (日志写入文件)\n  {bin} -b my_links.txt --type tchMaterial --log-level debug\n\n  # 获取 Token 帮助\n  {bin} --token-help", bin = clap::crate_name!());
    let cmd = Cli::command().override_usage(format!("{} <MODE> [OPTIONS]", clap::crate_name!())).after_help(after_help);
    let matches = cmd.get_matches();
    let args = Arc::new(Cli::from_arg_matches(&matches).unwrap());
    init_logger(args.log_level);
    let cancellation_token = Arc::new(AtomicBool::new(false));
    let handler_token = cancellation_token.clone();

    tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("无法监听 Ctrl-C 信号: {}", e); return;
        }
        if handler_token.load(Ordering::Relaxed) {
            println!("\n第二次中断，强制退出...");
            warn!("用户第二次按下 Ctrl+C，强制退出。");
            std::process::exit(130);
        }
        println!("\n{} 正在停止... 请等待当前任务完成。再按一次 {} 可强制退出。", *symbols::WARN, *symbols::CTRL_C);
        warn!("用户通过 Ctrl+C 请求中断程序。");
        handler_token.store(true, Ordering::Relaxed);
    });

    if let Err(e) = run_from_cli(args, cancellation_token).await {
        match e {
            AppError::UserInterrupt => {
                warn!("程序被用户中断。");
                std::process::exit(130);
            }
            AppError::TokenInvalid => {
                error!("程序因Token无效而退出: {}", e);
                eprintln!("\n{} {}", *symbols::ERROR, format!("{}", e).red());
                eprintln!("{} {}", *symbols::INFO, "请使用 --token-help 命令查看如何获取或更新您的 Access Token。");
                std::process::exit(1);
            }
            _ => {
                error!("程序执行出错: {}", e);
                eprintln!("\n{} {}", *symbols::ERROR, format!("程序执行出错: {}", e).red());
                std::process::exit(1);
            }
        }
    }
    info!("程序正常退出。");
}