// src/config.rs (正确版本)

use crate::{
    cli::Cli,
    constants,
    error::{AppError, AppResult},
};
use anyhow::{anyhow, Context};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, path::PathBuf, time::Duration};

// ===================================================================
// 1. 定义与 config.json 对应的结构体
// ===================================================================

/// 对应 config.json 中 "api_endpoints" 部分的结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiEndpointConfigFromFile {
    pub id_param: String,
    pub extractor: String, // 从 JSON 读取时是字符串
    pub url_template_keys: HashMap<String, String>,
}

/// 对应整个 config.json 文件的顶层结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accesstoken: Option<String>,
    pub url_templates: HashMap<String, String>,
    pub api_endpoints: HashMap<String, ApiEndpointConfigFromFile>,
}

impl ExternalConfig {
    /// 创建一个包含默认应用配置的实例
    fn default_app_config() -> Self {
        let url_templates = HashMap::from([
            ("TEXTBOOK_DETAILS".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/resources/tch_material/details/{resource_id}.json".into()),
            ("TEXTBOOK_AUDIO".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrs/resources/{resource_id}/relation_audios.json".into()),
            ("COURSE_QUALITY".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/resources/{resource_id}.json".into()),
            ("COURSE_SYNC".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/national_lesson/resources/details/{resource_id}.json".into()),
            ("CHAPTER_TREE".into(), "https://{prefix}.ykt.cbern.com.cn/zxx/ndrv2/national_lesson/trees/{tree_id}.json".into()),
        ]);

        let api_endpoints = HashMap::from([
            (
                "tchMaterial".into(),
                ApiEndpointConfigFromFile {
                    id_param: "contentId".into(),
                    extractor: "Textbook".into(),
                    url_template_keys: HashMap::from([
                        ("textbook".into(), "TEXTBOOK_DETAILS".into()),
                        ("audio".into(), "TEXTBOOK_AUDIO".into()),
                    ]),
                },
            ),
            (
                "qualityCourse".into(),
                ApiEndpointConfigFromFile {
                    id_param: "courseId".into(),
                    extractor: "Course".into(),
                    url_template_keys: HashMap::from([("main".into(), "COURSE_QUALITY".into())]),
                },
            ),
            (
                "syncClassroom/classActivity".into(),
                ApiEndpointConfigFromFile {
                    id_param: "activityId".into(),
                    extractor: "Course".into(),
                    url_template_keys: HashMap::from([("main".into(), "COURSE_SYNC".into())]),
                },
            ),
        ]);

        Self {
            accesstoken: None,
            url_templates,
            api_endpoints,
        }
    }
}

// ===================================================================
// 2. 定义程序内部使用的配置结构体
// ===================================================================

/// 内部使用的 API 端点配置，extractor 是强类型的枚举
#[derive(Debug, Clone)]
pub struct ApiEndpointConfig {
    pub id_param: String,
    pub extractor: ResourceExtractorType,
    pub url_template_keys: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
pub enum ResourceExtractorType {
    Textbook,
    Course,
}

/// 全局应用配置，这是程序运行时真正使用的配置对象
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub max_workers: usize,
    pub default_audio_format: String,
    pub server_prefixes: Vec<String>,
    pub user_agent: String,
    pub connect_timeout: Duration,
    pub timeout: Duration,
    pub max_retries: u32,
    pub api_endpoints: HashMap<String, ApiEndpointConfig>,
    pub url_templates: HashMap<String, String>,
}

// ===================================================================
// 3. 实现新的配置加载和创建逻辑
// ===================================================================

impl AppConfig {
    /// 统一构造函数：加载配置、应用命令行参数
    pub fn new(args: &Cli) -> AppResult<Self> {
        let external_config = load_or_create_external_config()?;

        // 将从文件读取的字符串 extractor 转换为内部使用的强类型枚举
        let api_endpoints = external_config
            .api_endpoints
            .into_iter()
            .map(|(key, file_config)| {
                let extractor_type = match file_config.extractor.as_str() {
                    "Textbook" => Ok(ResourceExtractorType::Textbook),
                    "Course" => Ok(ResourceExtractorType::Course),
                    _ => Err(anyhow!("未知的提取器类型: '{}' in config.json", file_config.extractor)),
                }?;
                
                Ok((
                    key,
                    ApiEndpointConfig {
                        id_param: file_config.id_param,
                        extractor: extractor_type,
                        url_template_keys: file_config.url_template_keys,
                    },
                ))
            })
            .collect::<AppResult<HashMap<_, _>>>()?;

        // 从硬编码的默认值和命令行参数构建最终的 AppConfig
        Ok(Self {
            max_workers: args.workers.unwrap_or(5),
            default_audio_format: args.audio_format.clone(),
            server_prefixes: vec!["s-file-1".into(), "s-file-2".into(), "s-file-3".into()],
            user_agent: constants::USER_AGENT.into(),
            connect_timeout: Duration::from_secs(5),
            timeout: Duration::from_secs(15),
            max_retries: 3,
            api_endpoints, // <-- 来自文件
            url_templates: external_config.url_templates, // <-- 来自文件
        })
    }
}

// ===================================================================
// 4. 实现文件 I/O 和 Token 管理
// ===================================================================

fn get_config_path() -> AppResult<PathBuf> {
    let path = dirs::home_dir()
        .ok_or_else(|| AppError::Other(anyhow!("无法获取用户主目录")))?
        .join(constants::CONFIG_DIR_NAME)
        .join(constants::CONFIG_FILE_NAME);
    Ok(path)
}

/// 加载或创建外部配置
fn load_or_create_external_config() -> AppResult<ExternalConfig> {
    let config_path = get_config_path()?;
    if config_path.is_file() {
        // 文件存在，直接读取和解析
        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("读取配置文件 '{}' 失败", config_path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("解析配置文件 '{}' 失败", config_path.display()))
            .map_err(AppError::from)
    } else {
        // 文件不存在，创建带有默认应用配置的新实例
        info!("配置文件 {:?} 不存在，将创建默认配置。", config_path);
        let config = ExternalConfig::default_app_config();
        
        // 确保目录存在
        if let Some(dir) = config_path.parent() {
            fs::create_dir_all(dir)?;
        }

        // 将默认配置写入文件
        let json_content = serde_json::to_string_pretty(&config)?;
        fs::write(&config_path, json_content)?;
        
        Ok(config)
    }
}

/// 保存 Token 到统一的配置文件
pub fn save_token(token: &str) -> AppResult<()> {
    if token.is_empty() {
        return Ok(());
    }

    let config_path = get_config_path()?;
    let mut config = load_or_create_external_config()?;
    
    config.accesstoken = Some(token.to_string());

    let json_content = serde_json::to_string_pretty(&config)?;
    fs::write(&config_path, json_content)
        .with_context(|| format!("保存Token到 '{}' 失败", config_path.display()))?;

    info!("用户已将 Token 保存至配置文件: {}", config_path.display());
    println!(
        "{} Token已成功保存至: {}",
        *crate::symbols::INFO,
        config_path.display()
    );

    Ok(())
}

/// 从统一的配置文件加载 Token
pub fn load_token_from_config() -> Option<String> {
    load_or_create_external_config().ok().and_then(|config| config.accesstoken)
}

/// 解析 Token 的来源
pub fn resolve_token(cli_token: Option<&str>) -> (Option<String>, String) {
    if let Some(token) = cli_token {
        if !token.is_empty() {
            debug!("使用来自命令行参数的 Token");
            return (Some(token.to_string()), "命令行参数".to_string());
        }
    }
    if let Ok(token) = std::env::var("ACCESS_TOKEN") {
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