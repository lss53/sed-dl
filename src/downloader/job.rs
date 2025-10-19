// src/downloader/job.rs

use super::{negotiator::ItemNegotiator, task_runner};
use crate::{
    cli::ResourceType,
    constants,
    error::*,
    models::{FileInfo, MetadataExtractionResult, ResourceCategory},
    ui, utils, DownloadJobContext,
};
use anyhow::anyhow;
use log::{debug, error, info, warn};
use std::{fs, path::Path};


#[derive(Clone)]
pub struct ResourceDownloader {
    pub(super) context: DownloadJobContext,
}

impl ResourceDownloader {
    pub fn new(context: DownloadJobContext) -> Self {
        Self { context }
    }
    

    /// 核心公共流程：处理已获取的文件列表并下载
    pub async fn process_and_download_items(&self, items: Vec<FileInfo>) -> AppResult<bool> {
        if items.is_empty() {
            ui::plain("");
            ui::info("未能提取到任何可下载的文件信息 (或所有文件均被过滤)。");
            return Ok(true);
        }

        let base_output_dir = self.context.args.output.clone();
        fs::create_dir_all(&base_output_dir)?;
        let absolute_path = dunce::canonicalize(&base_output_dir)?;
        info!("文件将保存到目录: \"{}\"", absolute_path.display());
        ui::plain("");
        ui::info(&format!("文件将保存到目录: \"{}\"", absolute_path.display()));

        let selected_indices = if self.context.non_interactive {
            self.parse_selection_from_args(&items)?
        } else {
            self.get_user_selection_interactive(&items)?
        };

        if selected_indices.is_empty() {
            ui::plain("");
            ui::info("未选择任何文件，任务结束。");
            return Ok(true);
        }
        let final_tasks: Vec<FileInfo> =
            selected_indices.into_iter().map(|i| items[i].clone()).collect();

        if final_tasks.is_empty() {
            ui::plain("");
            ui::info("根据您的清晰度/格式选择，没有文件可供下载。");
            return Ok(true);
        }

        let final_tasks_with_paths = self.prepare_final_tasks(final_tasks, &base_output_dir)?;
        self.execute_download_loop(final_tasks_with_paths).await
    }

