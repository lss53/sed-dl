// src/downloader/job.rs

use super::{negotiator::ItemNegotiator, task_runner};
use crate::{
    DownloadJobContext,
    cli::ResourceType,
    constants,
    error::*,
    models::{FileInfo, ResourceCategory},
    symbols, ui, utils,
};
use anyhow::anyhow;
use log::{debug, error, info, warn};
use std::{fs, path::Path};

pub struct ResourceDownloader {
    pub(super) context: DownloadJobContext,
}

impl ResourceDownloader {
    pub fn new(context: DownloadJobContext) -> Self {
        Self { context }
    }

    pub async fn run(&self, url: &str) -> AppResult<bool> {
        info!("开始处理 URL: {}", url);
        let (extractor, resource_id) = self.get_extractor_info(url)?;
        self.prepare_and_run(extractor, &resource_id).await
    }

    pub async fn run_with_id(&self, resource_id: &str) -> AppResult<bool> {
        info!("开始处理 ID: {}", resource_id);
        let resource_type_enum = self
            .context
            .args
            .r#type
            .as_ref()
            .ok_or_else(|| AppError::Other(anyhow!("使用 --id 时必须提供 --type 参数")))?;

        let type_key = match resource_type_enum {
            ResourceType::TchMaterial => "tchMaterial",
            ResourceType::QualityCourse => "qualityCourse",
            ResourceType::SyncClassroom => "syncClassroom/classActivity",
        };

        let api_conf = self
            .context
            .config
            .api_endpoints
            .get(type_key)
            .ok_or_else(|| AppError::Other(anyhow!("未找到资源类型 '{}' 的API配置", type_key)))?;
        let extractor = self.create_extractor(api_conf);
        self.prepare_and_run(extractor, resource_id).await
    }

    pub async fn prepare_and_run(
        &self,
        extractor: Box<dyn crate::extractor::ResourceExtractor>,
        resource_id: &str,
    ) -> AppResult<bool> {
        let base_output_dir = self.context.args.output.clone();
        fs::create_dir_all(&base_output_dir)?;
        let absolute_path = dunce::canonicalize(&base_output_dir)?;
        info!("文件将保存到目录: \"{}\"", absolute_path.display());
        println!(
            "\n{} 文件将保存到目录: \"{}\"",
            *symbols::INFO,
            absolute_path.display()
        );

        let all_file_items = extractor
            .extract_file_info(resource_id, &self.context)
            .await?;

        let items_after_ext_filter = if let Some(exts) = &self.context.args.filter_ext {
            let original_count = all_file_items.len();
            let lower_exts_to_keep: Vec<String> = exts.iter().map(|s| s.to_lowercase()).collect();
            let filtered: Vec<_> = all_file_items
                .into_iter()
                .filter(|item| {
                    item.filepath
                        .extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(|ext| lower_exts_to_keep.contains(&ext.to_lowercase()))
                })
                .collect();
            if original_count > filtered.len() {
                println!(
                    "{} 已应用扩展名过滤器，文件数量从 {} 个变为 {} 个。",
                    *symbols::INFO,
                    original_count,
                    filtered.len()
                );
            }
            filtered
        } else {
            all_file_items
        };

        if items_after_ext_filter.is_empty() {
            println!(
                "\n{} 未能提取到任何可下载的文件信息 (或所有文件均被过滤)。",
                *symbols::INFO
            );
            return Ok(true);
        }

        let negotiator = ItemNegotiator::new(&self.context);

        let items_for_selection = self
            .prepare_selection_list(items_after_ext_filter, &negotiator)
            .await?;

        let selected_indices = self.get_user_selection(&items_for_selection)?;
        if selected_indices.is_empty() {
            println!("\n{} 未选择任何文件，任务结束。", *symbols::INFO);
            return Ok(true);
        }
        let final_tasks: Vec<FileInfo> = selected_indices
            .into_iter()
            .map(|i| items_for_selection[i].clone())
            .collect();

        if final_tasks.is_empty() {
            println!(
                "\n{} 根据您的清晰度/格式选择，没有文件可供下载。",
                *symbols::INFO
            );
            return Ok(true);
        }

        let final_tasks_with_paths = self.prepare_final_tasks(final_tasks, &base_output_dir)?;
        self.execute_download_loop(final_tasks_with_paths).await
    }

