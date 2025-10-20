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
use tokio::task::block_in_place;

#[derive(Clone)]
pub struct RobustClient {
    pub client: ClientWithMiddleware,
    config: Arc<AppConfig>,
}

impl RobustClient {
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
        .with(RetryTransientMiddleware::new_with_policy_and_strategy(
            retry_policy,
            RateLimitingRetryStrategy,
        ))
        .build();
        debug!("RobustClient created with max_retries={}", config.max_retries);
        Ok(Self { client, config })
    }

    pub async fn get<T: IntoUrl>(&self, url: T) -> AppResult<Response> {
        let url_ref = url.as_str();
        debug!("HTTP GET: {}", url_ref);

        let res = self.client.get(url_ref).send().await?;

        match res.status() {
            s if s.is_success() => Ok(res),
            StatusCode::UNAUTHORIZED => { // 401
                warn!("请求 {} 返回 401: Token 无效或缺失。", res.url());
                Err(AppError::TokenInvalid)
            }
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => { // 403 and 404
                warn!("请求 {} 返回 {}: 资源不存在。", res.url(), res.status());
                Err(AppError::Network(res.error_for_status().err().unwrap()))
            }
            s => { // 其他所有错误码
                warn!("向 {} 的 HTTP 请求返回状态码: {}", res.url(), s);
                Err(AppError::Network(res.error_for_status().err().unwrap()))
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
                            warn!("服务器 '{}' 响应成功但JSON解析失败: {:?}. 尝试...", prefix, e);
                            last_error = Some(AppError::ApiParseFailed { url: url.clone(), source: e });
                        }
                    }
                }
                Err(e) => {
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
        Err(last_error.unwrap_or_else(|| AppError::Other(anyhow!("所有服务器均请求失败"))))
    }
}

#[derive(Clone)]
struct RateLimitingRetryStrategy;

impl RetryableStrategy for RateLimitingRetryStrategy {
    fn handle(&self, res: &Result<reqwest::Response, reqwest_middleware::Error>) -> Option<Retryable> {
        if let Ok(success) = res && success.status() == StatusCode::TOO_MANY_REQUESTS {
            debug!("服务器返回 429 Too Many Requests，将进行重试");
            let retry_after = success.headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(std::time::Duration::from_secs);
            let delay = retry_after.unwrap_or_else(|| std::time::Duration::from_secs(1));
            warn!("服务器速率限制，等待 {:?} 后重试...", delay);
            // 使用 block_in_place 包裹同步 sleep
            block_in_place(|| {
                std::thread::sleep(delay);
            });
            
            return Some(Retryable::Transient);
        }
        DefaultRetryableStrategy.handle(res)
    }
}