    /// 封装了从单个输入（URL/ID）抓取元数据的完整逻辑
    pub async fn fetch_metadata(&self, task_input: &str) -> AppResult<MetadataExtractionResult> {
        let context = &self.context;
        // +++ 使用常量 +++
        use constants::api::types::*;

        let (extractor, resource_id) = if utils::is_resource_id(task_input) {
            let resource_type_enum = context.args.r#type.as_ref().ok_or_else(|| {
                AppError::UserInputError("使用ID时必须提供 --type".to_string())
            })?;
            let type_key = match resource_type_enum {
                // +++ 使用常量 +++
                ResourceType::TchMaterial => TCH_MATERIAL,
                ResourceType::QualityCourse => QUALITY_COURSE,
                ResourceType::SyncClassroom => SYNC_CLASSROOM,
            };
            let api_conf = context.config.api_endpoints.get(type_key).ok_or_else(|| {
                AppError::Other(anyhow!("未找到类型 '{}' 的API配置", type_key))
            })?;
            (self.create_extractor(api_conf)?, task_input.to_string())
        } else if url::Url::parse(task_input).is_ok() {
            self.get_extractor_info(task_input)?
        } else {
            return Err(AppError::UserInputError(format!(
                "无效条目: {}",
                task_input
            )));
        };

        let all_file_items = extractor.extract_file_info(&resource_id, context).await?;
        let original_count = all_file_items.len();

        let items_after_ext_filter = if let Some(exts) = &context.args.filter_ext {
            let lower_exts_to_keep: Vec<String> = exts.iter().map(|s| s.to_lowercase()).collect();
            all_file_items
                .into_iter()
                .filter(|item| {
                    item.filepath
                        .extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(|ext| lower_exts_to_keep.contains(&ext.to_lowercase()))
                })
                .collect()
        } else {
            all_file_items
        };
        let ext_filtered_count = items_after_ext_filter.len();
        
        let negotiator = ItemNegotiator::new(context);
        let (final_list, version_filtered_count) = self
            .prepare_selection_list(items_after_ext_filter, &negotiator)
            .await?;
        
        // 返回包含所有计数信息的元组
        Ok(MetadataExtractionResult {
            files: final_list,
            original_count,
            after_ext_filter_count: ext_filtered_count,
            after_version_filter_count: version_filtered_count,
        })
    }
    
    fn parse_selection_from_args(&self, items: &[FileInfo]) -> AppResult<Vec<usize>> {
        let user_input = self.context.args.select.clone();
        let indices = utils::parse_selection_indices(&user_input, items.len());
        debug!(
            "非交互模式：根据输入 '{}' 解析出索引: {:?}",
            user_input, indices
        );
        Ok(indices)
    }

    fn get_user_selection_interactive(&self, items: &[FileInfo]) -> AppResult<Vec<usize>> {
        let options: Vec<String> = items
            .iter()
            .map(|item| {
                let date_str = item
                    .date
                    .map_or("[ 日期未知 ]".to_string(), |d| format!("[{}]", d.format("%Y-%m-%d")));
                let filename = item.filepath.file_name().unwrap().to_string_lossy();
                let truncated_name =
                    utils::truncate_text(&filename, constants::FILENAME_TRUNCATE_LENGTH);
                format!("{} {}", date_str, truncated_name)
            })
            .collect();

        let user_input = ui::selection_menu(
            &options,
            "文件下载列表",
            "支持格式: 1, 3, 2-4, all",
            &self.context.args.select,
        )?;
        let indices = utils::parse_selection_indices(&user_input, options.len());
        debug!(
            "交互模式：根据用户输入 '{}' 解析出索引: {:?}",
            user_input, indices
        );
        Ok(indices)
    }

    pub(super) async fn prepare_selection_list<'a>(
        &self,
        items: Vec<FileInfo>,
        negotiator: &'a ItemNegotiator<'a>,
    ) -> AppResult<(Vec<FileInfo>, usize)> {
        let count_before_version_filter = items.len();
        
        let mut prepared_list = if self.context.non_interactive {
            negotiator.pre_filter_items(items)?
        } else {
            debug!("交互模式：开始预协商...");
            let (video_items, non_video_items): (Vec<FileInfo>, Vec<FileInfo>) =
                items.into_iter().partition(|f| f.category == ResourceCategory::Video);
            let (audio_items, mut other_items): (Vec<FileInfo>, Vec<FileInfo>) = non_video_items
                .into_iter()
                .partition(|f| f.category == ResourceCategory::Audio);

            let negotiated_videos = negotiator.negotiate_video_interactive(video_items).await?;
            let negotiated_audios = negotiator.negotiate_audio_interactive(audio_items)?;

            other_items.extend(negotiated_videos);
            other_items.extend(negotiated_audios);
            other_items
        };
        
        prepared_list.sort_by_key(|f| f.filepath.clone());
        
        // 在非交互模式下，prepared_list 的长度就是版本过滤后的数量。
        // 在交互模式下，我们返回过滤前的数量，因为真正的“选择”尚未发生。
        let final_version_filtered_count = if self.context.non_interactive {
            prepared_list.len()
        } else {
            count_before_version_filter
        };

        debug!("预协商/过滤完成，最终待选列表数量: {}", prepared_list.len());
        Ok((prepared_list, final_version_filtered_count))
    }

    pub(super) fn prepare_final_tasks(
        &self,
        tasks: Vec<FileInfo>,
        base_dir: &Path,
    ) -> AppResult<Vec<FileInfo>> {
        tasks
            .into_iter()
            .map(|mut item| {
                item.filepath = utils::secure_join_path(base_dir, &item.filepath)?;
                Ok(item)
            })
            .collect()
    }

    pub(super) async fn execute_download_loop(&self, final_tasks: Vec<FileInfo>) -> AppResult<bool> {
        let mut tasks_to_attempt = final_tasks.clone();
        self.context.manager.start_batch(tasks_to_attempt.len());
        loop {
            match task_runner::execute_tasks(&self.context, &tasks_to_attempt).await {
                Ok(_) => break,
                Err(e @ AppError::TokenInvalid) => {
                    let token_is_empty = self.context.token.lock().await.is_empty();
                    let specific_error = if token_is_empty { AppError::TokenMissing } else { e };

                    warn!("下载任务因认证问题中断: {}", specific_error);
                    if self.context.non_interactive {
                        error!("非交互模式下因认证问题无法继续。");
                        return Err(specific_error);
                    }

                    let retry_result = self.handle_token_failure_and_retry(&final_tasks).await?;
                    if retry_result.should_abort {
                        info!("用户选择中止任务。");
                        return Ok(false);
                    }
                    if let Some(remaining) = retry_result.remaining_tasks {
                        tasks_to_attempt = remaining;
                    } else { break; }
                }
                Err(e) => {
                    error!("执行下载任务时发生不可恢复的错误: {}", e);
                    return Err(e);
                }
            }
        }
        self.context.manager.print_report();
        Ok(self.context.manager.did_all_succeed())
    }
}