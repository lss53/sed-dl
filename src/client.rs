// src/client.rs

use crate::{config::AppConfig, error::*};
use anyhow::anyhow;
use log::{debug, error, trace, warn};
use reqwest::{IntoUrl, Response, StatusCode};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{
    DefaultRetryableStrategy, // 直接从 crate 根导入
    RetryTransientMiddleware,
    policies::ExponentialBackoff,
};
use serde::de::DeserializeOwned;
use std::sync::Arc;

#[derive(Clone)]
pub struct RobustClient {
    pub client: ClientWithMiddleware,
    config: Arc<AppConfig>,
}

impl RobustClient {
    // ## 修改点 1: 更新中间件初始化 ##
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
        // 使用新的 `new_with_policy_and_strategy` 方法，并提供默认的重试策略
        .with(RetryTransientMiddleware::new_with_policy_and_strategy(
            retry_policy,
            DefaultRetryableStrategy,
        ))
        .build();
        debug!(
            "RobustClient created with max_retries={}",
            config.max_retries
        );
        Ok(Self { client, config })
    }

    // ## 修改点 2: 重构 get 方法以适应 reqwest 0.12 的错误处理 ##
    pub async fn get<T: IntoUrl>(&self, url: T) -> AppResult<Response> {
        let url_str = url.as_str().to_owned();
        debug!("HTTP GET: {}", url_str);

        // `send()` 的错误现在主要是网络层面的
        let res = self.client.get(url_str).send().await?;

        match res.status() {
            s if s.is_success() => Ok(res),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                warn!(
                    "向 {} 的 HTTP 请求返回状态码: {}。这表明 Token 无效。",
                    res.url(),
                    res.status()
                );
                Err(AppError::TokenInvalid)
            }
            s => {
                warn!(
                    "向 {} 的 HTTP 请求返回状态码: {}",
                    res.url(),
                    s
                );
                // error_for_status() 会根据状态码生成一个合适的 reqwest::Error
                // .into() 会自动将其转换为 AppError::Network
                Err(res.error_for_status().unwrap_err().into())
            }
        }
    }

    pub async fn fetch_json<T: DeserializeOwned>(
        &self,
        url_template: &str,
        params: &[(&str, &str)],
    ) -> AppResult<T> {
        let mut last_error: Option<AppError> = None;
        for prefix in &self.config.server_prefixes {
            let mut url = url_template.replace("{prefix}", prefix);
            for (key, val) in params {
                url = url.replace(&format!("{{{}}}", key), val);
            }
            match self.get(&url).await {
                Ok(res) => {
                    let text = res.text().await?;
                    trace!("Raw JSON response from {}: {}", url, text);

                    match serde_json::from_str::<T>(&text) {
                        Ok(data) => return Ok(data), // 解析成功，直接返回
                        Err(e) => {
                            warn!(
                                "服务器 '{}' 响应成功但JSON解析失败: {:?}. 尝试下一个服务器...",
                                prefix, e
                            );
                            last_error = Some(AppError::from(e));
                            // 继续循环，尝试下一个服务器
                        }
                    }
                }
                Err(e) => {
                    // 如果是 Token 错误，这是一个不可恢复的致命错误，立即中止并返回
                    if matches!(e, AppError::TokenInvalid) {
                        warn!("请求因 Token 无效而失败，停止尝试其他服务器。");
                        return Err(e);
                    }
                    warn!("服务器 '{}' 请求失败: {:?}", prefix, e);
                    last_error = Some(e);
                }
            }
        }
        error!("所有服务器均请求失败 for template: {}", url_template);
        match last_error {
            Some(err) => Err(err),
            None => Err(AppError::Other(anyhow!(
                "所有服务器均请求失败，且没有配置服务器前缀"
            ))),
        }
    }
}
