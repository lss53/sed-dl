// src/downloader/mod.rs

mod m3u8;

use self::m3u8::M3u8Downloader;
use crate::{
    cli::Cli,
    config::{self, ResourceExtractorType},
    constants,
    error::*,
    models::*,
    symbols, ui, utils, DownloadJobContext,
};
use anyhow::anyhow;
use colored::*;
use futures::{stream, StreamExt};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use log::{debug, error, info, trace, warn};
use reqwest::{header, StatusCode};
use std::{
    cmp::min,
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
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

    pub fn record_success(&self) { self.stats.lock().unwrap().success += 1; }
    pub fn record_skip(&self, filename: &str, reason: &str) {
        info!("跳过文件 '{}'，原因: {}", filename, reason);
        self.stats.lock().unwrap().skipped += 1;
        self.skipped_downloads.lock().unwrap().push((filename.to_string(), reason.to_string()));
    }
    pub fn record_failure(&self, filename: &str, status: DownloadStatus) {
        error!("文件 '{}' 下载失败，状态: {:?}", filename, status);
        self.stats.lock().unwrap().failed += 1;
        let (_, _, msg) = get_status_display_info(status);
        self.failed_downloads.lock().unwrap().push((filename.to_string(), msg.to_string()));
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
    pub fn get_stats(&self) -> DownloadStats { self.stats.lock().unwrap().clone() }
    pub fn did_all_succeed(&self) -> bool { self.stats.lock().unwrap().failed == 0 }

    pub fn print_report(&self) {
        let stats = self.get_stats();
        let skipped = self.skipped_downloads.lock().unwrap();
        let failed = self.failed_downloads.lock().unwrap();
        info!("下载报告: Total={}, Success={}, Skipped={}, Failed={}", stats.total, stats.success, stats.skipped, stats.failed);

        if !skipped.is_empty() || !failed.is_empty() {
            ui::print_sub_header("下载详情报告");
            if !skipped.is_empty() {
                println!("\n{} 跳过的文件 ({}个):", *symbols::INFO, stats.skipped);
                print_grouped_report(&skipped);
            }
            if !failed.is_empty() {
                println!("\n{} 失败的文件 ({}个):", *symbols::ERROR, stats.failed);
                print_grouped_report(&failed);
            }
        }
        ui::print_sub_header("任务总结");
        if stats.success == 0 && stats.failed == 0 && stats.skipped > 0 {
            println!("{} 所有 {} 个文件均已存在且有效，无需操作。", *symbols::OK, stats.total);
        } else {
            let summary = format!("{} | {} | {}",
                format!("成功: {}", stats.success).green(),
                format!("失败: {}", stats.failed).red(),
                format!("跳过: {}", stats.skipped).yellow()
            );
            println!("{}", summary);
        }

        fn print_grouped_report(items: &[(String, String)]) {
            let mut grouped: HashMap<&String, Vec<&String>> = HashMap::new();
            for (filename, reason) in items { grouped.entry(reason).or_default().push(filename); }
            let mut sorted_reasons: Vec<_> = grouped.keys().collect();
            sorted_reasons.sort();
            for reason in sorted_reasons {
                println!("  - 原因: {}", reason);
                let mut filenames = grouped.get(reason).unwrap().clone();
                filenames.sort();
                for filename in filenames { println!("    - {}", filename); }
            }
        }
    }
}

pub fn get_status_display_info(status: DownloadStatus) -> (ColoredString, fn(ColoredString) -> ColoredString, &'static str) {
    match status {
        DownloadStatus::Success => (symbols::OK.clone(), |s| s.green(), "下载并校验成功"),
        DownloadStatus::Resumed => (symbols::OK.clone(), |s| s.green(), "续传成功，文件有效"),
        DownloadStatus::Skipped => (symbols::INFO.clone(), |s| s.cyan(), "文件已存在，跳过"),
        DownloadStatus::Md5Failed => (symbols::ERROR.clone(), |s| s.red(), "校验失败 (MD5不匹配)"),
        DownloadStatus::SizeFailed => (symbols::ERROR.clone(), |s| s.red(), "校验失败 (大小不匹配)"),
        DownloadStatus::HttpError => (symbols::ERROR.clone(), |s| s.red(), "服务器返回错误"),
        DownloadStatus::NetworkError => (symbols::ERROR.clone(), |s| s.red(), "网络请求失败"),
        DownloadStatus::ConnectionError => (symbols::ERROR.clone(), |s| s.red(), "无法建立连接"),
        DownloadStatus::TimeoutError => (symbols::WARN.clone(), |s| s.yellow(), "网络连接超时"),
        DownloadStatus::MergeError => (symbols::ERROR.clone(), |s| s.red(), "视频分片合并失败"),
        DownloadStatus::KeyError => (symbols::ERROR.clone(), |s| s.red(), "视频解密密钥获取失败"),
        DownloadStatus::TokenError => (symbols::ERROR.clone(), |s| s.red(), "认证失败 (Token无效)"),
        DownloadStatus::IoError => (symbols::ERROR.clone(), |s| s.red(), "本地文件读写错误"),
        DownloadStatus::UnexpectedError => (symbols::ERROR.clone(), |s| s.red(), "发生未预期的程序错误"),
    }
}

pub struct ResourceDownloader {
    context: DownloadJobContext,
    m_progress: MultiProgress,
}

impl ResourceDownloader {
    pub fn new(context: DownloadJobContext) -> Self {
        Self {
            context,
            m_progress: MultiProgress::new(),
        }
    }

    pub async fn run(&self, url: &str) -> AppResult<bool> {
        info!("开始处理 URL: {}", url);
        let (extractor, resource_id) = self.get_extractor_info(url)?;
        self.prepare_and_run(extractor, &resource_id).await
    }

    pub async fn run_with_id(&self, resource_id: &str) -> AppResult<bool> {
        info!("开始处理 ID: {}", resource_id);
        let r#type = self.context.args.r#type.as_ref().unwrap();
        let api_conf = self.context.config.api_endpoints.get(r#type).unwrap();
        let extractor = self.create_extractor(api_conf);
        self.prepare_and_run(extractor, resource_id).await
    }

    async fn prepare_and_run(&self, extractor: Box<dyn crate::extractor::ResourceExtractor>, resource_id: &str) -> AppResult<bool> {
        let base_output_dir = self.context.args.output.clone();
        fs::create_dir_all(&base_output_dir)?;
        let absolute_path = dunce::canonicalize(&base_output_dir)?;
        info!("文件将保存到目录: \"{}\"", absolute_path.display());
        println!("\n{} 文件将保存到目录: \"{}\"", *symbols::INFO, absolute_path.display());

        let mut all_file_items = extractor.extract_file_info(resource_id, &self.context).await?;
        if all_file_items.is_empty() {
            println!("\n{} 未能提取到任何可下载的文件信息。", *symbols::INFO);
            return Ok(true);
        }
        debug!("提取到 {} 个文件项。", all_file_items.len());

        for item in &mut all_file_items {
            item.filepath = utils::secure_join_path(&base_output_dir, &item.filepath)?;
        }

        let indices = if all_file_items.len() == 1 { vec![0] } else { self.display_files_and_prompt_selection(&all_file_items)? };

        if indices.is_empty() {
            println!("\n{} 未选择任何文件，任务结束。", *symbols::INFO);
            return Ok(true);
        }
        let mut tasks_to_run: Vec<FileInfo> = indices.into_iter().map(|i| all_file_items[i].clone()).collect();
        tasks_to_run.sort_by_key(|item| item.ti_size.unwrap_or(0));
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

    fn get_extractor_info(&self, url_str: &str) -> AppResult<(Box<dyn crate::extractor::ResourceExtractor>, String)> {
        let url = Url::parse(url_str)?;
        debug!("解析 URL: {}", url);
        for (path_key, api_conf) in &self.context.config.api_endpoints {
            if url.path().contains(path_key) {
                debug!("URL 路径匹配 API 端点: '{}'", path_key);
                if let Some(resource_id) = url.query_pairs().find(|(k, _)| k == &api_conf.id_param) {
                    let id = resource_id.1.to_string();
                    if utils::is_resource_id(&id) {
                        info!("从 URL 中成功提取到资源 ID: '{}' (类型: {})", id, path_key);
                        return Ok((self.create_extractor(api_conf), id));
                    }
                }
            }
        }
        error!("无法从 URL '{}' 中识别资源类型或提取ID。", url_str);
        Err(AppError::Other(anyhow!("无法识别的URL格式或不支持的资源类型。")))
    }

    fn create_extractor(&self, api_conf: &crate::config::ApiEndpointConfig) -> Box<dyn crate::extractor::ResourceExtractor> {
        match api_conf.extractor {
            ResourceExtractorType::Textbook => {
                debug!("创建 TextbookExtractor");
                Box::new(crate::extractor::textbook::TextbookExtractor::new(self.context.http_client.clone(), self.context.config.clone()))
            }
            ResourceExtractorType::Course => {
                let template_key = api_conf.url_template_keys.get("main").expect("Course API config missing 'main' template key");
                let url_template = self.context.config.url_templates.get(template_key).expect("URL template not found for key").clone();
                debug!("创建 CourseExtractor, 使用 URL 模板: {}", url_template);
                Box::new(crate::extractor::course::CourseExtractor::new(self.context.http_client.clone(), self.context.config.clone(), url_template))
            }
        }
    }

    async fn handle_token_failure_and_retry(&self, initial_tasks: &[FileInfo]) -> AppResult<TokenRetryResult> {
        ui::box_message("认证失败", &["当前 Access Token 已失效或无权限访问。", "输入 '2' 可以查看获取 Token 的详细指南。"], |s| s.red());
        loop {
            let prompt_msg = format!("选择操作: [1] 输入新 Token  [2] 查看帮助 (按 {} 中止)", *symbols::CTRL_C);
            match ui::prompt(&prompt_msg, Some("1")) {
                Ok(choice) if choice == "1" => {
                    match ui::prompt_hidden("请输入新 Token (输入不可见，完成后按回车)") {
                        Ok(new_token) if !new_token.is_empty() => {
                            info!("用户输入了新的 Token。");
                            *self.context.token.lock().await = new_token.clone();
                            if ui::confirm("是否保存此新 Token 以便后续使用?", false) {
                                if let Err(e) = config::save_token(&new_token) {
                                    error!("尝试保存新Token时失败: {}", e);
                                    eprintln!("{} 保存新Token失败: {}", *symbols::WARN, e);
                                    }
                                }
                            break;
                        }
                        _ => println!("{}", "Token 不能为空。".yellow()),
                    }
                }
                Ok(choice) if choice == "2" => { ui::box_message("获取 Access Token 指南", constants::HELP_TOKEN_GUIDE.lines().collect::<Vec<_>>().as_slice(), |s| s.cyan()); }
                Ok(_) => continue,
                Err(_) => {
                    warn!("用户在 Token 提示处中断。");
                    return Ok(TokenRetryResult { remaining_tasks: None, should_abort: true });
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
            return Ok(TokenRetryResult { remaining_tasks: None, should_abort: false });
        }
        self.context.manager.reset_token_failures(&remaining_filenames);
        info!("准备重试剩余的 {} 个任务。", remaining_tasks.len());
        println!("{} 准备重试剩余的 {} 个任务...", *symbols::INFO, remaining_tasks.len());
        Ok(TokenRetryResult { remaining_tasks: Some(remaining_tasks), should_abort: false })
    }

    fn display_files_and_prompt_selection(&self, items: &[FileInfo]) -> AppResult<Vec<usize>> {
        let options: Vec<String> = items.iter().map(|item| {
            let date_str = item.date.map_or("[ 日期未知 ]".to_string(), |d| format!("[{}]", d.format("%Y-%m-%d")));
            let filename_str = utils::truncate_text(&item.filepath.file_name().unwrap().to_string_lossy(), constants::FILENAME_TRUNCATE_LENGTH);
            format!("{} {}", date_str, filename_str)
        }).collect();
        let user_input = if self.context.non_interactive {
            self.context.args.select.clone()
        } else {
            ui::selection_menu(&options, "文件下载列表", "支持格式: 1, 3, 2-4, all", &self.context.args.select)
        };
        let indices = utils::parse_selection_indices(&user_input, items.len());
        debug!("用户选择的下载索引: {:?}", indices);
        Ok(indices)
    }

    async fn execute_download_tasks(&self, tasks: &[FileInfo]) -> AppResult<()> {
        let max_workers = min(self.context.config.max_workers, tasks.len());
        if max_workers == 0 { return Ok(()); }
        println!("\n{} 开始下载 {} 个文件 (并发数: {})...", *symbols::INFO, tasks.len(), max_workers);

        let task_id_counter = Arc::new(AtomicU64::new(0));
        let active_tasks = Arc::new(Mutex::new(BTreeMap::<u64, String>::new()));
        let main_pbar_style = ProgressStyle::with_template("{prefix:7.bold.cyan} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos:>3}/{len:3} ({percent:>3}%) [ETA: {eta}]").expect("进度条模板无效").progress_chars("#>-");
        let main_pbar = self.m_progress.add(ProgressBar::new(tasks.len() as u64));
        main_pbar.set_style(main_pbar_style.clone());
        main_pbar.set_prefix("总进度");
        main_pbar.enable_steady_tick(Duration::from_millis(100));

        let error_sender = Arc::new(tokio::sync::Mutex::new(None::<AppError>));
        let tasks_stream = stream::iter(tasks.to_owned());
        let update_msg = |pbar: &ProgressBar, tasks: &Mutex<BTreeMap<u64, String>>| {
            let guard = tasks.lock().unwrap();
            pbar.set_message(guard.values().cloned().collect::<Vec<String>>().join(", "));
        };
        update_msg(&main_pbar, &active_tasks);

        tasks_stream.for_each_concurrent(max_workers, |task| {
            let context = self.context.clone();
            let main_pbar = main_pbar.clone();
            let m_progress = self.m_progress.clone();
            let error_sender = error_sender.clone();
            let task_id_counter = task_id_counter.clone();
            let active_tasks = active_tasks.clone();
            async move {
                if context.cancellation_token.load(Ordering::Relaxed) { return; }
                if error_sender.lock().await.is_some() {
                    trace!("检测到全局错误，提前中止新任务的启动。");
                    return;
                }
                let task_id = task_id_counter.fetch_add(1, Ordering::Relaxed);
                let task_name = task.filepath.file_name().unwrap().to_string_lossy().to_string();
                active_tasks.lock().unwrap().insert(task_id, utils::truncate_text(&task_name, 30));
                update_msg(&main_pbar, &active_tasks);

                match Self::process_single_task(task.clone(), context.clone()).await {
                    Ok(result) => {
                        match result.status {
                            DownloadStatus::Success | DownloadStatus::Resumed => context.manager.record_success(),
                            DownloadStatus::Skipped => context.manager.record_skip(&result.filename, result.message.as_deref().unwrap_or("文件已存在")),
                            _ => context.manager.record_failure(&result.filename, result.status),
                        }
                        if result.status != DownloadStatus::Skipped {
                            let (symbol, _, default_msg) = get_status_display_info(result.status);
                            let msg = if let Some(err_msg) = result.message {
                                format!("\n{} 任务 '{}' 失败: {} (详情: {})", symbol, task_name, default_msg, err_msg)
                            } else {
                                format!("{} {}", symbol, task_name)
                            };
                            // 忽略打印结果，即使失败也不影响程序核心逻辑
                            let _ = m_progress.println(msg);
                        }
                    }
                    Err(e @ AppError::TokenInvalid) => {
                        let mut error_lock = error_sender.lock().await;
                        if error_lock.is_none() {
                            error!("任务 '{}' 因 Token 失效失败，将中止整个批次。", task_name);
                            context.manager.record_failure(&task.filepath.to_string_lossy(), DownloadStatus::TokenError);
                            *error_lock = Some(e);
                        }
                    }
                    Err(e) => { error!("未捕获的错误在并发循环中: {}", e); }
                }
                main_pbar.inc(1);
                active_tasks.lock().unwrap().remove(&task_id);
                update_msg(&main_pbar, &active_tasks);
            }
        }).await;
        main_pbar.finish_and_clear();

        if self.context.cancellation_token.load(Ordering::Relaxed) { return Err(AppError::UserInterrupt); }
        if let Some(err) = error_sender.lock().await.take() { return Err(err); }
        Ok(())
    }

    async fn process_single_task(item: FileInfo, context: DownloadJobContext) -> AppResult<DownloadResult> {
        debug!("开始处理单个下载任务: {:?}", item.filepath);
        let attempt_result: AppResult<DownloadResult> = async {
            if let Some(parent) = item.filepath.parent() { fs::create_dir_all(parent)?; }
            let (action, resume_bytes, reason) = Self::prepare_download_action(&item, &context.args)?;
            debug!("文件 '{:?}' 的下载操作为: {:?} (resume_bytes: {}, reason: '{}')", item.filepath, action, resume_bytes, reason);

            if action == DownloadAction::Skip {
                return Ok(DownloadResult {
                    filename: item.filepath.file_name().unwrap().to_string_lossy().to_string(),
                    status: DownloadStatus::Skipped,
                    message: Some(reason),
                });
            }
            let is_m3u8 = item.url.ends_with(constants::api::resource_formats::M3U8);
            let download_status = if is_m3u8 {
                M3u8Downloader::new(context.clone()).download(&item).await?
            } else {
                Self::download_standard_file(&item, resume_bytes, &context).await?
            };
            let final_status = if download_status == DownloadStatus::Success || download_status == DownloadStatus::Resumed {
                Self::finalize_and_validate(&item, is_m3u8)?
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

    fn prepare_download_action(item: &FileInfo, args: &Cli) -> AppResult<(DownloadAction, u64, String)> {
        if !item.filepath.exists() { return Ok((DownloadAction::DownloadNew, 0, "文件不存在".to_string())); }
        if args.force_redownload {
            info!("用户强制重新下载文件: {:?}", item.filepath);
            return Ok((DownloadAction::DownloadNew, 0, "强制重新下载".to_string()));
        }
        if item.url.ends_with(constants::api::resource_formats::M3U8) {
            info!("视频文件 {:?} 已存在，跳过下载。使用 -f 强制重下。", item.filepath);
            return Ok((DownloadAction::Skip, 0, "视频已存在 (使用 -f 强制重下)".to_string()));
        }
        match Self::validate_file(&item.filepath, item.ti_md5.as_deref(), item.ti_size) {
            Ok(_) => {
                info!("文件 {:?} 已存在且校验通过，跳过。使用 -f 强制重下。", item.filepath);
                Ok((DownloadAction::Skip, 0, "文件已存在且校验通过 (使用 -f 强制重下)".to_string()))
            }
            Err(e) => {
                warn!("文件 '{:?}' 存在但校验失败: {}", item.filepath, e);
                let actual_size = item.filepath.metadata()?.len();
                if let Some(expected_size) = item.ti_size {
                    if actual_size > 0 && actual_size < expected_size {
                        info!("文件 '{:?}' 不完整 ({} / {} bytes)，将进行续传。", item.filepath, actual_size, expected_size);
                        return Ok((DownloadAction::Resume, actual_size, "文件不完整，尝试续传".to_string()));
                    }
                }
                println!("{} {} - 文件已存在但校验失败，将重新下载。", *symbols::WARN, item.filepath.display());
                info!("文件 '{:?}' 存在但校验失败，将重新下载。", item.filepath);
                Ok((DownloadAction::DownloadNew, 0, "文件校验失败".to_string()))
            }
        }
    }

    fn finalize_and_validate(item: &FileInfo, is_m3u8: bool) -> AppResult<DownloadStatus> {
        debug!("对文件 '{:?}' 进行最终校验", item.filepath);
        if !item.filepath.exists() || item.filepath.metadata()?.len() == 0 {
            error!("下载完成但文件 '{:?}' 不存在或为空。", item.filepath);
            return Ok(DownloadStatus::SizeFailed);
        }
        if is_m3u8 { return Ok(DownloadStatus::Success); }
        match Self::validate_file(&item.filepath, item.ti_md5.as_deref(), item.ti_size) {
            Ok(_) => {
                debug!("文件 '{:?}' 校验成功。", item.filepath);
                Ok(DownloadStatus::Success)
            }
            Err(e @ AppError::Validation(_)) => {
                error!("文件 '{:?}' 最终校验失败: {}", item.filepath, e);
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    fn validate_file(filepath: &Path, expected_md5: Option<&str>, expected_size: Option<u64>) -> AppResult<()> {
        if let Some(size) = expected_size {
            let actual_size = filepath.metadata()?.len();
            if actual_size != size {
                trace!("大小校验失败 for '{:?}': expected {}, got {}", filepath, size, actual_size);
                return Err(AppError::Validation("大小不匹配".to_string()));
            }
        }
        if let Some(md5) = expected_md5 {
            let actual_md5 = utils::calculate_file_md5(filepath)?;
            if !actual_md5.eq_ignore_ascii_case(md5) {
                trace!("MD5校验失败 for '{:?}': expected {}, got {}", filepath, md5, actual_md5);
                return Err(AppError::Validation("MD5不匹配".to_string()));
            }
        }
        Ok(())
    }

    async fn download_standard_file(item: &FileInfo, resume_from: u64, context: &DownloadJobContext) -> AppResult<DownloadStatus> {
        let mut current_resume_from = resume_from;
        loop {
            let mut url = Url::parse(&item.url)?;
            let token = context.token.lock().await;
            if !token.is_empty() { url.query_pairs_mut().append_pair("accessToken", &token); }

            let mut request_builder = context.http_client.client.get(url.clone());
            if current_resume_from > 0 {
                debug!("尝试从 {} 字节处续传: {}", current_resume_from, url);
                request_builder = request_builder.header(header::RANGE, format!("bytes={}-", current_resume_from));
            }
            drop(token);
            let res = request_builder.send().await?;

            if res.status() == StatusCode::RANGE_NOT_SATISFIABLE {
                warn!("续传点 {} 无效，将从头开始下载: {}", current_resume_from, &item.filepath.display());
                println!("{} 续传点无效，将从头开始下载: {}", *symbols::WARN, &item.filepath.display());
                current_resume_from = 0;
                if item.filepath.exists() { fs::remove_file(&item.filepath)?; }
                continue;
            }
            if res.status() == StatusCode::UNAUTHORIZED || res.status() == StatusCode::FORBIDDEN { return Err(AppError::TokenInvalid); }
            let res = res.error_for_status()?;
            let mut file = if current_resume_from > 0 {
                OpenOptions::new().write(true).append(true).open(&item.filepath)?
            } else {
                File::create(&item.filepath)?
            };
            let mut stream = res.bytes_stream();
            while let Some(chunk_res) = stream.next().await { file.write_all(&chunk_res?)?; }
            return Ok(if current_resume_from > 0 { DownloadStatus::Resumed } else { DownloadStatus::Success });
        }
    }
}