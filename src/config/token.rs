// src/config/token.rs

use crate::{
    config::ExternalConfig, // 只需要从父模块导入结构体定义
    constants,
    error::{AppError, AppResult},
};
use anyhow::{Context, anyhow};
use log::{debug, info};
use std::{fs, path::PathBuf};

pub(super) fn get_config_path() -> AppResult<PathBuf> {
    let path = dirs::home_dir()
        .ok_or_else(|| AppError::Other(anyhow!("无法获取用户主目录")))?
        .join(constants::CONFIG_DIR_NAME)
        .join(constants::CONFIG_FILE_NAME);
    Ok(path) // 将最终的 PathBuf 包装在 Ok() 中返回
}

pub(crate) fn load_or_create_external_config() -> AppResult<ExternalConfig> {
    let config_path = get_config_path()?;
    if config_path.is_file() {
        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("读取配置文件 '{}' 失败", config_path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("解析配置文件 '{}' 失败", config_path.display()))
            .map_err(AppError::from)
    } else {
        info!("配置文件 {:?} 不存在，将创建默认配置。", config_path);
        let config = ExternalConfig::default_app_config();

        if let Some(dir) = config_path.parent() {
            fs::create_dir_all(dir)?;
        }

        let json_content = serde_json::to_string_pretty(&config)?;
        fs::write(&config_path, json_content)?;

        Ok(config)
    }
}

pub fn save_token(token: &str) -> AppResult<()> {
    if token.is_empty() {
        return Ok(());
    }

    let config_path = get_config_path()?;
    let mut config = load_or_create_external_config()?; // 现在调用的是本模块的函数

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

pub fn load_token_from_config() -> Option<String> {
    load_or_create_external_config()
        .ok()
        .and_then(|config| config.accesstoken)
}

pub fn resolve_token(cli_token: Option<&str>) -> (Option<String>, String) {
    if let Some(token) = cli_token && !token.is_empty() {
        debug!("使用来自命令行参数的 Token");
        return (Some(token.to_string()), "命令行参数".to_string());
    }
    if let Ok(token) = std::env::var("ACCESS_TOKEN") && !token.is_empty() {
        debug!("使用来自环境变量 ACCESS_TOKEN 的 Token");
        return (Some(token), "环境变量 (ACCESS_TOKEN)".to_string());
    }
    if let Some(token) = load_token_from_config() && !token.is_empty() {
        debug!("使用来自本地配置文件的 Token");
        return (Some(token), "本地Token文件".to_string());
    }
    debug!("未在任何位置找到可用的 Token");
    (None, "未找到".to_string())
}
