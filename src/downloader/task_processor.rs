// src/downloader/task_processor.rs

use super::m3u8::M3u8Downloader;
use crate::{cli::Cli, error::*, models::*, utils, DownloadJobContext};
use futures::StreamExt;
use indicatif::{HumanBytes, ProgressBar};
use log::{debug, error, info, warn};
use reqwest::{header, StatusCode};
use std::{
    fs::{self, File, OpenOptions},
    io::Write as IoWrite,
};
use url::Url;

#[derive(Debug, PartialEq, Eq)]
enum ValidationStatus {
    Valid,
    Invalid(String),
    CanResume(u64),
    NoInfoToValidate,
}


/// `TaskProcessor` 封装了处理单个下载任务的所有逻辑。
pub struct TaskProcessor {
    context: DownloadJobContext,
}

impl TaskProcessor {
    pub fn new(context: DownloadJobContext) -> Self {
        Self { context }
    }
    

    /// 处理单个文件任务，包括准备、下载和最终校验。
    pub async fn process(
        &self,
        item: FileInfo,
        pbar: ProgressBar,
        use_byte_progress: bool,
    ) -> AppResult<DownloadResult> {
        let attempt_result: AppResult<DownloadResult> = async {
            if let Some(parent) = item.filepath.parent() {
                fs::create_dir_all(parent)?;
            }
            let (action, resume_bytes, reason) = Self::prepare_download_action(&item, &self.context.args)?;
            if action == DownloadAction::Skip {
                return Ok(DownloadResult {
                    filename: item.filepath.file_name().unwrap().to_string_lossy().to_string(),
                    status: DownloadStatus::Skipped,
                    message: Some(reason),
                });
            }

            let download_status = match item.category {
                ResourceCategory::Video => {
                    M3u8Downloader::new(self.context.clone())
                        .download(&item, pbar, use_byte_progress)
                        .await?
                }
                _ => {
                    self.download_standard_file(&item, resume_bytes, pbar, use_byte_progress)
                        .await?
                }
            };

            let final_status = if matches!(download_status, DownloadStatus::Success | DownloadStatus::Resumed) {
                Self::finalize_and_validate(&item)?
            } else {
                download_status
            };
            Ok(DownloadResult {
                filename: item.filepath.file_name().unwrap().to_string_lossy().to_string(),
                status: final_status,
                message: None,
            })
        }.await;

        match attempt_result {
            Ok(result) => Ok(result),
            Err(e @ AppError::TokenInvalid) => Err(e),
            Err(e) => {
                error!("处理任务 '{:?}' 时发生错误: {}", item.filepath, e);
                Ok(DownloadResult {
                    filename: item.filepath.file_name().unwrap().to_string_lossy().to_string(),
                    status: DownloadStatus::from(&e),
                    message: Some(e.to_string()),
                })
            }
        }
    }

    /// 检查本地文件状态，决定是跳过、续传还是重新下载。
    /// 改为 pub(super) 以便 auth 模块可以调用它。
    pub(super) fn prepare_download_action(
        item: &FileInfo,
        args: &Cli,
    ) -> AppResult<(DownloadAction, u64, String)> {
        if !item.filepath.exists() {
            return Ok((DownloadAction::DownloadNew, 0, "文件不存在".to_string()));
        }
        if args.force_redownload {
            info!("用户强制重新下载文件: {:?}", item.filepath);
            return Ok((DownloadAction::DownloadNew, 0, "强制重新下载".to_string()));
        }
        match Self::check_local_file_status(item)? {
            ValidationStatus::Valid => Ok((
                DownloadAction::Skip,
                0,
                "文件已存在且校验通过".to_string(),
            )),
            ValidationStatus::CanResume(from) => {
                Ok((DownloadAction::Resume, from, "文件不完整，尝试续传".to_string()))
            }
            ValidationStatus::Invalid(reason) => {
                Ok((DownloadAction::DownloadNew, 0, format!("文件无效: {}", reason)))
            }
            ValidationStatus::NoInfoToValidate => {
                Ok((DownloadAction::Skip, 0, "文件已存在 (无校验信息)".to_string()))
            }
        }
    }

    /// 下载完成后对文件进行最终的校验。
    fn finalize_and_validate(item: &FileInfo) -> AppResult<DownloadStatus> {
        debug!("对文件 '{:?}' 进行最终校验", item.filepath);
        match Self::check_local_file_status(item)? {
            ValidationStatus::Valid | ValidationStatus::NoInfoToValidate => Ok(DownloadStatus::Success),
            ValidationStatus::CanResume(_) => {
                error!("文件 '{:?}' 下载后仍不完整，校验失败。", item.filepath);
                Ok(DownloadStatus::SizeFailed)
            }
            ValidationStatus::Invalid(reason) => {
                error!("文件 '{:?}' 最终校验失败: {}", item.filepath, reason);
                Ok(if reason.contains("MD5") {
                    DownloadStatus::Md5Failed
                } else {
                    DownloadStatus::SizeFailed
                })
            }
        }
    }

