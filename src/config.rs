// src/config.rs

use crate::{
    cli::Cli,
    constants,
    error::{AppError, AppResult},
};
use anyhow::{anyhow, Context};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, fs, path::PathBuf, time::Duration};

/// 全局应用配置
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

impl AppConfig {
    pub fn from_args(args: &Cli) -> Self {
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
                "tchMaterial".into(),
                ApiEndpointConfig {
                    id_param: "contentId".into(),
                    extractor: ResourceExtractorType::Textbook,
                    url_template_keys: HashMap::from([
                        ("textbook".into(), "TEXTBOOK_DETAILS".into()),
                        ("audio".into(), "TEXTBOOK_AUDIO".into()),
                    ]),
                },
            ),
            (
                "qualityCourse".into(),
                ApiEndpointConfig {
                    id_param: "courseId".into(),
                    extractor: ResourceExtractorType::Course,
                    url_template_keys: HashMap::from([("main".into(), "COURSE_QUALITY".into())]),
                },
            ),
            (
                "syncClassroom/classActivity".into(),
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
    pub id_param: String,
    pub extractor: ResourceExtractorType,
    pub url_template_keys: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
pub enum ResourceExtractorType {
    Textbook,
    Course,
}

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
    if token.is_empty() {
        return Ok(());
    }

    let config_path = get_config_path()?;
    let config_dir = config_path
        .parent()
        .ok_or_else(|| anyhow!("无法获取配置文件的父目录"))?;
    fs::create_dir_all(config_dir)
        .with_context(|| format!("创建配置目录 '{}' 失败", config_dir.display()))?;

    let mut config: LocalConfig = fs::read_to_string(&config_path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or(LocalConfig { accesstoken: None });

    config.accesstoken = Some(token.to_string());

    let json_content =
        serde_json::to_string_pretty(&config).context("序列化Token配置失败")?;

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