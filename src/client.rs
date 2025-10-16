// src/client.rs

use crate::{config::AppConfig, error::*};
use anyhow::anyhow;
use log::{debug, error, trace, warn};
use reqwest::{header, IntoUrl, Response, StatusCode};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{
    policies::ExponentialBackoff, DefaultRetryableStrategy, Retryable, RetryableStrategy,
    RetryTransientMiddleware,
};
use serde::de::DeserializeOwned;
use std::sync::Arc;

#[derive(Clone)]
pub struct RobustClient {
    pub client: ClientWithMiddleware,
    config: Arc<AppConfig>,
}

impl RobustClient {
    // 更新中间件初始化
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
            RateLimitingRetryStrategy, 
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
        let url_ref = url.as_str();
        debug!("HTTP GET: {}", url_ref);

        // `send()` 的错误现在主要是网络层面的
        let res = self.client.get(url_ref).send().await?;

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
                warn!("向 {} 的 HTTP 请求返回状态码: {}", res.url(), s);
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
                    trace!("原始JSON响应来自 {}: {}", url, text);

                    match serde_json::from_str::<T>(&text) {
                        Ok(data) => return Ok(data),
                        Err(e) => {
                            warn!(
                                "服务器 '{}' 响应成功但JSON解析失败: {:?}. 尝试下一个服务器...",
                                prefix, e
                            );
                            last_error = Some(AppError::ApiParseFailed {
                                url: url.clone(),
                                source: e,
                            });
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

/// 一个自定义的重试策略，增加了对 HTTP 429 (Too Many Requests) 错误的处理。
#[derive(Clone)]
struct RateLimitingRetryStrategy;

impl RetryableStrategy for RateLimitingRetryStrategy {
    fn handle(
        &self,
        res: &Result<reqwest::Response, reqwest_middleware::Error>,
    ) -> Option<Retryable> {
        // 只检查我们关心的特殊情况：HTTP 429 错误。
        if let Ok(success) = res {
            if success.status() == StatusCode::TOO_MANY_REQUESTS {
                debug!("服务器返回 429 Too Many Requests，将根据 Retry-After 头进行重试");
                // 尝试从服务器的 Retry-After 响应头中获取建议的等待时间
                let retry_after = success
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(std::time::Duration::from_secs);

                let delay = retry_after.unwrap_or_else(|| {
                    // 如果服务器没有提供 Retry-After，我们自己设定一个默认的短暂停顿
                    // 以免立即重试再次触发速率限制。
                    std::time::Duration::from_secs(1)
                });
 
                warn!("服务器速率限制，将等待 {:?} 后重试...", delay);
                std::thread::sleep(delay);
 
                // 等待结束后，我们告诉中间件这是一个“临时错误”，
                // 它会立即（或经过很短的指数退避延迟后）进行下一次尝试。
                return Some(Retryable::Transient);
            }
        }

        // 对于所有其他情况（包括网络错误和其他HTTP状态码），
        // 我们直接将任务委托给 reqwest-retry 库的默认策略。
        DefaultRetryableStrategy.handle(res)
    }
}