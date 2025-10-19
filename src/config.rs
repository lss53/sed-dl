// src/config.rs

pub mod token;

use self::token::load_or_create_external_config;
use crate::{cli::Cli, constants, error::AppResult};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};

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
    #[serde(default)]
    pub network: NetworkConfig,
    pub url_templates: HashMap<String, String>,
    // 直接使用 ApiEndpointConfig
    pub api_endpoints: HashMap<String, ApiEndpointConfig>,
    #[serde(default)]
    pub directory_structure: DirectoryStructureConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryStructureConfig {
    pub textbook_path_order: Vec<String>,
    pub textbook_path_defaults: HashMap<String, String>,
}

// 为 DirectoryStructureConfig 实现 Default
impl Default for DirectoryStructureConfig {
    fn default() -> Self {
        // 使用常量
        use constants::api::dimensions::*;
        Self {
            textbook_path_order: vec![
                STAGE.into(), GRADE.into(), SUBJECT.into(), VERSION.into(), VOLUME.into()
            ],
            textbook_path_defaults: HashMap::from([
                (STAGE.into(), "未知学段".into()),
                (GRADE.into(), "未知年级".into()),
                (SUBJECT.into(), "未知学科".into()),
                (VERSION.into(), "未知版本".into()),
                (VOLUME.into(), "未知册次".into()),
            ]),
        }
    }
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

        // 使用常量
        use constants::api::types::*;
        let api_endpoints = HashMap::from([
            (
                TCH_MATERIAL.into(),
                // 直接构造 ApiEndpointConfig
                ApiEndpointConfig {
                    id_param: "contentId".into(),
                    extractor: ResourceExtractorType::Textbook,
                    main_template_key: "TEXTBOOK_DETAILS".into(),
                },
            ),
            (
                QUALITY_COURSE.into(),
                ApiEndpointConfig {
                    id_param: "courseId".into(),
                    extractor: ResourceExtractorType::Course,
                    main_template_key: "COURSE_QUALITY".into(),
                },
            ),
            (
                SYNC_CLASSROOM.into(),
                ApiEndpointConfig {
                    id_param: "activityId".into(),
                    extractor: ResourceExtractorType::SyncClassroom,
                    main_template_key: "COURSE_SYNC".into(),
                },
            ),
        ]);

        // 为 NetworkConfig 提供一组稳健的默认值
        let network_config = NetworkConfig {
            server_prefixes: Some(vec!["s-file-1".into(), "s-file-2".into(), "s-file-3".into()]),
            connect_timeout_secs: Some(10),
            timeout_secs: Some(60), // 推荐把 60 秒设为超时默认值
            max_retries: Some(3),
        };

        Self {
            accesstoken: None,
            network: network_config,
            url_templates,
            api_endpoints,
            directory_structure: DirectoryStructureConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiEndpointConfig {
    pub id_param: String,
    pub extractor: ResourceExtractorType,
    #[serde(default = "default_main_template_key")] // serde 需要这个函数
    pub main_template_key: String,
}

// 辅助函数现在为 ApiEndpointConfig 服务
fn default_main_template_key() -> String {
    "main".to_string()
}

// 为 ResourceExtractorType 添加 serde 属性，使其可以直接从 JSON 文件中反序列化
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum ResourceExtractorType {
    Textbook,
    Course,
    SyncClassroom,
}

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
    pub dir_config: DirectoryStructureConfig,
}

impl AppConfig {
    pub fn new(args: &Cli) -> AppResult<Self> {
        let external_config = load_or_create_external_config()?;
        
        // 现在这里的逻辑是正确的，因为不再需要转换
        let api_endpoints = external_config.api_endpoints;

        Ok(Self {
            max_workers: args.workers.unwrap_or(5),
            default_audio_format: args.audio_format.clone(),
            server_prefixes: external_config
                .network
                .server_prefixes
                .unwrap_or_default(),
            user_agent: constants::USER_AGENT.into(),
            connect_timeout: Duration::from_secs(
                external_config.network.connect_timeout_secs.unwrap_or(10),
            ),
            timeout: Duration::from_secs(external_config.network.timeout_secs.unwrap_or(60)),
            max_retries: external_config.network.max_retries.unwrap_or(3),
            api_endpoints, // 直接使用
            url_templates: external_config.url_templates,
            dir_config: external_config.directory_structure,
        })
    }
}

#[cfg(feature = "testing")]
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
            dir_config: DirectoryStructureConfig::default(),
        }
    }
}
