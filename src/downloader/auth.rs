// src/downloader/auth.rs

use super::job::ResourceDownloader;
use crate::{config, constants, error::*, models::*, symbols, ui};
// --- 修正: 导入缺失的 trait ---
use colored::Colorize;
use log::{debug, error, info, warn};
use reqwest::StatusCode;
use url::Url;

/// 这部分 `impl` 专注于处理认证失败和用户交互。
impl ResourceDownloader {
    /// 在 Token 失效后，提示用户输入新 Token 并准备重试。
    pub(super) async fn handle_token_failure_and_retry(
        &self,
        initial_tasks: &[FileInfo],
    ) -> AppResult<TokenRetryResult> {
        ui::box_message(
            "认证失败",
            &[
                "当前 Access Token 已失效或无权限访问。",
                "输入 '2' 可以查看获取 Token 的详细指南。",
            ],
            |s| s.red(),
        );
        loop {
            let prompt_msg = format!(
                "选择操作: [1] 输入新 Token  [2] 查看帮助 (按 {} 中止)",
                *symbols::CTRL_C
            );
            match ui::prompt(&prompt_msg, Some("1")) {
                Ok(choice) if choice == "1" => {
                    match ui::prompt_hidden("请输入新 Token (输入不可见，完成后按回车)") {
                        Ok(new_token) if !new_token.is_empty() => {
                            info!("用户输入了新的 Token，正在验证...");
                            if !self.validate_token_with_probe(&new_token, initial_tasks).await {
                                eprintln!("\n{} 新输入的Token似乎仍然无效，请重试。", *symbols::ERROR);
                                continue;
                            }
                            *self.context.token.lock().await = new_token.clone();
                            if ui::confirm("是否保存此新 Token 以便后续使用?", false) {
                                if let Err(e) = config::token::save_token(&new_token) {
                                    error!("尝试保存新Token时失败: {}", e);
                                    eprintln!("{} 保存新Token失败: {}", *symbols::WARN, e);
                                }
                            }
                            break;
                        }
                        _ => println!("{}", "Token 不能为空。".yellow()),
                    }
                }
                Ok(choice) if choice == "2" => {
                    ui::box_message(
                        "获取 Access Token 指南",
                        constants::HELP_TOKEN_GUIDE
                            .lines()
                            .collect::<Vec<_>>()
                            .as_slice(),
                        |s| s.cyan(),
                    );
                }
                Err(_) => {
                    warn!("用户在 Token 提示处中断。");
                    return Ok(TokenRetryResult {
                        remaining_tasks: None,
                        should_abort: true,
                    });
                }
                _ => continue,
            }
        }
        println!("\n{} Token 已更新。正在检查剩余任务...", *symbols::INFO);
        let mut remaining_tasks = vec![];
        let mut remaining_filenames = vec![];
        for task in initial_tasks {
            // Re-check file status with the new token context in mind.
            if let Ok((action, _, _)) =
                super::task_processor::TaskProcessor::prepare_download_action(
                    task,
                    &self.context.args,
                )
            {
                if action != DownloadAction::Skip {
                    remaining_tasks.push(task.clone());
                    remaining_filenames.push(task.filepath.to_string_lossy().into_owned());
                }
            }
        }
        if remaining_tasks.is_empty() {
            info!("所有任务均已完成，无需重试。");
            println!("{} 所有任务均已完成，无需重试。", *symbols::OK);
            return Ok(TokenRetryResult {
                remaining_tasks: None,
                should_abort: false,
            });
        }
        self.context
            .manager
            .reset_token_failures(&remaining_filenames);
        info!("准备重试剩余的 {} 个任务。", remaining_tasks.len());
        println!(
            "{} 准备重试剩余的 {} 个任务...",
            *symbols::INFO,
            remaining_tasks.len()
        );
        Ok(TokenRetryResult {
            remaining_tasks: Some(remaining_tasks),
            should_abort: false,
        })
    }

    /// 使用 HEAD 请求探测一个 URL，以验证新 Token 的有效性。
    async fn validate_token_with_probe(&self, token: &str, tasks: &[FileInfo]) -> bool {
        let Some(probe_url_str) = tasks.iter().find_map(|t| if t.url.starts_with("http") { Some(&t.url) } else { None }) else {
            warn!("在剩余任务中未找到可用于探测Token的HTTP URL。");
            return true;
        };
        let Ok(mut url) = Url::parse(probe_url_str) else { return false; };
        url.query_pairs_mut()
            .append_pair("accessToken", token);
        let res = self.context.http_client.client.head(url).send().await;
        match res {
            Ok(response) => {
                let status = response.status();
                debug!("Token 探测响应状态码: {}", status);
                !matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            }
            Err(e) => {
                warn!("Token 探测请求失败: {}", e);
                false
            }
        }
    }
}