    /// 检查本地文件的有效性（大小、MD5等）。
    /// 优化：只对 M3U8 视频应用大小容差。
    fn check_local_file_status(item: &FileInfo) -> AppResult<ValidationStatus> {
        if !item.filepath.exists() {
            return Ok(ValidationStatus::Invalid("文件不存在".to_string()));
        }
        let metadata = item.filepath.metadata()?;
        let actual_size = metadata.len();
        
        if item.category == ResourceCategory::Video {
            debug!(
                "M3U8 校验: 文件='{:?}', 期望大小(来自JSON): {:?}, 实际大小(合并后): {}",
                item.filepath.file_name(),
                item.ti_size,
                actual_size
            );
        }

        if actual_size == 0 {
            return Ok(ValidationStatus::Invalid("文件为空(0字节)".to_string()));
        }

        if let Some(expected_size) = item.ti_size {
            // --- 区分文件类型 ---
            let is_video = item.category == ResourceCategory::Video;
            let tolerance = if is_video {
                // M3U8 视频，应用 1% 的容差
                (expected_size as f64 * 0.01) as u64
            } else {
                // 普通文件，无容差
                0
            };
            
            let diff = (actual_size as i64 - expected_size as i64).abs() as u64;

            debug!(
                "大小校验详情 for '{:?}': 差异={}, 容差={}",
                item.filepath.file_name(),
                HumanBytes(diff),
                HumanBytes(tolerance)
            );

            if diff > tolerance {
                if actual_size < expected_size {
                    // 差异超出容差，且文件不完整，可续传
                    return Ok(ValidationStatus::CanResume(actual_size));
                } else {
                    // 差异超出容差，且文件过大，判定为错误
                    return Ok(ValidationStatus::Invalid(format!(
                        "大小错误 (预期: {}, 实际: {})",
                        HumanBytes(expected_size),
                        HumanBytes(actual_size)
                    )));
                }
            } else {
                // 差异在容差范围内，认为大小校验通过，跳过 MD5 校验
                debug!(
                    "文件 '{:?}' 大小校验通过，跳过 MD5 校验。",
                    item.filepath.file_name()
                );
                return Ok(ValidationStatus::Valid);
            }
        }

        // 只有在没有大小信息可供校验时，才会执行到 MD5 校验
        if let Some(expected_md5) = &item.ti_md5 {
            debug!(
                "文件 '{:?}' 没有大小信息，开始进行 MD5 校验...",
                item.filepath.file_name()
            );
            let actual_md5 = utils::calculate_file_md5(&item.filepath)?;
            if !actual_md5.eq_ignore_ascii_case(expected_md5) {
                return Ok(ValidationStatus::Invalid("MD5不匹配".to_string()));
            }
            return Ok(ValidationStatus::Valid);
        }
        
        Ok(ValidationStatus::NoInfoToValidate)
    }

    /// 下载标准文件（非 M3U8），支持断点续传。
    async fn download_standard_file(
        &self,
        item: &FileInfo,
        resume_from: u64,
        pbar: ProgressBar,
        use_byte_progress: bool,
    ) -> AppResult<DownloadStatus> {
        let mut current_resume_from = resume_from;
        loop {
            let mut url = Url::parse(&item.url)?;
            let token = self.context.token.lock().await;
            if !token.is_empty() {
                url.query_pairs_mut()
                    .append_pair("accessToken", &token);
            }
            let mut request_builder = self.context.http_client.client.get(url.clone());
            if current_resume_from > 0 {
                request_builder =
                    request_builder.header(header::RANGE, format!("bytes={}-", current_resume_from));
            }
            drop(token);

            let res = request_builder.send().await?;
            if res.status() == StatusCode::RANGE_NOT_SATISFIABLE {
                warn!(
                    "续传点 {} 无效，将从头开始下载: {}",
                    current_resume_from,
                    &item.filepath.display()
                );
                current_resume_from = 0;
                if item.filepath.exists() {
                    fs::remove_file(&item.filepath)?;
                }
                continue;
            }
            if matches!(res.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                return Err(AppError::TokenInvalid);
            }
            let res = res.error_for_status()?;

            let mut file = if current_resume_from > 0 {
                OpenOptions::new().append(true).open(&item.filepath)?
            } else {
                File::create(&item.filepath)?
            };

            let mut stream = res.bytes_stream();
            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result?;
                file.write_all(&chunk)?;
                if use_byte_progress {
                    pbar.inc(chunk.len() as u64);
                }
            }
            return Ok(if current_resume_from > 0 {
                DownloadStatus::Resumed
            } else {
                DownloadStatus::Success
            });
        }
    }
}