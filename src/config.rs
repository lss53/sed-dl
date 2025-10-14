// src/config.rs

pub mod token; // 声明子模块

use self::token::load_or_create_external_config;
use crate::{
    cli::Cli,
    constants,
    error::AppResult,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration}; // 明确地从子模块导入函数

// ===================================================================
// 1. 定义与 config.json 对应的结构体
// ===================================================================
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")] // 这样 "Textbook" 就能正确映射到 Textbook
pub enum ExtractorTypeForFile {
    Textbook,
    Course,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiEndpointConfigFromFile {
    pub id_param: String,
    pub extractor: ExtractorTypeForFile, // <-- 类型从 String 变为 ExtractorTypeForFile
    pub url_template_keys: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkConfig {
    pub server_prefixes: Option<Vec<String>>,
    pub connect_timeout_secs: Option<u64>,
    pub timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accesstoken: Option<String>,
    #[serde(default)] // 这是个好习惯，可以防止手动删除配置项导致解析失败
    pub network: NetworkConfig,
    pub url_templates: HashMap<String, String>,
    pub api_endpoints: HashMap<String, ApiEndpointConfigFromFile>,
}

impl ExternalConfig {
    pub(crate) fn default_app_config() -> Self {
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
                    extractor: ExtractorTypeForFile::Textbook,
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
                    extractor: ExtractorTypeForFile::Course,
                    url_template_keys: HashMap::from([("main".into(), "COURSE_QUALITY".into())]),
                },
            ),
            (
                "syncClassroom/classActivity".into(),
                ApiEndpointConfigFromFile {
                    id_param: "activityId".into(),
                    extractor: ExtractorTypeForFile::Course,
                    url_template_keys: HashMap::from([("main".into(), "COURSE_SYNC".into())]),
                },
            ),
        ]);

        Self {
            accesstoken: None,
            network: NetworkConfig::default(), // 使用 NetworkConfig 的默认空值
            url_templates,
            api_endpoints,
        }
    }
}

// ===================================================================
// 2. 定义程序内部使用的配置结构体
// ===================================================================

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

// AppConfig 结构体定义
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

#[cfg(feature = "testing")]  // 标记这个实现只在测试时编译
impl Default for AppConfig {
    fn default() -> Self {
        Self {
            max_workers: 5,
            default_audio_format: "mp3".to_string(),
            server_prefixes: vec!["s-file-1".to_string()],
            user_agent: "test-agent/1.0".to_string(),
            connect_timeout: Duration::from_secs(5),
            timeout: Duration::from_secs(15),
            max_retries: 3,
            api_endpoints: HashMap::new(),
            url_templates: HashMap::new(),
        }
    }
}

// ===================================================================
// 3. 实现新的 AppConfig 构造函数
// ===================================================================

impl AppConfig {
    pub fn new(args: &Cli) -> AppResult<Self> {
        let external_config = load_or_create_external_config()?;

        let api_endpoints = external_config
            .api_endpoints
            .into_iter()
            .map(|(key, file_config)| {
                let extractor_type = match file_config.extractor {
                    ExtractorTypeForFile::Textbook => ResourceExtractorType::Textbook,
                    ExtractorTypeForFile::Course => ResourceExtractorType::Course,
                };

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

        Ok(Self {
            max_workers: args.workers.unwrap_or(5),
            default_audio_format: args.audio_format.clone(),
            server_prefixes: external_config.network.server_prefixes
                .unwrap_or_else(|| vec!["s-file-1".into(), "s-file-2".into(), "s-file-3".into()]),
            user_agent: constants::USER_AGENT.into(),
            connect_timeout: Duration::from_secs(external_config.network.connect_timeout_secs.unwrap_or(5)),
            timeout: Duration::from_secs(external_config.network.timeout_secs.unwrap_or(15)),
            max_retries: external_config.network.max_retries.unwrap_or(3),
            api_endpoints,
            url_templates: external_config.url_templates,
        })
    }
}
