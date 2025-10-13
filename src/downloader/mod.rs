// src/downloader/mod.rs

mod m3u8;

use self::m3u8::M3u8Downloader;
use crate::{
    DownloadJobContext,
    cli::{Cli, ResourceType},
    config::{self, ResourceExtractorType},
    constants,
    error::*,
    models::*,
    symbols, ui, utils,
};
use anyhow::anyhow;
use colored::*;
use futures::{StreamExt, stream};
use indicatif::{HumanBytes, ProgressBar, ProgressStyle};
use log::{debug, error, info, warn};
use reqwest::{StatusCode, header};
use std::{
    cmp::min,
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::Write as IoWrite,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use url::Url;

#[derive(Clone, Default)]
pub struct DownloadStats {
    pub total: usize,
    pub success: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Clone)]
pub struct DownloadManager {
    stats: Arc<Mutex<DownloadStats>>,
    failed_downloads: Arc<Mutex<Vec<(String, String)>>>,
    skipped_downloads: Arc<Mutex<Vec<(String, String)>>>,
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            stats: Arc::new(Mutex::new(DownloadStats::default())),
            failed_downloads: Arc::new(Mutex::new(Vec::new())),
            skipped_downloads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn start_batch(&self, total_tasks: usize) {
        info!("开始新一批下载任务，总数: {}", total_tasks);
        let mut stats = self.stats.lock().unwrap();
        *stats = DownloadStats {
            total: total_tasks,
            ..Default::default()
        };
        self.failed_downloads.lock().unwrap().clear();
        self.skipped_downloads.lock().unwrap().clear();
    }

    pub fn record_success(&self) {
        self.stats.lock().unwrap().success += 1;
    }
    pub fn record_skip(&self, filename: &str, reason: &str) {
        info!("跳过文件 '{}'，原因: {}", filename, reason);
        self.stats.lock().unwrap().skipped += 1;
        self.skipped_downloads
            .lock()
            .unwrap()
            .push((filename.to_string(), reason.to_string()));
    }
    pub fn record_failure(&self, filename: &str, status: DownloadStatus) {
        error!("文件 '{}' 下载失败，状态: {:?}", filename, status);
        self.stats.lock().unwrap().failed += 1;
        let (_, _, msg) = status.get_display_info();
        self.failed_downloads
            .lock()
            .unwrap()
            .push((filename.to_string(), msg.to_string()));
    }
    pub fn reset_token_failures(&self, filenames_to_reset: &[String]) {
        let mut failed_downloads = self.failed_downloads.lock().unwrap();
        let original_len = failed_downloads.len();
        failed_downloads.retain(|(name, _)| !filenames_to_reset.contains(name));
        let removed_count = original_len - failed_downloads.len();
        if removed_count > 0 {
            info!("重置了 {} 个因Token失败的任务", removed_count);
            self.stats.lock().unwrap().failed -= removed_count;
        }
    }
    pub fn get_stats(&self) -> DownloadStats {
        self.stats.lock().unwrap().clone()
    }
    pub fn did_all_succeed(&self) -> bool {
        self.stats.lock().unwrap().failed == 0
    }

