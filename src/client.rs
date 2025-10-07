// src/client.rs

use crate::{config::AppConfig, error::*};
use anyhow::anyhow;
use colored::Colorize;
use reqwest::{IntoUrl, Response, StatusCode};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct RobustClient {
    pub client: ClientWithMiddleware,
    config: Arc<AppConfig>,
}

impl RobustClient {
    pub fn new(config: Arc<AppConfig>) -> Self {
        let retry_policy =
            ExponentialBackoff::builder().build_with_max_retries(config.max_retries);
        let client = ClientBuilder::new(
            reqwest::Client::builder()
                .user_agent(config.user_agent.clone())
                .connect_timeout(config.connect_timeout)
                .timeout(config.timeout)
                .pool_max_idle_per_host(config.max_workers * 3)
                .build()
                .unwrap(),
        )
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .build();

        Self { client, config }
    }

    pub async fn get<T: IntoUrl>(&self, url: T) -> AppResult<Response> {
        let res = self.client.get(url).send().await?;
        if res.status() == StatusCode::UNAUTHORIZED || res.status() == StatusCode::FORBIDDEN {
            return Err(AppError::TokenInvalid);
        }
        Ok(res.error_for_status()?)
    }

    pub async fn fetch_json(
        &self,
        url_template: &str,
        params: &[(&str, &str)],
    ) -> AppResult<Value> {
        let mut last_error = None;
        for prefix in &self.config.server_prefixes {
            let mut url = url_template.replace("{prefix}", prefix);
            for (key, val) in params {
                url = url.replace(&format!("{{{}}}", key), val);
            }
            match self.get(&url).await {
                Ok(res) => return Ok(res.json().await?),
                Err(e) => {
                    // This is a transient error, so we just log it to stderr
                    // rather than returning it, to allow other servers to be tried.
                    eprintln!(
                        "{} 服务器 '{}' 请求失败: {:?}",
                        "[!]".yellow(),
                        prefix,
                        e
                    );
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or(AppError::Other(anyhow!("所有服务器均请求失败"))))
    }
}