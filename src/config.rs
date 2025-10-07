// src/config.rs

use crate::{cli::Cli, constants};
use colored::Colorize;
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