    /// 辅助函数: 在交互模式下预协商版本（视频和音频），或在非交互模式下应用过滤器。
    async fn prepare_selection_list<'a>(
        &self,
        items: Vec<FileInfo>,
        negotiator: &'a ItemNegotiator<'a>,
    ) -> AppResult<Vec<FileInfo>> {
        let mut prepared_list = if self.context.non_interactive {
            negotiator.pre_filter_items(items)?
        } else {
            debug!("交互模式：开始预协商...");
            // 添加类型注解
            let (video_items, non_video_items): (Vec<FileInfo>, Vec<FileInfo>) = items
                .into_iter()
                .partition(|f| f.category == ResourceCategory::Video);
            let (audio_items, mut other_items): (Vec<FileInfo>, Vec<FileInfo>) = non_video_items
                .into_iter()
                .partition(|f| f.category == ResourceCategory::Audio);

            let negotiated_videos = negotiator.negotiate_video_interactive(video_items).await?;
            let negotiated_audios = negotiator.negotiate_audio_interactive(audio_items)?;

            other_items.extend(negotiated_videos);
            other_items.extend(negotiated_audios);
            other_items
        };

        // --- 对两种模式都应用统一的排序 ---
        prepared_list.sort_by_key(|f| f.filepath.clone());

        debug!("预协商/过滤完成，最终待选列表数量: {}", prepared_list.len());
        Ok(prepared_list)
    }

    fn prepare_final_tasks(
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

    async fn execute_download_loop(&self, final_tasks: Vec<FileInfo>) -> AppResult<bool> {
        let mut tasks_to_attempt = final_tasks.clone();
        self.context.manager.start_batch(tasks_to_attempt.len());
        loop {
            match task_runner::execute_tasks(&self.context, &tasks_to_attempt).await {
                Ok(_) => break,
                Err(_e @ AppError::TokenInvalid) => {
                    warn!("下载任务因 Token 失效而中断。");
                    if self.context.non_interactive {
                        error!("非交互模式下 Token 失效，无法继续。");
                        return Err(AppError::TokenInvalid);
                    }
                    let retry_result = self.handle_token_failure_and_retry(&final_tasks).await?;
                    if retry_result.should_abort {
                        info!("用户选择中止任务。");
                        return Ok(false);
                    }
                    if let Some(remaining) = retry_result.remaining_tasks {
                        tasks_to_attempt = remaining;
                    } else {
                        break;
                    }
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

    /// 获取用户选择
    fn get_user_selection(&self, items: &[FileInfo]) -> AppResult<Vec<usize>> {
        debug!("get_user_selection 接收到列表 (共 {} 项):", items.len());

        // 为每个待选项生成一个唯一的、用户友好的显示字符串
        // 使用 .unique() 来处理协商后可能产生的重复显示项（如同一个音轨选择了多种格式）
        let options: Vec<String> = items
            .iter()
            .map(|item| {
                let date_str = item.date.map_or("[ 日期未知 ]".to_string(), |d| {
                    format!("[{}]", d.format("%Y-%m-%d"))
                });

                // 使用完整文件名（包括后缀）
                let filename = item.filepath.file_name().unwrap().to_string_lossy();
                let truncated_name =
                    utils::truncate_text(&filename, constants::FILENAME_TRUNCATE_LENGTH);

                format!("{} {}", date_str, truncated_name)
            })
            .collect();

        let user_input = if self.context.non_interactive {
            self.context.args.select.clone()
        } else {
            ui::selection_menu(
                &options,
                "文件下载列表",
                "支持格式: 1, 3, 2-4, all",
                &self.context.args.select,
            )
        };

        // --- 逻辑简化：现在可以直接解析索引，无需反向匹配 ---
        let indices = utils::parse_selection_indices(&user_input, options.len());

        debug!(
            "根据用户输入 '{}'，解析出的索引为: {:?}",
            user_input, indices
        );
        Ok(indices)
    }
}
