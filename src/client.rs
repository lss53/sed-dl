// src/client.rs

use crate::{config::AppConfig, error::*};
use anyhow::anyhow;
use log::{debug, error, trace, warn};
use reqwest::{IntoUrl, Response, StatusCode};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{
    policies::ExponentialBackoff, 
    RetryTransientMiddleware, 
    DefaultRetryableStrategy // 直接从 crate 根导入
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
        let retry_policy =
            ExponentialBackoff::builder().build_with_max_retries(config.max_retries);
            
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
        debug!("RobustClient created with max_retries={}", config.max_retries);
        Ok(Self { client, config })
    }

    // ## 修改点 2: 重构 get 方法以适应 reqwest 0.12 的错误处理 ##
    pub async fn get<T: IntoUrl>(&self, url: T) -> AppResult<Response> {
        let url_str = url.as_str().to_owned();
        debug!("HTTP GET: {}", url_str);

        // `send()` 的错误现在主要是网络层面的
        let res = self.client.get(url_str).send().await?;
        
        let status = res.status();

        // 手动检查 HTTP 状态码
        if status.is_success() {
            // 2xx 状态码，请求成功
            Ok(res)
        } else {
            // 4xx 或 5xx 状态码，请求失败
            warn!("HTTP request to {} resulted in status code: {}", res.url(), status);
            
            // 专门处理认证失败的情况
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                warn!("Status code indicates invalid token.");
                Err(AppError::TokenInvalid)
            } else {
                // 对于其他错误，我们使用 error_for_status() 来生成一个包含上下文的 reqwest::Error
                // .unwrap_err() 在这里是安全的，因为我们已经知道 status 不是成功状态
                let err = res.error_for_status().unwrap_err();
                Err(AppError::Network(err))
            }
        }
    }

    pub async fn fetch_json<T: DeserializeOwned>(
        &self,
        url_template: &str,
        params: &[(&str, &str)],
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