    pub fn print_report(&self) {
        let stats = self.get_stats();
        let skipped = self.skipped_downloads.lock().unwrap();
        let failed = self.failed_downloads.lock().unwrap();
        info!(
            "下载报告: Total={}, Success={}, Skipped={}, Failed={}",
            stats.total, stats.success, stats.skipped, stats.failed
        );

        if !skipped.is_empty() || !failed.is_empty() {
            ui::print_sub_header("下载详情报告");
            if !skipped.is_empty() {
                println!("\n{} 跳过的文件 ({}个):", *symbols::INFO, stats.skipped);
                print_grouped_report(&skipped, |s| s.cyan());
            }
            if !failed.is_empty() {
                println!("\n{} 失败的文件 ({}个):", *symbols::ERROR, stats.failed);
                print_grouped_report(&failed, |s| s.red());
            }
        }
        ui::print_sub_header("任务总结");
        if stats.success == 0 && stats.failed == 0 && stats.skipped > 0 {
            println!(
                "{} 所有 {} 个文件均已存在且有效，无需操作。",
                *symbols::OK,
                stats.total
            );
        } else {
            let summary = format!(
                "{} | {} | {}",
                format!("成功: {}", stats.success).green(),
                format!("失败: {}", stats.failed).red(),
                format!("跳过: {}", stats.skipped).yellow()
            );
            println!("{}", summary);
        }

        fn print_grouped_report(
            items: &[(String, String)],
            color_fn: fn(ColoredString) -> ColoredString,
        ) {
            let mut grouped: HashMap<&String, Vec<&String>> = HashMap::new();
            for (filename, reason) in items {
                grouped.entry(reason).or_default().push(filename);
            }
            let mut sorted_reasons: Vec<_> = grouped.keys().collect();
            sorted_reasons.sort();
            for reason in sorted_reasons {
                println!("  - {}", color_fn(format!("原因: {}", reason).into()));
                let mut filenames = grouped.get(reason).unwrap().clone();
                filenames.sort();
                for filename in filenames {
                    println!("    - {}", filename);
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ValidationStatus {
    Valid,
    Invalid(String),
    CanResume(u64),
    NoInfoToValidate,
}

pub struct ResourceDownloader {
    context: DownloadJobContext,
}

impl ResourceDownloader {
    pub fn new(context: DownloadJobContext) -> Self {
        Self { context }
    }

    fn setup_progress_bar(tasks: &[FileInfo], max_workers: usize) -> ProgressBar {
        // 将这里的 all_sizes_available 强制设为 false 来测试回退样式
        // let all_sizes_available = false;
        let all_sizes_available = tasks
            .iter()
            .all(|t| t.ti_size.is_some() && t.ti_size.unwrap() > 0);
        
        let pbar: ProgressBar;

        if all_sizes_available {
            let total_size: u64 = tasks.iter().map(|t| t.ti_size.unwrap_or(0)).sum();
            println!(
                "\n{} 开始下载 {} 个文件 (总大小: {}) (并发数: {})...",
                *symbols::INFO,
                tasks.len(),
                HumanBytes(total_size),
                max_workers
            );

            pbar = ProgressBar::new(total_size);
            let pbar_style = ProgressStyle::with_template(
                "{prefix:4.cyan.bold}: [{elapsed_precise}] [{bar:40.green/white.dim}] {percent:>3}% | {bytes:>10}/{total_bytes:<10} | {bytes_per_sec:<10} | ETA: {eta_precise}"
            )
            .unwrap()
            .progress_chars("━╸ ");
            pbar.set_style(pbar_style);
            pbar.set_prefix("下载");
        } else {
            println!(
                "\n{} 部分文件大小未知，将按文件数量显示进度。",
                *symbols::WARN
            );
            println!(
                "{} 开始下载 {} 个文件 (并发数: {})...",
                *symbols::INFO,
                tasks.len(),
                max_workers
            );

            pbar = ProgressBar::new(tasks.len() as u64);
            let pbar_style = ProgressStyle::with_template(
                "{prefix:4.yellow.bold}: [{elapsed_precise}] [{bar:40.yellow/white.dim}] {pos}/{len} ({percent}%) ETA: {eta}"
            ).unwrap().progress_chars("#>-");
            pbar.set_style(pbar_style);
            pbar.set_prefix("任务");
        }
        
        pbar
    }

    pub async fn run(&self, url: &str) -> AppResult<bool> {
        info!("开始处理 URL: {}", url);
        let (extractor, resource_id) = self.get_extractor_info(url)?;
        self.prepare_and_run(extractor, &resource_id).await
    }

    pub async fn run_with_id(&self, resource_id: &str) -> AppResult<bool> {
        info!("开始处理 ID: {}", resource_id);
        let resource_type_enum = self.context.args.r#type.as_ref().unwrap(); // 这是 ResourceType 枚举

        // 将枚举转换为字符串 key
        let type_key = match resource_type_enum {
            ResourceType::TchMaterial => "tchMaterial",
            ResourceType::QualityCourse => "qualityCourse",
            ResourceType::SyncClassroom => "syncClassroom/classActivity",
        };

        let api_conf = self.context.config.api_endpoints.get(type_key).unwrap();
        let extractor = self.create_extractor(api_conf);
        self.prepare_and_run(extractor, resource_id).await
    }

    async fn prepare_and_run(
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

        let mut all_file_items = extractor
            .extract_file_info(resource_id, &self.context)
            .await?;
        if all_file_items.is_empty() {
            println!("\n{} 未能提取到任何可下载的文件信息。", *symbols::INFO);
            return Ok(true);
        }
        debug!("提取到 {} 个文件项。", all_file_items.len());

        for item in &mut all_file_items {
            item.filepath = utils::secure_join_path(&base_output_dir, &item.filepath)?;
        }

        let indices = if all_file_items.len() == 1 {
            vec![0]
        } else {
            self.display_files_and_prompt_selection(&all_file_items)?
        };

        if indices.is_empty() {
            println!("\n{} 未选择任何文件，任务结束。", *symbols::INFO);
            return Ok(true);
        }

        let tasks_to_run: Vec<FileInfo> = indices
            .into_iter()
            .map(|i| all_file_items[i].clone())
            .collect();
        debug!("最终确定了 {} 个下载任务。", tasks_to_run.len());

        let mut tasks_to_attempt = tasks_to_run.clone();
        self.context.manager.start_batch(tasks_to_attempt.len());

        loop {
            match self.execute_download_tasks(&tasks_to_attempt).await {
                Ok(_) => break,
                Err(AppError::TokenInvalid) => {
                    warn!("下载任务因 Token 失效而中断。");
                    if self.context.non_interactive {
                        error!("非交互模式下 Token 失效，无法继续。");
                        return Err(AppError::TokenInvalid);
                    }
                    let retry_result = self.handle_token_failure_and_retry(&tasks_to_run).await?;
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

    fn get_extractor_info(
        &self,
        url_str: &str,
    ) -> AppResult<(Box<dyn crate::extractor::ResourceExtractor>, String)> {
        let url = Url::parse(url_str)?;
        debug!("解析 URL: {}", url);
        for (path_key, api_conf) in &self.context.config.api_endpoints {
            if url.path().contains(path_key) {
                debug!("URL 路径匹配 API 端点: '{}'", path_key);
                if let Some(resource_id) = url.query_pairs().find(|(k, _)| k == &api_conf.id_param)
                {
                    let id = resource_id.1.to_string();
                    if utils::is_resource_id(&id) {
                        info!("从 URL 中成功提取到资源 ID: '{}' (类型: {})", id, path_key);
                        return Ok((self.create_extractor(api_conf), id));
                    }
                }
            }
        }
        error!("无法从 URL '{}' 中识别资源类型或提取ID。", url_str);
        Err(AppError::Other(anyhow!(
            "无法识别的URL格式或不支持的资源类型。"
        )))
    }

    fn create_extractor(
        &self,
        api_conf: &crate::config::ApiEndpointConfig,
    ) -> Box<dyn crate::extractor::ResourceExtractor> {
        match api_conf.extractor {
            ResourceExtractorType::Textbook => {
                debug!("创建 TextbookExtractor");
                Box::new(crate::extractor::textbook::TextbookExtractor::new(
                    self.context.http_client.clone(),
                    self.context.config.clone(),
                ))
            }
            ResourceExtractorType::Course => {
                let template_key = api_conf
                    .url_template_keys
                    .get("main")
                    .expect("Course API config missing 'main' template key");
                let url_template = self
                    .context
                    .config
                    .url_templates
                    .get(template_key)
                    .expect("URL template not found for key")
                    .clone();
                debug!("创建 CourseExtractor, 使用 URL 模板: {}", url_template);
                Box::new(crate::extractor::course::CourseExtractor::new(
                    self.context.http_client.clone(),
                    self.context.config.clone(),
                    url_template,
                ))
            }
        }
    }

    async fn handle_token_failure_and_retry(
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
                    match ui::prompt_hidden("请输入新 Token (输入不可见，完成后按回车)")
                    {
                        Ok(new_token) if !new_token.is_empty() => {
                            info!("用户输入了新的 Token。");
                            *self.context.token.lock().await = new_token.clone();
                            if ui::confirm("是否保存此新 Token 以便后续使用?", false) {
                                #[allow(clippy::collapsible_if)]
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
                Ok(_) => continue,
                Err(_) => {
                    warn!("用户在 Token 提示处中断。");
                    return Ok(TokenRetryResult {
                        remaining_tasks: None,
                        should_abort: true,
                    });
                }
            }
        }
        println!("\n{} Token 已更新。正在检查剩余任务...", *symbols::INFO);
        let mut remaining_tasks = vec![];
        let mut remaining_filenames = vec![];
        for task in initial_tasks {
            let (action, _, _) = Self::prepare_download_action(task, &self.context.args)?;
            if action != DownloadAction::Skip {
                remaining_tasks.push(task.clone());
                remaining_filenames.push(task.filepath.to_string_lossy().into_owned());
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

    fn display_files_and_prompt_selection(&self, items: &[FileInfo]) -> AppResult<Vec<usize>> {
        let options: Vec<String> = items
            .iter()
            .map(|item| {
                let date_str = item.date.map_or("[ 日期未知 ]".to_string(), |d| {
                    format!("[{}]", d.format("%Y-%m-%d"))
                });
                let filename_str = utils::truncate_text(
                    &item.filepath.file_name().unwrap().to_string_lossy(),
                    constants::FILENAME_TRUNCATE_LENGTH,
                );
                format!("{} {}", date_str, filename_str)
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
        let indices = utils::parse_selection_indices(&user_input, items.len());
        debug!("用户选择的下载索引: {:?}", indices);
        Ok(indices)
    }

    async fn execute_download_tasks(&self, tasks: &[FileInfo]) -> AppResult<()> {
        let max_workers = min(self.context.config.max_workers, tasks.len());
        if max_workers == 0 {
            return Ok(());
        }

        // --- 调用新函数来创建和配置进度条 ---
        let main_pbar = Self::setup_progress_bar(tasks, max_workers);
        
        // --- 确定后续是否使用字节进度 ---
        // 注意：这里的 all_sizes_available 变量在函数局部仍然需要，用于后续的逻辑判断
        let all_sizes_available = tasks
            .iter()
            .all(|t| t.ti_size.is_some() && t.ti_size.unwrap() > 0);

        main_pbar.enable_steady_tick(Duration::from_millis(100));

        let error_sender = Arc::new(tokio::sync::Mutex::new(None::<AppError>));
        let tasks_stream = stream::iter(tasks.to_owned());

        tasks_stream
            .for_each_concurrent(max_workers, |task| {
                let context = self.context.clone();
                let main_pbar = main_pbar.clone();
                let error_sender = error_sender.clone();

                async move {
                    if context.cancellation_token.load(Ordering::Relaxed) {
                        return;
                    }
                    if error_sender.lock().await.is_some() {
                        return;
                    }

                    let result = Self::process_single_task(
                        task.clone(),
                        context.clone(),
                        main_pbar.clone(),
                        all_sizes_available,
                    )
                    .await;

                    match result {
                        Ok(result) => {
                            match result.status {
                                DownloadStatus::Success | DownloadStatus::Resumed => {
                                    context.manager.record_success()
                                }
                                DownloadStatus::Skipped => context.manager.record_skip(
                                    &result.filename,
                                    result.message.as_deref().unwrap_or("文件已存在"),
                                ),
                                _ => context
                                    .manager
                                    .record_failure(&result.filename, result.status),
                            }

                            match result.status {
                                DownloadStatus::Skipped => {
                                    if all_sizes_available {
                                        if let Some(skipped_size) = task.ti_size {
                                            main_pbar.inc(skipped_size);
                                        }
                                    } else {
                                        main_pbar.inc(1);
                                    }
                                }
                                _ => {
                                    if !all_sizes_available {
                                        main_pbar.inc(1);
                                    }
                                }
                            }

                            if result.status != DownloadStatus::Skipped {
                                let (symbol, color_fn, default_msg) =
                                    result.status.get_display_info();
                                let task_name =
                                    task.filepath.file_name().unwrap().to_string_lossy();
                                let msg = if let Some(err_msg) = result.message {
                                    let error_details =
                                        format!("失败: {} (详情: {})", default_msg, err_msg);
                                    format!(
                                        "\n{} {} {}",
                                        symbol,
                                        task_name,
                                        color_fn(error_details.into())
                                    )
                                } else {
                                    format!("{} {}", symbol, task_name)
                                };
                                main_pbar.println(msg);
                            }
                        }
                        Err(e @ AppError::TokenInvalid) => {
                            let mut error_lock = error_sender.lock().await;
                            if error_lock.is_none() {
                                let task_name = task.filepath.to_string_lossy();
                                error!("任务 '{}' 因 Token 失效失败，将中止整个批次。", task_name);
                                context
                                    .manager
                                    .record_failure(&task_name, DownloadStatus::TokenError);
                                *error_lock = Some(e);
                            }
                        }
                        Err(e) => {
                            error!("未捕获的错误在并发循环中: {}", e);
                            if !all_sizes_available {
                                main_pbar.inc(1);
                            }
                        }
                    }
                }
            })
            .await;

        // main_pbar.finish_with_message("所有下载任务完成!");
        main_pbar.finish_and_clear();

        if self.context.cancellation_token.load(Ordering::Relaxed) {
            return Err(AppError::UserInterrupt);
        }
        if let Some(err) = error_sender.lock().await.take() {
            return Err(err);
        }
        Ok(())
    }

    async fn process_single_task(
        item: FileInfo,
        context: DownloadJobContext,
        pbar: ProgressBar,
        use_byte_progress: bool,
    ) -> AppResult<DownloadResult> {
        debug!("开始处理单个下载任务: {:?}", item.filepath);
        let attempt_result: AppResult<DownloadResult> = async {
            if let Some(parent) = item.filepath.parent() {
                fs::create_dir_all(parent)?;
            }
            let (action, resume_bytes, reason) =
                Self::prepare_download_action(&item, &context.args)?;
            debug!(
                "文件 '{:?}' 的下载操作为: {:?} (resume_bytes: {}, reason: '{}')",
                item.filepath, action, resume_bytes, reason
            );

            if action == DownloadAction::Skip {
                return Ok(DownloadResult {
                    filename: item
                        .filepath
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                    status: DownloadStatus::Skipped,
                    message: Some(reason),
                });
            }

            let is_m3u8 = item.url.ends_with(constants::api::resource_formats::M3U8);
            let download_status = if is_m3u8 {
                M3u8Downloader::new(context.clone())
                    .download(&item, pbar, use_byte_progress)
                    .await?
            } else {
                Self::download_standard_file(&item, resume_bytes, &context, pbar, use_byte_progress)
                    .await?
            };

            let final_status = if download_status == DownloadStatus::Success
                || download_status == DownloadStatus::Resumed
            {
                Self::finalize_and_validate(&item)?
            } else {
                download_status
            };

            Ok(DownloadResult {
                filename: item
                    .filepath
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                status: final_status,
                message: None,
            })
        }
        .await;
        match attempt_result {
            Ok(result) => Ok(result),
            Err(e @ AppError::TokenInvalid) => Err(e),
            Err(e) => {
                error!("处理任务 '{:?}' 时发生错误: {}", item.filepath, e);
                Ok(DownloadResult {
                    filename: item
                        .filepath
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                    status: DownloadStatus::from(&e),
                    message: Some(e.to_string()),
                })
            }
        }
    }

    fn check_local_file_status(item: &FileInfo) -> AppResult<ValidationStatus> {
        if !item.filepath.exists() {
            return Ok(ValidationStatus::Invalid("文件不存在".to_string()));
        }
        let actual_size = item.filepath.metadata()?.len();
        if actual_size == 0 {
            return Ok(ValidationStatus::Invalid("文件为空(0字节)".to_string()));
        }
        if item.ti_md5.is_none() && item.ti_size.is_none() {
            return Ok(ValidationStatus::NoInfoToValidate);
        }
        if let Some(expected_size) = item.ti_size &&  actual_size != expected_size {
            let is_m3u8 = item.url.ends_with(".m3u8");
            let tolerance = if is_m3u8 {
                (expected_size as f64 * 0.01) as u64
            } else {
                0
            };

            if (actual_size as i64 - expected_size as i64).unsigned_abs() > tolerance {
                if actual_size < expected_size {
                    return Ok(ValidationStatus::CanResume(actual_size));
                } else {
                    return Ok(ValidationStatus::Invalid(format!(
                        "大小错误 (预期: {}, 实际: {})",
                        HumanBytes(expected_size),
                        HumanBytes(actual_size)
                    )));
                }
            }
        }
        if let Some(expected_md5) = &item.ti_md5 {
            let actual_md5 = utils::calculate_file_md5(&item.filepath)?;
            if !actual_md5.eq_ignore_ascii_case(expected_md5) {
                return Ok(ValidationStatus::Invalid("MD5不匹配".to_string()));
            }
        }
        Ok(ValidationStatus::Valid)
    }

    fn prepare_download_action(
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
            ValidationStatus::Valid => {
                Ok((DownloadAction::Skip, 0, "文件已存在且校验通过".to_string()))
            }
            ValidationStatus::CanResume(from) => Ok((
                DownloadAction::Resume,
                from,
                "文件不完整，尝试续传".to_string(),
            )),
            ValidationStatus::Invalid(reason) => Ok((
                DownloadAction::DownloadNew,
                0,
                format!("文件无效: {}", reason),
            )),
            ValidationStatus::NoInfoToValidate => Ok((
                DownloadAction::Skip,
                0,
                "文件已存在 (无校验信息)".to_string(),
            )),
        }
    }

    fn finalize_and_validate(item: &FileInfo) -> AppResult<DownloadStatus> {
        debug!("对文件 '{:?}' 进行最终校验", item.filepath);
        match Self::check_local_file_status(item)? {
            ValidationStatus::Valid | ValidationStatus::NoInfoToValidate => {
                Ok(DownloadStatus::Success)
            }
            ValidationStatus::CanResume(_) => {
                error!("文件 '{:?}' 下载后仍不完整，校验失败。", item.filepath);
                Ok(DownloadStatus::SizeFailed)
            }
            ValidationStatus::Invalid(reason) => {
                error!("文件 '{:?}' 最终校验失败: {}", item.filepath, reason);
                if reason.contains("MD5") {
                    Ok(DownloadStatus::Md5Failed)
                } else {
                    Ok(DownloadStatus::SizeFailed)
                }
            }
        }
    }

    async fn download_standard_file(
        item: &FileInfo,
        resume_from: u64,
        context: &DownloadJobContext,
        pbar: ProgressBar,
        use_byte_progress: bool,
    ) -> AppResult<DownloadStatus> {
        let mut current_resume_from = resume_from;
        loop {
            let mut url = Url::parse(&item.url)?;
            let token = context.token.lock().await;
            if !token.is_empty() {
                url.query_pairs_mut().append_pair("accessToken", &token);
            }

            let mut request_builder = context.http_client.client.get(url.clone());
            if current_resume_from > 0 {
                debug!("尝试从 {} 字节处续传: {}", current_resume_from, url);
                request_builder = request_builder
                    .header(header::RANGE, format!("bytes={}-", current_resume_from));
            }
            drop(token);
            let res = request_builder.send().await?;

            if res.status() == StatusCode::RANGE_NOT_SATISFIABLE {
                warn!(
                    "续传点 {} 无效，将从头开始下载: {}",
                    current_resume_from,
                    &item.filepath.display()
                );
                println!(
                    "{} 续传点无效，将从头开始下载: {}",
                    *symbols::WARN,
                    &item.filepath.display()
                );
                current_resume_from = 0;
                if item.filepath.exists() {
                    fs::remove_file(&item.filepath)?;
                }
                continue;
            }
            if res.status() == StatusCode::UNAUTHORIZED || res.status() == StatusCode::FORBIDDEN {
                return Err(AppError::TokenInvalid);
            }
            let res = res.error_for_status()?;

            let mut file = if current_resume_from > 0 {
                OpenOptions::new()
                    .append(true)
                    .open(&item.filepath)?
            } else {
                File::create(&item.filepath)?
            };

            let mut stream = res.bytes_stream();
            while let Some(chunk_res) = stream.next().await {
                let chunk = chunk_res?;
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
