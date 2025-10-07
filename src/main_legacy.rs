// --- 1. crate 导入与模块定义 ---
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyInit, KeyIvInit};
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, FixedOffset};
use clap::{command, CommandFactory, FromArgMatches, Parser};
use colored::*;
use dashmap::DashMap;
use futures::{stream, StreamExt};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use md5::{Digest, Md5};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::{header, IntoUrl, Response, StatusCode};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};
use serde::Deserialize;
use serde_json::Value;
use std::{
    cmp::min,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    env,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
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

// --- 3. 常量定义 ---
mod constants {
    pub const UI_WIDTH: usize = 88;
    pub const FILENAME_TRUNCATE_LENGTH: usize = 65;
    pub const MAX_FILENAME_BYTES: usize = 200;
    pub const CONFIG_DIR_NAME: &str = ".sed-dl";
    pub const CONFIG_FILE_NAME: &str = "config.json";
    pub const DEFAULT_SAVE_DIR: &str = "downloads";
    pub const UNCLASSIFIED_DIR: &str = "未分类资源";
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

// --- 4. 辅助函数与工具 ---
mod utils {
    use super::*;
    use std::collections::BTreeSet;

    pub static UUID_PATTERN: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^[a-f0-9]{8}-([a-f0-9]{4}-){3}[a-f0-9]{12}$").unwrap());

    static ILLEGAL_CHARS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"[\\/*?:"<>|]"#).unwrap());
    static WHITESPACE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").unwrap());

    pub fn is_resource_id(text: &str) -> bool {
        UUID_PATTERN.is_match(text)
    }

    pub fn sanitize_filename(name: &str) -> String {
        let original_name = name.trim();
        if original_name.is_empty() {
            return "unknown".to_string();
        }

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
        name = name
            .trim_matches(|c: char| c == '.' || c.is_whitespace())
            .to_string();

        if name.is_empty() {
            return "unnamed".to_string();
        }

        if name.as_bytes().len() > constants::MAX_FILENAME_BYTES {
            if let (Some(stem_part), Some(ext)) =
                (Path::new(&name).file_stem(), Path::new(&name).extension())
            {
                let stem_part_str = stem_part.to_string_lossy();
                let ext_str = format!(".{}", ext.to_string_lossy());
                let max_stem_bytes =
                    constants::MAX_FILENAME_BYTES.saturating_sub(ext_str.as_bytes().len());
                let truncated_stem = safe_truncate_utf8(&stem_part_str, max_stem_bytes);
                name = format!("{}{}", truncated_stem, ext_str);
            } else {
                name = safe_truncate_utf8(&name, constants::MAX_FILENAME_BYTES).to_string();
            }
        }
        name
    }

    fn safe_truncate_utf8(s: &str, max_bytes: usize) -> &str {
        if s.len() <= max_bytes {
            return s;
        }
        let mut i = max_bytes;
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
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
        if end_pos == 0 {
            text.to_string()
        } else {
            format!("{}...", &text[..end_pos])
        }
    }

    pub fn parse_selection_indices(selection_str: &str, total_items: usize) -> Vec<usize> {
        if selection_str.to_lowercase() == "all" {
            return (0..total_items).collect();
        }
        let mut indices = BTreeSet::new();
        for part in selection_str.split(',').map(|s| s.trim()) {
            if part.is_empty() {
                continue;
            }
            if let Some(range_part) = part.split_once('-') {
                if let (Ok(start), Ok(end)) =
                    (range_part.0.parse::<usize>(), range_part.1.parse::<usize>())
                {
                    if start == 0 || end == 0 {
                        continue;
                    }
                    let (min, max) = (start.min(end), start.max(end));
                    for i in min..=max {
                        if i > 0 && i <= total_items {
                            indices.insert(i - 1);
                        }
                    }
                }
            } else if let Ok(num) = part.parse::<usize>() {
                if num > 0 && num <= total_items {
                    indices.insert(num - 1);
                }
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
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        let result = hasher.finalize();
        Ok(format!("{:x}", result))
    }

    pub fn secure_join_path(base_dir: &Path, relative_path: &Path) -> AppResult<PathBuf> {
        let resolved_base = dunce::canonicalize(base_dir)
            .with_context(|| format!("基础目录 '{:?}' 不存在或无法访问", base_dir))?;

        let mut final_path = resolved_base.clone();
        for component in relative_path.components() {
            match component {
                Component::Normal(part) => final_path.push(part),
                Component::ParentDir => {
                    return Err(AppError::Security("检测到路径遍历 '..' ".to_string()));
                }
                _ => continue,
            }
        }

        if !final_path.starts_with(&resolved_base) {
            return Err(AppError::Security(format!(
                "路径遍历攻击检测: '{:?}'",
                relative_path
            )));
        }

        Ok(final_path)
    }
}

// --- 5. UI 与交互 ---
mod ui {
    use super::*;
    use std::io::{self, Write};

    pub fn print_header(title: &str) {
        println!("\n{}", "═".repeat(constants::UI_WIDTH));
        println!(" {}", title.cyan().bold());
        println!("{}", "═".repeat(constants::UI_WIDTH));
    }

    pub fn print_sub_header(title: &str) {
        println!("\n--- {} ---", title.bold());
    }

    pub fn box_message(
        title: &str,
        content: &[&str],
        color_func: fn(ColoredString) -> ColoredString,
    ) {
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
            match prompt(
                &format!("{} {} (按 {} 取消)", question, options, "Ctrl+C".yellow()),
                None,
            ) {
                Ok(choice) => {
                    let choice = choice.to_lowercase();
                    if choice == "y" {
                        return true;
                    }
                    if choice == "n" {
                        return false;
                    }
                    if choice.is_empty() {
                        return default_yes;
                    }
                    println!("{}", "无效输入，请输入 'y' 或 'n'。".red());
                }
                Err(_) => return false,
            }
        }
    }

    pub fn selection_menu(
        options: &[String],
        title: &str,
        instructions: &str,
        default_choice: &str,
    ) -> String {
        println!("\n┌{}┐", "─".repeat(constants::UI_WIDTH - 2));
        println!("  {}", title.cyan().bold());
        println!("├{}┤", "─".repeat(constants::UI_WIDTH - 2));

        let pad = options.len().to_string().len();
        for (i, option) in options.iter().enumerate() {
            println!(
                "  [{}] {}",
                format!("{:<pad$}", i + 1, pad = pad).yellow(),
                option
            );
        }

        println!("├{}┤", "─".repeat(constants::UI_WIDTH - 2));
        println!("  {} (按 {} 可取消)", instructions, "Ctrl+C".yellow());
        println!("└{}┘", "─".repeat(constants::UI_WIDTH - 2));

        prompt("请输入你的选择", Some(default_choice)).unwrap_or_default()
    }

    pub fn prompt_hidden(message: &str) -> io::Result<String> {
        print!("\n>>> {}: ", message);
        io::stdout().flush()?;
        rpassword::read_password()
    }
}

// --- 6. 配置与 Token 管理 ---
mod config {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    struct LocalConfig {
        accesstoken: Option<String>,
    }

    fn get_config_path() -> PathBuf {
        dirs::home_dir()
            .expect("无法获取用户主目录")
            .join(constants::CONFIG_DIR_NAME)
            .join(constants::CONFIG_FILE_NAME)
    }

    pub fn save_token(token: &str) {
        if token.is_empty() {
            return;
        }
        let config_path = get_config_path();
        let config_dir = config_path.parent().unwrap();
        if let Err(e) = fs::create_dir_all(config_dir) {
            eprintln!(
                "{} {}",
                "[!]".yellow(),
                format!("创建配置目录失败: {}", e).red()
            );
            return;
        }

        let mut config: LocalConfig = fs::read_to_string(&config_path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or(LocalConfig { accesstoken: None });

        config.accesstoken = Some(token.to_string());

        match serde_json::to_string_pretty(&config) {
            Ok(json_content) => {
                if let Err(e) = fs::write(&config_path, json_content) {
                    eprintln!("{} {}", "[X]".red(), format!("保存Token失败: {}", e).red());
                } else {
                    println!(
                        "{} {}",
                        "[i]".cyan(),
                        format!("Token已成功保存至: {:?}", config_path)
                    );
                }
            }
            Err(e) => {
                eprintln!("{} {}", "[X]".red(), format!("序列化配置失败: {}", e).red());
            }
        }
    }

    pub fn load_token_from_config() -> Option<String> {
        let config_path = get_config_path();
        if !config_path.is_file() {
            return None;
        }

        fs::read_to_string(config_path)
            .ok()
            .and_then(|content| serde_json::from_str::<LocalConfig>(&content).ok())
            .and_then(|config| config.accesstoken)
    }

    pub fn resolve_token(cli_token: Option<&str>) -> (Option<String>, String) {
        if let Some(token) = cli_token {
            if !token.is_empty() {
                return (Some(token.to_string()), "命令行参数".to_string());
            }
        }
        if let Ok(token) = env::var("ACCESS_TOKEN") {
            if !token.is_empty() {
                return (Some(token), "环境变量 (ACCESS_TOKEN)".to_string());
            }
        }
        if let Some(token) = load_token_from_config() {
            if !token.is_empty() {
                return (Some(token), "本地Token文件".to_string());
            }
        }
        (None, "未找到".to_string())
    }
}

// --- 7. 核心数据结构与类型 ---
#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    filepath: PathBuf,
    url: String,
    ti_md5: Option<String>,
    ti_size: Option<u64>,
    date: Option<DateTime<FixedOffset>>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DownloadStatus {
    Success,
    Skipped,
    Resumed,
    Md5Failed,
    SizeFailed,
    HttpError,
    NetworkError,
    ConnectionError,
    TimeoutError,
    TokenError,
    IoError,
    MergeError,
    KeyError,
    UnexpectedError,
}

#[derive(Debug, Clone)]
pub struct DownloadResult {
    filename: String,
    status: DownloadStatus,
    message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadAction {
    Skip,
    Resume,
    DownloadNew,
}

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
        let mut api_endpoints = HashMap::new();
        let mut url_templates = HashMap::new();

        url_templates.insert("TEXTBOOK_DETAILS".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/resources/tch_material/details/{resource_id}.json".into());
        url_templates.insert("TEXTBOOK_AUDIO".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrs/resources/{resource_id}/relation_audios.json".into());
        url_templates.insert("COURSE_QUALITY".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/resources/{resource_id}.json".into());
        url_templates.insert("COURSE_SYNC".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/national_lesson/resources/details/{resource_id}.json".into());
        url_templates.insert("CHAPTER_TREE".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/national_lesson/trees/{tree_id}.json".into());

        api_endpoints.insert(
            "tchMaterial".into(),
            ApiEndpointConfig {
                id_param: "contentId".into(),
                extractor: ResourceExtractorType::Textbook,
                url_template_keys: vec![("textbook".into(), "TEXTBOOK_DETAILS".into()), ("audio".into(), "TEXTBOOK_AUDIO".into())].into_iter().collect(),
            },
        );
        api_endpoints.insert(
            "qualityCourse".into(),
            ApiEndpointConfig {
                id_param: "courseId".into(),
                extractor: ResourceExtractorType::Course,
                url_template_keys: vec![("main".into(), "COURSE_QUALITY".into())].into_iter().collect(),
            },
        );
        api_endpoints.insert(
            "syncClassroom/classActivity".into(),
            ApiEndpointConfig {
                id_param: "activityId".into(),
                extractor: ResourceExtractorType::Course,
                url_template_keys: vec![("main".into(), "COURSE_SYNC".into())].into_iter().collect(),
            },
        );

        Self {
            max_workers: 5,
            default_audio_format: "mp3".into(),
            server_prefixes: vec!["s-file-1".into(), "s-file-2".into(), "s-file-3".into()],
            user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36".into(),
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
}

// --- 8. HTTP 客户端 ---
#[derive(Clone)]
pub struct RobustClient {
    client: ClientWithMiddleware,
    config: Arc<AppConfig>,
}

impl RobustClient {
    pub fn new(config: Arc<AppConfig>) -> Self {
        let retry_policy =
            ExponentialBackoff::builder().build_with_max_retries(config.max_retries);
        let client = ClientBuilder::new(
            reqwest::Client::builder()
                .user_agent(config.user_agent.clone())
                .connect_timeout(config.connect_timeout)
                .timeout(config.timeout)
                .pool_max_idle_per_host(config.max_workers * 3)
                .build()
                .unwrap(),
        )
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .build();

        Self { client, config }
    }

    pub async fn get<T: IntoUrl>(&self, url: T) -> AppResult<Response> {
        let res = self.client.get(url).send().await?;
        if res.status() == StatusCode::UNAUTHORIZED || res.status() == StatusCode::FORBIDDEN {
            return Err(AppError::TokenInvalid);
        }
        Ok(res.error_for_status()?)
    }

    pub async fn fetch_json(
        &self,
        url_template: &str,
        params: &[(&str, &str)],
    ) -> AppResult<Value> {
        let mut last_error = None;
        for prefix in &self.config.server_prefixes {
            let mut url = url_template.replace("{prefix}", prefix);
            for (key, val) in params {
                url = url.replace(&format!("{{{}}}", key), val);
            }
            match self.get(&url).await {
                Ok(res) => return Ok(res.json().await?),
                Err(e) => {
                    eprintln!(
                        "{} 服务器 '{}' 请求失败: {:?}",
                        "[!]".yellow(),
                        prefix,
                        e
                    );
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or(AppError::Other(anyhow!("所有服务器均请求失败"))))
    }
}

// --- 9. 资源提取器 ---
mod extractor {
    use super::*;
    use percent_encoding;

    #[async_trait]
    pub trait ResourceExtractor: Send + Sync {
        async fn extract_file_info(
            &self,
            resource_id: &str,
            context: &DownloadJobContext,
        ) -> AppResult<Vec<FileInfo>>;
    }

    pub struct TextbookExtractor {
        http_client: Arc<RobustClient>,
        config: Arc<AppConfig>,
    }
    impl TextbookExtractor {
        pub fn new(http_client: Arc<RobustClient>, config: Arc<AppConfig>) -> Self {
            Self { http_client, config }
        }

        fn extract_pdf_info(&self, data: &Value) -> (Vec<FileInfo>, Option<String>) {
            let mut results = vec![];
            let mut textbook_basename = None;

            let base_path = self.build_resource_path(data.get("tag_list"));
            if let Some(items) = data.get("ti_items").and_then(|i| i.as_array()) {
                for item in items {
                    if item.get("ti_file_flag").and_then(|f| f.as_str()) == Some("source")
                        && item.get("ti_format").and_then(|f| f.as_str()) == Some("pdf")
                    {
                        if let Some(url_str) = item
                            .get("ti_storages")
                            .and_then(|s| s.as_array())
                            .and_then(|a| a.get(0))
                            .and_then(|u| u.as_str())
                        {
                            if let Ok(url) = Url::parse(url_str) {
                                let path = Path::new(url.path());
                                let raw_filename = path
                                    .file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("unknown.pdf");
                                let decoded_filename =
                                    percent_encoding::percent_decode(raw_filename.as_bytes())
                                        .decode_utf8_lossy()
                                        .to_string();

                                let name = if self.is_generic_filename(&decoded_filename) {
                                    let title = data
                                        .get("global_title")
                                        .and_then(|t| t.get("zh-CN"))
                                        .and_then(|t| t.as_str())
                                        .or_else(|| data.get("title").and_then(|t| t.as_str()))
                                        .unwrap_or_else(|| {
                                            data.get("id")
                                                .and_then(|i| i.as_str())
                                                .unwrap_or("unknown_pdf")
                                        });
                                    format!("{}.pdf", utils::sanitize_filename(title))
                                } else {
                                    utils::sanitize_filename(&decoded_filename)
                                };

                                if textbook_basename.is_none() {
                                    textbook_basename = Some(
                                        Path::new(&name)
                                            .file_stem()
                                            .unwrap()
                                            .to_string_lossy()
                                            .to_string(),
                                    );
                                }

                                // .unwrap() is safe here because we construct the JSON value ourselves.
                                results.push(
                                    serde_json::from_value::<FileInfo>(serde_json::json!({
                                        "filepath": base_path.join(&name),
                                        "url": url_str,
                                        "ti_md5": item.get("ti_md5"),
                                        "ti_size": item.get("ti_size"),
                                        "date": data.get("update_time")
                                    }))
                                    .unwrap(),
                                );
                            } else {
                                eprintln!(
                                    "{} {}",
                                    "[!]".yellow(),
                                    format!("警告: 跳过一个无效的PDF URL: {}", url_str).red()
                                );
                            }
                        }
                    }
                }
            }
            (results, textbook_basename)
        }

        fn is_generic_filename(&self, filename: &str) -> bool {
            let patterns = [
                r"^pdf\.pdf$",
                r"^document\.pdf$",
                r"^file\.pdf$",
                r"^\d+\.pdf$",
                r"^[a-f0-9]{32}\.pdf$",
            ];
            patterns
                .iter()
                .any(|p| Regex::new(p).unwrap().is_match(filename.to_lowercase().as_str()))
        }

        async fn extract_audio_info(
            &self,
            resource_id: &str,
            base_path: PathBuf,
            textbook_basename: Option<String>,
            context: &DownloadJobContext,
        ) -> AppResult<Vec<FileInfo>> {
            let url_template = self.config.url_templates.get("TEXTBOOK_AUDIO").unwrap();
            let audio_data = self
                .http_client
                .fetch_json(url_template, &[("resource_id", resource_id)])
                .await?;

            if let Some(audio_items) = audio_data.as_array() {
                let available_formats = self.get_available_audio_formats(audio_items);
                if available_formats.is_empty() {
                    return Ok(vec![]);
                }

                let selected_formats: Vec<String> = if context.non_interactive
                    || &context.args.audio_format != &self.config.default_audio_format
                {
                    let preferred = context.args.audio_format.to_lowercase();
                    let chosen_format = if available_formats.contains(&preferred) {
                        preferred
                    } else {
                        available_formats[0].clone()
                    };
                    vec![chosen_format]
                } else {
                    let options: Vec<String> =
                        available_formats.iter().map(|f| f.to_uppercase()).collect();
                    let indices = utils::parse_selection_indices(
                        &ui::selection_menu(
                            &options,
                            "选择音频格式",
                            "支持格式: 1, 3, 2-4, all",
                            "1",
                        ),
                        options.len(),
                    );
                    indices
                        .iter()
                        .map(|&i| available_formats[i].clone())
                        .collect()
                };

                if selected_formats.is_empty() {
                    println!("{} 未选择任何音频格式，跳过音频下载。", "[i]".cyan());
                    return Ok(vec![]);
                }

                let mut results = vec![];
                let audio_path = textbook_basename
                    .map(|b| base_path.join(format!("{} - [audio]", b)))
                    .unwrap_or(base_path);

                for (i, item) in audio_items.iter().enumerate() {
                    let title = item
                        .get("global_title")
                        .and_then(|t| t.get("zh-CN"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("未知音频");
                    let base_name = format!("{:03} {}", i + 1, utils::sanitize_filename(title));

                    for format in &selected_formats {
                        if let Some(ti) = item
                            .get("ti_items")
                            .and_then(|v| v.as_array())
                            .and_then(|items| {
                                items.iter().find(|ti| {
                                    ti.get("ti_format").and_then(|f| f.as_str())
                                        == Some(format.as_str())
                                })
                            })
                        {
                            if let Some(url) = ti
                                .get("ti_storages")
                                .and_then(|v| v.as_array())
                                .and_then(|a| a.get(0))
                                .and_then(|v| v.as_str())
                            {
                                results.push(
                                    serde_json::from_value::<FileInfo>(serde_json::json!({
                                        "filepath": audio_path.join(format!("{}.{}", base_name, format)),
                                        "url": url,
                                        "ti_md5": ti.get("ti_md5"),
                                        "ti_size": ti.get("ti_size"),
                                        "date": item.get("update_time")
                                    }))
                                    .unwrap(),
                                );
                            }
                        }
                    }
                }
                Ok(results)
            } else {
                Ok(vec![])
            }
        }

        fn get_available_audio_formats(&self, data: &[Value]) -> Vec<String> {
            let mut formats = HashSet::new();
            for item in data {
                if let Some(ti_items) = item.get("ti_items").and_then(|v| v.as_array()) {
                    for ti in ti_items {
                        if let Some(format) = ti.get("ti_format").and_then(|v| v.as_str()) {
                            formats.insert(format.to_string());
                        }
                    }
                }
            }
            let mut sorted_formats: Vec<String> = formats.into_iter().collect();
            sorted_formats.sort();
            sorted_formats
        }

        fn build_resource_path(&self, tag_list_val: Option<&Value>) -> PathBuf {
            let template: HashMap<&str, &str> = [
                ("zxxxd", "未知学段"),
                ("zxxnj", "未知年级"),
                ("zxxxk", "未知学科"),
                ("zxxbb", "未知版本"),
                ("zxxcc", "未知册"),
            ]
            .iter()
            .cloned()
            .collect();
            let mut path_map = template.clone();

            if let Some(tags) = tag_list_val.and_then(|v| v.as_array()) {
                for tag in tags {
                    if let (Some(dim_id), Some(tag_name)) = (
                        tag.get("tag_dimension_id").and_then(|v| v.as_str()),
                        tag.get("tag_name").and_then(|v| v.as_str()),
                    ) {
                        if path_map.contains_key(dim_id) {
                            path_map.insert(dim_id, tag_name);
                        }
                    }
                }
            }

            let default_values: HashSet<&str> = template.values().cloned().collect();
            let components: Vec<String> = ["zxxxd", "zxxnj", "zxxxk", "zxxbb", "zxxcc"]
                .iter()
                .filter_map(|&key| path_map.get(key))
                .filter(|&&val| !default_values.contains(val))
                .map(|&name| utils::sanitize_filename(name))
                .collect();

            if components.is_empty() {
                PathBuf::from(constants::UNCLASSIFIED_DIR)
            } else {
                components.iter().collect()
            }
        }
    }

    #[async_trait]
    impl ResourceExtractor for TextbookExtractor {
        async fn extract_file_info(
            &self,
            resource_id: &str,
            context: &DownloadJobContext,
        ) -> AppResult<Vec<FileInfo>> {
            let url_template = self.config.url_templates.get("TEXTBOOK_DETAILS").unwrap();
            let data = self
                .http_client
                .fetch_json(url_template, &[("resource_id", resource_id)])
                .await?;

            let (mut pdf_files, textbook_basename) = self.extract_pdf_info(&data);
            let base_path = self.build_resource_path(data.get("tag_list"));

            let audio_files = self
                .extract_audio_info(resource_id, base_path, textbook_basename, context)
                .await?;

            pdf_files.extend(audio_files);
            Ok(pdf_files)
        }
    }

    pub struct CourseExtractor {
        http_client: Arc<RobustClient>,
        config: Arc<AppConfig>,
        chapter_resolver: ChapterTreeResolver,
        url_template: String,
    }

    impl CourseExtractor {
        pub fn new(
            http_client: Arc<RobustClient>,
            config: Arc<AppConfig>,
            url_template: String,
        ) -> Self {
            let chapter_resolver = ChapterTreeResolver::new(http_client.clone(), config.clone());
            Self {
                http_client,
                config,
                chapter_resolver,
                url_template,
            }
        }

        async fn get_base_directory(&self, data: &Value) -> PathBuf {
            let course_title = data
                .get("global_title")
                .and_then(|t| t.get("zh-CN"))
                .and_then(|v| v.as_str())
                .unwrap_or("未知课程");
            let textbook_path = TextbookExtractor::new(self.http_client.clone(), self.config.clone())
                .build_resource_path(data.get("tag_list"));

            let tree_id = data
                .get("custom_properties")
                .and_then(|p| p.get("teachingmaterial_info"))
                .and_then(|i| i.get("id"))
                .and_then(|v| v.as_str());
            let chapter_path_str = data
                .get("chapter_paths")
                .and_then(|p| p.as_array())
                .and_then(|a| a.get(0))
                .and_then(|v| v.as_str());

            let mut full_chapter_path = PathBuf::new();
            if let (Some(tree_id), Some(path_str)) = (tree_id, chapter_path_str) {
                if let Ok(path) = self
                    .chapter_resolver
                    .get_full_chapter_path(tree_id, path_str)
                    .await
                {
                    full_chapter_path = path;
                }
            }

            let course_title_sanitized = utils::sanitize_filename(course_title);
            let parent_path = if full_chapter_path
                .file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |s| utils::sanitize_filename(s) == course_title_sanitized)
            {
                full_chapter_path
                    .parent()
                    .unwrap_or(&full_chapter_path)
                    .to_path_buf()
            } else {
                full_chapter_path
            };

            textbook_path
                .join(parent_path)
                .join(utils::sanitize_filename(course_title))
        }

        fn get_teacher_map(&self, data: &Value) -> HashMap<usize, String> {
            let teacher_id_map: HashMap<String, String> = data
                .get("teacher_list")
                .and_then(|v| v.as_array())
                .map_or(HashMap::new(), |list| {
                    list.iter()
                        .filter_map(|t| {
                            let id = t.get("id")?.as_str()?.to_string();
                            let name = t.get("name")?.as_str()?.to_string();
                            Some((id, name))
                        })
                        .collect()
                });

            let mut resource_teacher_map = HashMap::new();
            if let Some(lessons) = data
                .get("resource_structure")
                .and_then(|s| s.get("relations"))
                .and_then(|r| r.as_array())
            {
                for lesson in lessons {
                    let teacher_names: Vec<String> = lesson
                        .get("custom_properties")
                        .and_then(|p| p.get("teacher_ids"))
                        .and_then(|v| v.as_array())
                        .map_or(vec![], |ids| {
                            ids.iter()
                                .filter_map(|id_val| {
                                    id_val
                                        .as_str()
                                        .and_then(|id_str| teacher_id_map.get(id_str).cloned())
                                })
                                .collect()
                        });
                    let teacher_name = if teacher_names.is_empty() {
                        "未知教师".to_string()
                    } else {
                        teacher_names.join(", ")
                    };

                    if let Some(refs) = lesson.get("res_ref").and_then(|v| v.as_array()) {
                        for ref_val in refs {
                            if let Some(ref_str) = ref_val.as_str() {
                                if let Ok(indices) = self.parse_res_ref(ref_str, 1000) {
                                    // Assume max resources
                                    for index in indices {
                                        resource_teacher_map.insert(index, teacher_name.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            resource_teacher_map
        }

        fn parse_res_ref(&self, ref_str: &str, max_index: usize) -> Result<Vec<usize>, ()> {
            if ref_str.contains("[*]") {
                return Ok((0..max_index).collect());
            }
            let re = Regex::new(r"\[([\d,]+)\]$").unwrap();
            if let Some(caps) = re.captures(ref_str) {
                let indices_str = &caps[1];
                let indices: Result<Vec<usize>, _> =
                    indices_str.split(',').map(|s| s.parse::<usize>()).collect();
                return indices.map_err(|_| ());
            }
            Err(())
        }

        async fn negotiate_video_qualities(
            &self,
            resources: &[Value],
            context: &DownloadJobContext,
        ) -> Option<Vec<String>> {
            let mut all_resolutions = BTreeSet::new();
            for res in resources {
                if let Some(streams) = self.get_available_video_streams(res.get("ti_items")) {
                    for stream in streams {
                        all_resolutions.insert(stream.resolution);
                    }
                }
            }

            if all_resolutions.is_empty() {
                return None;
            }
            let mut sorted_resolutions: Vec<String> = all_resolutions.into_iter().collect();
            sorted_resolutions.sort_by_key(|r| r.replace('p', "").parse::<u32>().unwrap_or(0));
            sorted_resolutions.reverse();

            if context.non_interactive || &context.args.video_quality != "best" {
                return Some(vec![context.args.video_quality.clone()]);
            }

            let indices = utils::parse_selection_indices(
                &ui::selection_menu(
                    &sorted_resolutions,
                    "选择视频清晰度",
                    "支持格式: 1, 3, 2-4, all",
                    "all",
                ),
                sorted_resolutions.len(),
            );

            if indices.is_empty() {
                None
            } else {
                Some(indices.iter().map(|&i| sorted_resolutions[i].clone()).collect())
            }
        }

        fn get_available_video_streams<'a>(
            &self,
            ti_items: Option<&'a Value>,
        ) -> Option<Vec<VideoStream>> {
            let streams: Vec<_> = ti_items?
                .as_array()?
                .iter()
                .filter_map(|item| {
                    if item.get("ti_format").and_then(Value::as_str)? == "m3u8" {
                        let height_str = item
                            .get("custom_properties")?
                            .get("requirements")?
                            .as_array()?
                            .iter()
                            .find_map(|p| {
                                if p.get("name").and_then(Value::as_str)? == "Height" {
                                    p.get("value").and_then(Value::as_str)
                                } else {
                                    None
                                }
                            })?;
                        let height = height_str.parse::<u32>().ok()?;
                        Some(VideoStream {
                            resolution: format!("{}p", height),
                            ti_item: item.clone(),
                        })
                    } else {
                        None
                    }
                })
                .collect();

            if streams.is_empty() {
                None
            } else {
                Some(streams)
            }
        }

        fn select_stream_non_interactive<'a>(
            &self,
            streams: &'a [VideoStream],
            quality: &str,
        ) -> Option<(&'a Value, &'a str)> {
            if streams.is_empty() {
                return None;
            }
            if quality == "best" {
                let best_stream = &streams[0];
                return Some((&best_stream.ti_item, &best_stream.resolution));
            }
            if quality == "worst" {
                let worst_stream = streams.last().unwrap();
                return Some((&worst_stream.ti_item, &worst_stream.resolution));
            }

            for stream in streams {
                if stream.resolution == quality {
                    return Some((&stream.ti_item, &stream.resolution));
                }
            }
            eprintln!(
                "{} 未找到指定清晰度 '{}'，将自动选择最高清晰度 '{}'。",
                "[!]".yellow(),
                quality,
                streams[0].resolution
            );
            let best_stream = &streams[0];
            Some((&best_stream.ti_item, &best_stream.resolution))
        }

        fn find_best_document_item<'a>(&self, ti_items: Option<&'a Value>) -> Option<&'a Value> {
            let items = ti_items?.as_array()?;
            items
                .iter()
                .find(|i| i.get("ti_format").and_then(Value::as_str) == Some("pdf"))
                .or_else(|| items.iter().find(|i| i.get("ti_storages").is_some()))
        }
    }

    #[derive(Debug)]
    struct VideoStream {
        resolution: String,
        ti_item: Value,
    }

    #[async_trait]
    impl ResourceExtractor for CourseExtractor {
        async fn extract_file_info(
            &self,
            resource_id: &str,
            _context: &DownloadJobContext,
        ) -> AppResult<Vec<FileInfo>> {
            let data = self
                .http_client
                .fetch_json(&self.url_template, &[("resource_id", resource_id)])
                .await?;

            let base_dir = self.get_base_directory(&data).await;
            let teacher_map = self.get_teacher_map(&data);

            let all_resources = data
                .get("relations")
                .and_then(|r| r.get("national_course_resource").or(r.get("course_resource")))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            if all_resources.is_empty() {
                println!("{} 未在该课程下找到任何资源。", "[!]".yellow());
                return Ok(vec![]);
            }

            let user_selected_qualities = self.negotiate_video_qualities(&all_resources, _context).await;
            let mut results = vec![];

            for (index, resource) in all_resources.iter().enumerate() {
                let title = utils::sanitize_filename(
                    resource
                        .get("global_title")
                        .and_then(|t| t.get("zh-CN"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("未知标题"),
                );
                let type_name = utils::sanitize_filename(
                    resource
                        .get("custom_properties")
                        .and_then(|p| p.get("alias_name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| {
                            resource
                                .get("resource_type_code_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                        }),
                );
                let teacher = teacher_map
                    .get(&index)
                    .cloned()
                    .unwrap_or_else(|| "未知教师".to_string());
                let date: Option<DateTime<FixedOffset>> = serde_json::from_value(
                    resource.get("update_time").cloned().unwrap_or(Value::Null),
                )
                .unwrap_or(None);

                let resource_type = resource.get("resource_type_code").and_then(|v| v.as_str());

                if resource_type == Some("assets_video") {
                    if let (Some(qualities), Some(mut streams)) = (
                        user_selected_qualities.as_ref(),
                        self.get_available_video_streams(resource.get("ti_items")),
                    ) {
                        streams.sort_by_key(|s| {
                            s.resolution.replace('p', "").parse::<u32>().unwrap_or(0)
                        });
                        streams.reverse();

                        for quality in qualities {
                            if let Some((ti_item, actual_quality)) =
                                self.select_stream_non_interactive(&streams, quality)
                            {
                                if let Some(url) = ti_item
                                    .get("ti_storages")
                                    .and_then(|v| v.as_array())
                                    .and_then(|a| a.get(0))
                                    .and_then(|v| v.as_str())
                                {
                                    let base_name =
                                        format!("{} - {} [{}]", title, type_name, actual_quality);
                                    let filename = format!("{} - [{}].ts", base_name, teacher);

                                    results.push(FileInfo {
                                        filepath: base_dir.join(filename),
                                        url: url.to_string(),
                                        ti_md5: ti_item
                                            .get("ti_md5")
                                            .and_then(|v| v.as_str())
                                            .map(String::from),
                                        ti_size: ti_item.get("ti_size").and_then(|v| v.as_u64()),
                                        date,
                                    });
                                }
                            }
                        }
                    }
                } else if ["assets_document", "coursewares", "lesson_plandesign"]
                    .contains(&resource_type.unwrap_or(""))
                {
                    if let Some(ti_item) = self.find_best_document_item(resource.get("ti_items")) {
                        if let Some(url) = ti_item
                            .get("ti_storages")
                            .and_then(|v| v.as_array())
                            .and_then(|a| a.get(0))
                            .and_then(|v| v.as_str())
                        {
                            let ext = ti_item
                                .get("ti_format")
                                .and_then(|v| v.as_str())
                                .unwrap_or("bin");
                            let filename =
                                format!("{} - {} - [{}].{}", title, type_name, teacher, ext);
                            results.push(FileInfo {
                                filepath: base_dir.join(filename),
                                url: url.to_string(),
                                ti_md5: ti_item
                                    .get("ti_md5")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                                ti_size: ti_item.get("ti_size").and_then(|v| v.as_u64()),
                                date,
                            });
                        }
                    }
                }
            }
            Ok(results)
        }
    }

    /// 章节树解析器，带缓存
    pub struct ChapterTreeResolver {
        http_client: Arc<RobustClient>,
        config: Arc<AppConfig>,
        cache: DashMap<String, (Value, SystemTime)>,
    }

    impl ChapterTreeResolver {
        pub fn new(http_client: Arc<RobustClient>, config: Arc<AppConfig>) -> Self {
            Self {
                http_client,
                config,
                cache: DashMap::new(),
            }
        }

        async fn get_tree_data(&self, tree_id: &str) -> AppResult<Value> {
            if let Some(entry) = self.cache.get(tree_id) {
                if entry.1.elapsed().unwrap_or_default() < Duration::from_secs(3600) {
                    return Ok(entry.0.clone());
                }
            }
            let url_template = self.config.url_templates.get("CHAPTER_TREE").unwrap();
            let data = self
                .http_client
                .fetch_json(url_template, &[("tree_id", tree_id)])
                .await?;
            self.cache
                .insert(tree_id.to_string(), (data.clone(), SystemTime::now()));
            Ok(data)
        }

        pub async fn get_full_chapter_path(
            &self,
            tree_id: &str,
            chapter_path_str: &str,
        ) -> AppResult<PathBuf> {
            let tree_data = self.get_tree_data(tree_id).await?;
            let lesson_node_id = chapter_path_str.split('/').last().unwrap_or("");

            let nodes_to_search =
                if let Some(nodes) = tree_data.get("child_nodes").and_then(|v| v.as_array()) {
                    nodes
                } else if let Some(nodes) = tree_data.as_array() {
                    nodes
                } else {
                    return Ok(PathBuf::new());
                };

            if let Some(path) = self.find_path_in_tree(nodes_to_search, lesson_node_id, vec![]) {
                Ok(path.iter().collect())
            } else {
                Ok(PathBuf::new())
            }
        }

        fn find_path_in_tree<'a>(
            &self,
            nodes: &'a [Value],
            target_id: &str,
            current_path: Vec<String>,
        ) -> Option<Vec<String>> {
            for node in nodes {
                let title = node.get("title").and_then(|v| v.as_str()).unwrap_or("未知章节");
                let mut new_path = current_path.clone();
                new_path.push(utils::sanitize_filename(title));

                if node.get("id").and_then(|v| v.as_str()) == Some(target_id) {
                    return Some(new_path);
                }

                if let Some(child_nodes) = node.get("child_nodes").and_then(|v| v.as_array()) {
                    if let Some(found_path) = self.find_path_in_tree(child_nodes, target_id, new_path)
                    {
                        return Some(found_path);
                    }
                }
            }
            None
        }
    }
}

// --- 10. 下载管理与执行 ---
mod downloader {
    use super::*;

    #[derive(Clone, Default)]
    pub struct DownloadStats {
        total: usize,
        success: usize,
        skipped: usize,
        failed: usize,
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
            let mut stats = self.stats.lock().unwrap();
            *stats = DownloadStats {
                total: total_tasks,
                ..Default::default()
            };
            self.failed_downloads.lock().unwrap().clear();
            self.skipped_downloads.lock().unwrap().clear();
        }

        pub fn record_success(&self) {
            self.stats.lock().unwrap().success += 1;
        }

        pub fn record_skip(&self, filename: &str, reason: &str) {
            self.stats.lock().unwrap().skipped += 1;
            self.skipped_downloads
                .lock()
                .unwrap()
                .push((filename.to_string(), reason.to_string()));
        }

        pub fn record_failure(&self, filename: &str, status: DownloadStatus) {
            self.stats.lock().unwrap().failed += 1;
            let (_, _, msg) = get_status_display_info(status);
            self.failed_downloads
                .lock()
                .unwrap()
                .push((filename.to_string(), msg.to_string()));
        }

        pub fn reset_token_failures(&self, filenames_to_reset: &[String]) {
            let mut failed_downloads = self.failed_downloads.lock().unwrap();
            let original_len = failed_downloads.len();
            failed_downloads.retain(|(name, _)| !filenames_to_reset.contains(name));
            let removed_count = original_len - failed_downloads.len();
            if removed_count > 0 {
                self.stats.lock().unwrap().failed -= removed_count;
            }
        }

        pub fn get_stats(&self) -> DownloadStats {
            self.stats.lock().unwrap().clone()
        }
        pub fn did_all_succeed(&self) -> bool {
            self.stats.lock().unwrap().failed == 0
        }

        pub fn print_report(&self) {
            let stats = self.get_stats();
            let skipped = self.skipped_downloads.lock().unwrap();
            let failed = self.failed_downloads.lock().unwrap();

            if !skipped.is_empty() || !failed.is_empty() {
                ui::print_sub_header("下载详情报告");
                if !skipped.is_empty() {
                    println!("\n{} 跳过的文件 ({}个):", "[i]".cyan(), stats.skipped);
                    print_grouped_report(&skipped);
                }
                if !failed.is_empty() {
                    println!("\n{} 失败的文件 ({}个):", "[X]".red(), stats.failed);
                    print_grouped_report(&failed);
                }
            }

            ui::print_sub_header("任务总结");
            if stats.success == 0 && stats.failed == 0 && stats.skipped > 0 {
                println!(
                    "{} 所有 {} 个文件均已存在且有效，无需操作。",
                    "[OK]".green(),
                    stats.total
                );
            } else {
                let summary = format!(
                    "{} | {} | {}",
                    format!("成功: {}", stats.success).green(),
                    format!("失败: {}", stats.failed).red(),
                    format!("跳过: {}", stats.skipped).yellow()
                );
                println!("{}", summary);
            }

            fn print_grouped_report(items: &[(String, String)]) {
                let mut grouped: HashMap<&String, Vec<&String>> = HashMap::new();
                for (filename, reason) in items {
                    grouped.entry(reason).or_default().push(filename);
                }
                let mut sorted_reasons: Vec<_> = grouped.keys().collect();
                sorted_reasons.sort();

                for reason in sorted_reasons {
                    println!("  - 原因: {}", reason);
                    let mut filenames = grouped.get(reason).unwrap().clone();
                    filenames.sort();
                    for filename in filenames {
                        println!("    - {}", filename);
                    }
                }
            }
        }
    }

    pub fn get_status_display_info(
        status: DownloadStatus,
    ) -> (ColoredString, fn(ColoredString) -> ColoredString, &'static str) {
        match status {
            DownloadStatus::Success => ("[OK]".green(), |s| s.green(), "下载并校验成功"),
            DownloadStatus::Resumed => ("[OK]".green(), |s| s.green(), "续传成功，文件有效"),
            DownloadStatus::Skipped => ("[i]".cyan(), |s| s.cyan(), "文件已存在，跳过"),
            DownloadStatus::Md5Failed => ("[X]".red(), |s| s.red(), "校验失败 (MD5不匹配)"),
            DownloadStatus::SizeFailed => ("[X]".red(), |s| s.red(), "校验失败 (大小不匹配)"),
            DownloadStatus::HttpError => ("[X]".red(), |s| s.red(), "服务器返回错误"),
            DownloadStatus::NetworkError => ("[X]".red(), |s| s.red(), "网络请求失败"),
            DownloadStatus::ConnectionError => ("[X]".red(), |s| s.red(), "无法建立连接"),
            DownloadStatus::TimeoutError => ("[!]".yellow(), |s| s.yellow(), "网络连接超时"),
            DownloadStatus::MergeError => ("[X]".red(), |s| s.red(), "视频分片合并失败"),
            DownloadStatus::KeyError => ("[X]".red(), |s| s.red(), "视频解密密钥获取失败"),
            DownloadStatus::TokenError => ("[X]".red(), |s| s.red(), "认证失败 (Token无效)"),
            DownloadStatus::IoError => ("[X]".red(), |s| s.red(), "本地文件读写错误"),
            DownloadStatus::UnexpectedError => ("[X]".red(), |s| s.red(), "发生未预期的程序错误"),
        }
    }

    pub struct ResourceDownloader {
        context: DownloadJobContext,
        m_progress: MultiProgress,
    }

    impl ResourceDownloader {
        pub fn new(context: DownloadJobContext) -> Self {
            Self {
                context,
                m_progress: MultiProgress::new(),
            }
        }

        pub async fn run(&self, url: &str) -> AppResult<bool> {
            let (extractor, resource_id) = self.get_extractor_info(url)?;
            self.prepare_and_run(extractor, &resource_id).await
        }

        pub async fn run_with_id(&self, resource_id: &str) -> AppResult<bool> {
            let r#type = self.context.args.r#type.as_ref().unwrap();
            let api_conf = self.context.config.api_endpoints.get(r#type).unwrap();
            let extractor = self.create_extractor(api_conf);
            self.prepare_and_run(extractor, resource_id).await
        }

        async fn prepare_and_run(
            &self,
            extractor: Box<dyn extractor::ResourceExtractor>,
            resource_id: &str,
        ) -> AppResult<bool> {
            let base_output_dir = self.context.args.output.clone();
            fs::create_dir_all(&base_output_dir)?;
            let absolute_path = dunce::canonicalize(&base_output_dir)?;
            println!(
                "\n{} 文件将保存到目录: \"{}\"",
                "[i]".cyan(),
                absolute_path.display()
            );

            let mut all_file_items = extractor.extract_file_info(resource_id, &self.context).await?;
            if all_file_items.is_empty() {
                println!("\n{} 未能提取到任何可下载的文件信息。", "[i]".cyan());
                return Ok(true);
            }

            for item in &mut all_file_items {
                item.filepath = utils::secure_join_path(&base_output_dir, &item.filepath)?;
            }

            let indices = if all_file_items.len() == 1 {
                let single_item = &all_file_items[0];
                let filename = single_item.filepath.file_name().unwrap().to_string_lossy();
                println!(
                    "\n{} 发现单个文件: '{}'，将直接开始下载。",
                    "[i]".cyan(),
                    filename
                );
                vec![0]
            } else {
                self.display_files_and_prompt_selection(&all_file_items)?
            };

            if indices.is_empty() {
                println!("\n{} 未选择任何文件，任务结束。", "[i]".cyan());
                return Ok(true);
            }

            let mut tasks_to_run: Vec<FileInfo> =
                indices.into_iter().map(|i| all_file_items[i].clone()).collect();
            tasks_to_run.sort_by_key(|item| item.ti_size.unwrap_or(0));

            let mut tasks_to_attempt = tasks_to_run.clone();

            self.context.manager.start_batch(tasks_to_attempt.len());

            loop {
                match self.execute_download_tasks(&tasks_to_attempt).await {
                    Ok(_) => break,
                    Err(AppError::TokenInvalid) => {
                        if self.context.non_interactive {
                            return Err(AppError::TokenInvalid);
                        }

                        let retry_result = self.handle_token_failure_and_retry(&tasks_to_run).await?;
                        if retry_result.should_abort {
                            return Ok(false);
                        }
                        if let Some(remaining) = retry_result.remaining_tasks {
                            tasks_to_attempt = remaining;
                        } else {
                            break;
                        }
                    }
                    Err(e) => return Err(e),
                }
            }

            self.context.manager.print_report();
            Ok(self.context.manager.did_all_succeed())
        }

        fn get_extractor_info(
            &self,
            url_str: &str,
        ) -> AppResult<(Box<dyn extractor::ResourceExtractor>, String)> {
            let url = Url::parse(url_str)?;
            for (path_key, api_conf) in &self.context.config.api_endpoints {
                if url.path().contains(path_key) {
                    if let Some(resource_id) =
                        url.query_pairs().find(|(k, _)| k == &api_conf.id_param)
                    {
                        let id = resource_id.1.to_string();
                        if utils::is_resource_id(&id) {
                            return Ok((self.create_extractor(api_conf), id));
                        }
                    }
                }
            }
            Err(AppError::Other(anyhow!(
                "无法识别的URL格式或不支持的资源类型。"
            )))
        }

        fn create_extractor(
            &self,
            api_conf: &ApiEndpointConfig,
        ) -> Box<dyn extractor::ResourceExtractor> {
            match api_conf.extractor {
                ResourceExtractorType::Textbook => Box::new(extractor::TextbookExtractor::new(
                    self.context.http_client.clone(),
                    self.context.config.clone(),
                )),
                ResourceExtractorType::Course => {
                    let template_key = api_conf
                        .url_template_keys
                        .get("main")
                        .expect("Course API config missing 'main' template key");
                    let url_template = self
                        .context
                        .config
                        .url_templates
                        .get(template_key)
                        .expect("URL template not found for key")
                        .clone();
                    Box::new(extractor::CourseExtractor::new(
                        self.context.http_client.clone(),
                        self.context.config.clone(),
                        url_template,
                    ))
                }
            }
        }

        async fn handle_token_failure_and_retry(
            &self,
            initial_tasks: &[FileInfo],
        ) -> AppResult<TokenRetryResult> {
            ui::box_message(
                "认证失败",
                &[
                    "当前 Access Token 已失效或无权限访问。",
                    "输入 '2' 可以查看获取 Token 的详细指南。",
                ],
                |s| s.red(),
            );
            loop {
                let prompt_msg = format!(
                    "选择操作: [1] 输入新 Token  [2] 查看帮助 (按 {} 中止)",
                    "Ctrl+C".yellow()
                );
                match ui::prompt(&prompt_msg, Some("1")) {
                    Ok(choice) if choice == "1" => {
                        match ui::prompt_hidden("请输入新 Token (输入不可见，完成后按回车)") {
                            Ok(new_token) if !new_token.is_empty() => {
                                *self.context.token.lock().await = new_token.clone();
                                if ui::confirm("是否保存此新 Token 以便后续使用?", false) {
                                    config::save_token(&new_token);
                                }
                                break;
                            }
                            _ => println!("{}", "Token 不能为空。".yellow()),
                        }
                    }
                    Ok(choice) if choice == "2" => {
                        ui::box_message(
                            "获取 Access Token 指南",
                            constants::HELP_TOKEN_GUIDE
                                .lines()
                                .collect::<Vec<_>>()
                                .as_slice(),
                            |s| s.cyan(),
                        );
                    }
                    Ok(_) => continue,
                    Err(_) => {
                        return Ok(TokenRetryResult {
                            remaining_tasks: None,
                            should_abort: true,
                        })
                    }
                }
            }

            println!("\n{} Token 已更新。正在检查剩余任务...", "[i]".cyan());
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
                println!("{} 所有任务均已完成，无需重试。", "[OK]".green());
                return Ok(TokenRetryResult {
                    remaining_tasks: None,
                    should_abort: false,
                });
            }

            self.context
                .manager
                .reset_token_failures(&remaining_filenames);
            println!(
                "{} 准备重试剩余的 {} 个任务...",
                "[i]".cyan(),
                remaining_tasks.len()
            );
            Ok(TokenRetryResult {
                remaining_tasks: Some(remaining_tasks),
                should_abort: false,
            })
        }

        fn display_files_and_prompt_selection(
            &self,
            items: &[FileInfo],
        ) -> AppResult<Vec<usize>> {
            let options: Vec<String> = items
                .iter()
                .map(|item| {
                    let date_str = item
                        .date
                        .map_or("[ 日期未知 ]".to_string(), |d| format!("[{}]", d.format("%Y-%m-%d")));
                    let filename_str = utils::truncate_text(
                        &item.filepath.file_name().unwrap().to_string_lossy(),
                        constants::FILENAME_TRUNCATE_LENGTH,
                    );
                    format!("{} {}", date_str, filename_str)
                })
                .collect();

            let default_sel = self.context.args.select.as_deref().unwrap_or("all");
            let user_input = if self.context.non_interactive {
                default_sel.to_string()
            } else {
                ui::selection_menu(
                    &options,
                    "文件下载列表",
                    "支持格式: 1, 3, 2-4, all",
                    default_sel,
                )
            };

            Ok(utils::parse_selection_indices(&user_input, items.len()))
        }

        async fn execute_download_tasks(&self, tasks: &[FileInfo]) -> AppResult<()> {
            let max_workers = min(self.context.config.max_workers, tasks.len());
            if max_workers == 0 {
                return Ok(());
            }

            println!(
                "\n{} 发现 {} 个待下载文件，启动 {} 个并发线程...",
                "[i]".cyan(),
                tasks.len(),
                max_workers
            );

            let task_id_counter = Arc::new(AtomicU64::new(0));
            let active_tasks = Arc::new(Mutex::new(BTreeMap::<u64, String>::new()));
            let main_pbar_style = ProgressStyle::with_template(
            "{prefix:7.bold.cyan} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos:>3}/{len:3} ({percent:>3}%) [ETA: {eta}]"
            )
            .expect("进度条模板无效")
            .progress_chars("#>-");
            let main_pbar = self.m_progress.add(ProgressBar::new(tasks.len() as u64));
            main_pbar.set_style(main_pbar_style.clone());
            main_pbar.set_prefix("总进度");
            main_pbar.enable_steady_tick(Duration::from_millis(100));

            let error_sender = Arc::new(TokioMutex::new(None::<AppError>));
            let tasks_stream = stream::iter(tasks.to_owned());

            let update_msg = |pbar: &ProgressBar, tasks: &Mutex<BTreeMap<u64, String>>| {
                let guard = tasks.lock().unwrap();
                let task_names: Vec<String> = guard.values().cloned().collect();
                pbar.set_message(task_names.join(", "));
            };
            update_msg(&main_pbar, &active_tasks);

            tasks_stream
                .for_each_concurrent(max_workers, |task| {
                    let context = self.context.clone();
                    let main_pbar = main_pbar.clone();
                    let m_progress = self.m_progress.clone();
                    let error_sender = error_sender.clone();
                    let task_id_counter = task_id_counter.clone();
                    let active_tasks = active_tasks.clone();

                    async move {
                        if error_sender.lock().await.is_some() {
                            return;
                        }

                        let task_id = task_id_counter.fetch_add(1, Ordering::Relaxed);
                        let task_name =
                            task.filepath.file_name().unwrap().to_string_lossy().to_string();

                        {
                            active_tasks
                                .lock()
                                .unwrap()
                                .insert(task_id, utils::truncate_text(&task_name, 30));
                            update_msg(&main_pbar, &active_tasks);
                        }

                        match Self::process_single_task(task.clone(), context.clone()).await {
                            Ok(result) => {
                                match result.status {
                                    DownloadStatus::Success | DownloadStatus::Resumed => {
                                        context.manager.record_success()
                                    }
                                    DownloadStatus::Skipped => context.manager.record_skip(
                                        &result.filename,
                                        result.message.as_deref().unwrap_or("文件已存在"),
                                    ),
                                    _ => context
                                        .manager
                                        .record_failure(&result.filename, result.status),
                                }

                                if result.status != DownloadStatus::Skipped {
                                    let (symbol, _, default_msg) =
                                        get_status_display_info(result.status);
                                    if let Some(err_msg) = result.message {
                                        m_progress
                                            .println(format!(
                                                "\n{} 任务 '{}' 失败: {} (详情: {})",
                                                symbol, task_name, default_msg, err_msg
                                            ))
                                            .unwrap();
                                    } else {
                                        m_progress
                                            .println(format!("{} {}", symbol, task_name))
                                            .unwrap();
                                    }
                                }
                            }
                            Err(e) => {
                                // 唯一可能到达这里的 Err 是 AppError::TokenInvalid
                                let mut error_lock = error_sender.lock().await;
                                if error_lock.is_none() {
                                    context.manager.record_failure(
                                        &task.filepath.to_string_lossy(),
                                        DownloadStatus::TokenError,
                                    );
                                    *error_lock = Some(e);
                                }
                            }
                        }

                        main_pbar.inc(1);

                        {
                            active_tasks.lock().unwrap().remove(&task_id);
                            update_msg(&main_pbar, &active_tasks);
                        }
                    }
                })
                .await;

            main_pbar.finish_and_clear();

            if let Some(err) = error_sender.lock().await.take() {
                return Err(err);
            }

            Ok(())
        }

        async fn process_single_task(
            item: FileInfo,
            context: DownloadJobContext,
        ) -> AppResult<DownloadResult> {
            let attempt_result: AppResult<DownloadResult> = async {
                if let Some(parent) = item.filepath.parent() {
                    fs::create_dir_all(parent)?;
                }

                let (action, resume_bytes, reason) =
                    Self::prepare_download_action(&item, &context.args)?;
                if action == DownloadAction::Skip {
                    return Ok(DownloadResult {
                        filename: item.filepath.file_name().unwrap().to_string_lossy().to_string(),
                        status: DownloadStatus::Skipped,
                        message: Some(reason),
                    });
                }

                let is_m3u8 = item.url.ends_with(".m3u8");
                let download_status = if is_m3u8 {
                    M3u8Downloader::new(context.clone()).download(&item).await?
                } else {
                    Self::download_standard_file(&item, resume_bytes, &context).await?
                };

                let final_status = if download_status == DownloadStatus::Success
                    || download_status == DownloadStatus::Resumed
                {
                    Self::finalize_and_validate(&item, is_m3u8)?
                } else {
                    download_status
                };

                Ok(DownloadResult {
                    filename: item.filepath.file_name().unwrap().to_string_lossy().to_string(),
                    status: final_status,
                    message: None,
                })
            }
            .await;

            match attempt_result {
                Ok(result) => Ok(result),
                Err(e @ AppError::TokenInvalid) => Err(e),
                Err(e) => {
                    let status = match &e {
                        AppError::Network(err)
                        | AppError::NetworkMiddleware(reqwest_middleware::Error::Reqwest(err)) => {
                            if err.is_timeout() {
                                DownloadStatus::TimeoutError
                            } else if err.is_connect() {
                                DownloadStatus::ConnectionError
                            } else if err.is_status() {
                                DownloadStatus::HttpError
                            } else {
                                DownloadStatus::NetworkError
                            }
                        }
                        AppError::NetworkMiddleware(_) => DownloadStatus::NetworkError,
                        AppError::Io(_) => DownloadStatus::IoError,
                        AppError::M3u8Parse(_) | AppError::Merge(_) => DownloadStatus::MergeError,
                        AppError::Security(_) => DownloadStatus::KeyError,
                        AppError::Validation(msg) => {
                            if msg.contains("MD5") {
                                DownloadStatus::Md5Failed
                            } else {
                                DownloadStatus::SizeFailed
                            }
                        }
                        _ => DownloadStatus::UnexpectedError,
                    };

                    Ok(DownloadResult {
                        filename: item.filepath.file_name().unwrap().to_string_lossy().to_string(),
                        status,
                        message: Some(e.to_string()),
                    })
                }
            }
        }

        fn prepare_download_action(
            item: &FileInfo,
            args: &Cli,
        ) -> AppResult<(DownloadAction, u64, String)> {
            if !item.filepath.exists() {
                return Ok((DownloadAction::DownloadNew, 0, "".to_string()));
            }
            if args.force_redownload {
                return Ok((DownloadAction::DownloadNew, 0, "".to_string()));
            }
            if item.url.ends_with(".m3u8") {
                return Ok((
                    DownloadAction::Skip,
                    0,
                    "视频已存在 (使用 -f 强制重下)".to_string(),
                ));
            }

            match Self::validate_file(&item.filepath, item.ti_md5.as_deref(), item.ti_size) {
                Ok(_) => Ok((DownloadAction::Skip, 0, "文件已存在且校验通过".to_string())),
                Err(_) => {
                    let actual_size = item.filepath.metadata()?.len();
                    if let Some(expected_size) = item.ti_size {
                        if actual_size > 0 && actual_size < expected_size {
                            return Ok((DownloadAction::Resume, actual_size, "".to_string()));
                        }
                    }
                    println!(
                        "{} {} - 文件已存在但校验失败，将重新下载。",
                        "[!]".yellow(),
                        item.filepath.display()
                    );
                    Ok((DownloadAction::DownloadNew, 0, "".to_string()))
                }
            }
        }

        fn finalize_and_validate(item: &FileInfo, is_m3u8: bool) -> AppResult<DownloadStatus> {
            if !item.filepath.exists() || item.filepath.metadata()?.len() == 0 {
                return Ok(DownloadStatus::SizeFailed);
            }
            if is_m3u8 {
                return Ok(DownloadStatus::Success);
            }

            match Self::validate_file(&item.filepath, item.ti_md5.as_deref(), item.ti_size) {
                Ok(_) => Ok(DownloadStatus::Success),
                Err(e @ AppError::Validation(_)) => Err(e),
                Err(e) => Err(e),
            }
        }

        fn validate_file(
            filepath: &Path,
            expected_md5: Option<&str>,
            expected_size: Option<u64>,
        ) -> AppResult<()> {
            if let Some(size) = expected_size {
                if filepath.metadata()?.len() != size {
                    return Err(AppError::Validation("大小不匹配".to_string()));
                }
            }
            if let Some(md5) = expected_md5 {
                let actual_md5 = utils::calculate_file_md5(filepath)?;
                if !actual_md5.eq_ignore_ascii_case(md5) {
                    return Err(AppError::Validation("MD5不匹配".to_string()));
                }
            }
            Ok(())
        }

        async fn download_standard_file(
            item: &FileInfo,
            resume_from: u64,
            context: &DownloadJobContext,
        ) -> AppResult<DownloadStatus> {
            let mut current_resume_from = resume_from;

            loop {
                let mut url = Url::parse(&item.url)?;
                let token = context.token.lock().await;
                if !token.is_empty() {
                    url.query_pairs_mut().append_pair("accessToken", &token);
                }

                let mut request_builder = context.http_client.client.get(url.clone());
                if current_resume_from > 0 {
                    request_builder = request_builder
                        .header(header::RANGE, format!("bytes={}-", current_resume_from));
                }

                drop(token);

                let res = request_builder.send().await?;

                if res.status() == StatusCode::RANGE_NOT_SATISFIABLE {
                    println!(
                        "{} 续传点无效，将从头开始下载: {}",
                        "[!]".yellow(),
                        &item.filepath.display()
                    );
                    current_resume_from = 0;
                    if item.filepath.exists() {
                        fs::remove_file(&item.filepath)?;
                    }
                    continue;
                }
                if res.status() == StatusCode::UNAUTHORIZED || res.status() == StatusCode::FORBIDDEN
                {
                    return Err(AppError::TokenInvalid);
                }
                let res = res.error_for_status()?;

                let mut file = if current_resume_from > 0 {
                    OpenOptions::new()
                        .write(true)
                        .append(true)
                        .open(&item.filepath)?
                } else {
                    File::create(&item.filepath)?
                };

                let mut stream = res.bytes_stream();

                while let Some(chunk_res) = stream.next().await {
                    let chunk = chunk_res?;
                    file.write_all(&chunk)?;
                }

                return Ok(if current_resume_from > 0 {
                    DownloadStatus::Resumed
                } else {
                    DownloadStatus::Success
                });
            }
        }
    }

    struct M3u8Downloader {
        context: DownloadJobContext,
    }

    impl M3u8Downloader {
        fn new(context: DownloadJobContext) -> Self {
            Self { context }
        }

        async fn download(&self, item: &FileInfo) -> AppResult<DownloadStatus> {
            let mut url = Url::parse(&item.url)?;
            let token = self.context.token.lock().await;
            if !token.is_empty() {
                url.query_pairs_mut().append_pair("accessToken", &token);
            }
            drop(token); // 释放锁

            let (key, iv, playlist) = self.get_m3u8_key_and_playlist(url.clone()).await?;

            if playlist.segments.is_empty() {
                return Err(AppError::M3u8Parse("M3U8文件不含分片".to_string()));
            }
            let segment_urls: Vec<String> = playlist.segments.iter().map(|s| s.uri.clone()).collect();

            let decryptor = if let (Some(key), Some(iv_hex)) = (key, iv) {
                let iv_bytes = hex::decode(iv_hex.trim_start_matches("0x"))
                    .map_err(|e| AppError::M3u8Parse(format!("无效的IV十六进制值: {}", e)))?;
                Some(
                    Aes128CbcDec::new_from_slices(&key, &iv_bytes)
                        .map_err(|e| AppError::Security(format!("AES解密器初始化失败: {}", e)))?,
                )
            } else {
                None
            };

            let temp_dir = tempfile::Builder::new().prefix("m3u8_dl_").tempdir()?;

            self.download_segments_with_retry(&url, &segment_urls, temp_dir.path(), decryptor)
                .await?;

            self.merge_ts_segments(temp_dir.path(), segment_urls.len(), &item.filepath)?;
            Ok(DownloadStatus::Success)
        }

        async fn download_segments_with_retry(
            &self,
            base_url: &Url,
            urls: &[String],
            temp_path: &Path,
            decryptor: Option<Aes128CbcDec>,
        ) -> AppResult<()> {
            let mut failed_indices: Vec<usize> = (0..urls.len()).collect();

            for attempt in 0..=self.context.config.max_retries {
                if failed_indices.is_empty() {
                    break;
                }
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }

                let stream = stream::iter(failed_indices.clone())
                    .map(|i| {
                        let url = base_url.join(&urls[i]).unwrap();
                        let ts_path = temp_path.join(format!("{:05}.ts", i));
                        let client = self.context.http_client.clone();
                        let decryptor = decryptor.clone();
                        tokio::spawn(async move {
                            Self::download_ts_segment(client, url, &ts_path, decryptor)
                                .await
                                .map_err(|_| i)
                        })
                    })
                    .buffer_unordered(self.context.config.max_workers * 2);

                let results: Vec<_> = stream.collect().await;
                failed_indices = results.into_iter().filter_map(|res| res.unwrap().err()).collect();
            }

            if !failed_indices.is_empty() {
                return Err(AppError::Merge(format!(
                    "{} 个分片最终下载失败",
                    failed_indices.len()
                )));
            }
            Ok(())
        }

        async fn download_ts_segment(
            client: Arc<RobustClient>,
            url: Url,
            ts_path: &Path,
            decryptor: Option<Aes128CbcDec>,
        ) -> AppResult<()> {
            let data = client.get(url).await?.bytes().await?;
            let final_data = if let Some(d) = decryptor {
                d.decrypt_padded_vec_mut::<Pkcs7>(&data)
                    .map_err(|e| AppError::Security(format!("分片解密失败: {}", e)))?
            } else {
                data.to_vec()
            };
            fs::write(ts_path, &final_data)?;
            Ok(())
        }

        fn merge_ts_segments(
            &self,
            temp_dir: &Path,
            num_segments: usize,
            output_path: &Path,
        ) -> AppResult<()> {
            let temp_output_path = output_path.with_extension("tmp");
            let mut writer = BufWriter::new(File::create(&temp_output_path)?);
            for i in 0..num_segments {
                let ts_path = temp_dir.join(format!("{:05}.ts", i));
                if !ts_path.exists() {
                    return Err(AppError::Merge(format!(
                        "丢失视频分片: {:?}",
                        ts_path.file_name().unwrap()
                    )));
                }
                let mut reader = File::open(ts_path)?;
                io::copy(&mut reader, &mut writer)?;
            }
            writer.flush()?;
            fs::rename(temp_output_path, output_path)?;
            Ok(())
        }

        async fn get_m3u8_key_and_playlist(
            &self,
            m3u8_url: Url,
        ) -> AppResult<(Option<Vec<u8>>, Option<String>, m3u8_rs::MediaPlaylist)> {
            let playlist_text = self.context.http_client.get(m3u8_url.clone()).await?.text().await?;
            let playlist = m3u8_rs::parse_playlist_res(playlist_text.as_bytes())
                .map_err(|e| AppError::M3u8Parse(e.to_string()))?;

            if let m3u8_rs::Playlist::MediaPlaylist(media) = playlist {
                let key_info = media.segments.iter().find_map(|seg| {
                    seg.key.as_ref().and_then(|k| {
                        if let m3u8_rs::Key {
                            uri: Some(uri),
                            iv,
                            ..
                        } = k
                        {
                            Some((uri.clone(), iv.clone()))
                        } else {
                            None
                        }
                    })
                });

                if let Some((uri, iv)) = key_info {
                    let key_url = m3u8_url.join(&uri)?;
                    let nonce_url = format!("{}/signs", key_url);
                    let signs_data: Value = self.context.http_client.get(&nonce_url).await?.json().await?;
                    let nonce = signs_data
                        .get("nonce")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            AppError::M3u8Parse("密钥服务器响应中未找到 'nonce'".to_string())
                        })?;

                    let key_filename = key_url
                        .path_segments()
                        .and_then(|segments| segments.last())
                        .ok_or_else(|| {
                            AppError::M3u8Parse(format!("无法从密钥URL中提取文件名: {}", key_url))
                        })?;

                    let sign_material = format!("{}{}", nonce, key_filename);
                    let mut hasher = Md5::new();
                    hasher.update(sign_material.as_bytes());
                    let result = hasher.finalize();
                    let sign = &format!("{:x}", result)[..16];

                    let final_key_url = format!("{}?nonce={}&sign={}", key_url, nonce, sign);
                    let key_data: Value =
                        self.context.http_client.get(&final_key_url).await?.json().await?;
                    let encrypted_key_b64 = key_data
                        .get("key")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            AppError::M3u8Parse("密钥服务器响应中未找到加密密钥 'key'".to_string())
                        })?;

                    let encrypted_key = BASE64.decode(encrypted_key_b64)?;
                    type EcbDec = ecb::Decryptor<aes::Aes128>;
                    let cipher = EcbDec::new(sign.as_bytes().into());
                    let decrypted_key = cipher
                        .decrypt_padded_vec_mut::<Pkcs7>(&encrypted_key)
                        .map_err(|e| AppError::Security(format!("AES密钥解密失败: {}", e)))?;

                    return Ok((Some(decrypted_key), iv, media));
                }
                return Ok((None, None, media));
            }
            Err(AppError::M3u8Parse(
                "预期的M3U8文件不是媒体播放列表".to_string(),
            ))
        }
    }

    pub struct TokenRetryResult {
        pub remaining_tasks: Option<Vec<FileInfo>>,
        pub should_abort: bool,
    }
}

// --- 11. 命令行接口 (CLI) ---
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
struct Cli {
    // --- Mode ---
    /// 启动交互式会话，逐一输入链接
    #[arg(short, long, action = clap::ArgAction::SetTrue, help_heading = "Mode")]
    interactive: bool,
    /// 指定要下载的单个资源链接
    #[arg(long, help_heading = "Mode")]
    url: Option<String>,
    /// 通过资源ID下载 (需配合 --type 使用)
    #[arg(long, help_heading = "Mode")]
    id: Option<String>,
    /// 从文本文件批量下载多个链接或ID
    #[arg(short, long, value_name = "FILE", help_heading = "Mode")]
    batch_file: Option<PathBuf>,
    /// 显示如何获取 Access Token 的指南并退出
    #[arg(long, action = clap::ArgAction::SetTrue, help_heading = "Mode")]
    token_help: bool,

    // --- Options ---
    /// [非交互] 指定下载项 (如 '1-5,8', 'all')
    #[arg(long, value_name = "SELECTION", help_heading = "Options")]
    select: Option<String>,
    /// [ID模式] 指定资源类型
    #[arg(long, help = "有效选项: tchMaterial, qualityCourse, syncClassroom/classActivity", help_heading = "Options")]
    r#type: Option<String>,
    /// 提供访问令牌(Access Token)
    #[arg(long, help_heading = "Options")]
    token: Option<String>,
    /// 强制重新下载已存在的文件
    #[arg(short, long, action = clap::ArgAction::SetTrue, help_heading = "Options")]
    force_redownload: bool,
    /// 选择视频清晰度: 'best', 'worst', 或 '720p' 等
    #[arg(short='q', long, default_value = "best", help_heading = "Options")]
    video_quality: String,
    /// 选择音频格式: 'mp3', 'm4a' 等
    #[arg(long, default_value = "mp3", help_heading = "Options")]
    audio_format: String,
    /// [批量模式] 为每个任务提供手动选择
    #[arg(long, action = clap::ArgAction::SetTrue, help_heading = "Options")]
    prompt_each: bool,
    /// 设置最大并发下载数
    #[arg(short, long, value_parser = clap::value_parser!(usize), help_heading = "Options")]
    workers: Option<usize>,
    /// 设置文件保存目录
    #[arg(short, long, value_name = "DIR", default_value_os_t = PathBuf::from(constants::DEFAULT_SAVE_DIR), help_heading = "Options")]
    output: PathBuf,

    // --- General ---
    /// 显示此帮助信息并退出
    #[arg(short = 'h', long, action = clap::ArgAction::Help, global = true, help_heading = "General")]
    help: Option<bool>,
    /// 显示版本信息并退出
    #[arg(short = 'V', long, action = clap::ArgAction::Version, global = true, help_heading = "General")]
    version: Option<bool>,
}

// --- 12. 主程序与执行流程 ---

/// 核心的执行函数，由 `main` 调用
async fn run_from_cli(args: Arc<Cli>) -> AppResult<()> {
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
        manager: downloader::DownloadManager::new(),
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
        downloader::ResourceDownloader::new(context).run(url).await?
    } else if let Some(id) = &args.id {
        downloader::ResourceDownloader::new(context).run_with_id(id).await?
    } else {
        true // Clap group rule prevents this
    };

    if !all_ok {
        Err(AppError::Other(anyhow!("一个或多个任务执行失败。")))
    } else {
        Ok(())
    }
}

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
    let content = match fs::read_to_string(batch_file) {
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
        downloader::ResourceDownloader::new(context)
            .run_with_id(task_input)
            .await
    } else if Url::parse(task_input).is_ok() {
        downloader::ResourceDownloader::new(context).run(task_input).await
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

#[tokio::main]
async fn main() {
    // 为 Windows 终端启用 ANSI 颜色支持
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