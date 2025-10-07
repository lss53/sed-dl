// src/downloader/mod.rs

use crate::{
    cli::Cli,
    config::{self, ResourceExtractorType},
    constants,
    error::*,
    extractor::FileInfo,
    ui,
    utils,
    DownloadJobContext,
};
use anyhow::anyhow;
use colored::*;
use futures::{stream, StreamExt};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
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

mod m3u8;
use self::m3u8::M3u8Downloader;

// --- 类型定义 ---
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DownloadStatus {
    Success,
    Skipped,
    Resumed,
    Md5Failed,
    SizeFailed,
    HttpError,
    NetworkError,
    ConnectionError,
    TimeoutError,
    TokenError,
    IoError,
    MergeError,
    KeyError,
    UnexpectedError,
}

#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub filename: String,
    pub status: DownloadStatus,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadAction {
    Skip,
    Resume,
    DownloadNew,
}

pub struct TokenRetryResult {
    pub remaining_tasks: Option<Vec<FileInfo>>,
    pub should_abort: bool,
}

// --- 下载统计与报告 ---

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
        self.stats.lock().unwrap().skipped += 1;
        self.skipped_downloads
            .lock()
            .unwrap()
            .push((filename.to_string(), reason.to_string()));
    }

    pub fn record_failure(&self, filename: &str, status: DownloadStatus) {
        self.stats.lock().unwrap().failed += 1;
        let (_, _, msg) = get_status_display_info(status);
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

        if !skipped.is_empty() || !failed.is_empty() {
            ui::print_sub_header("下载详情报告");
            if !skipped.is_empty() {
                println!("\n{} 跳过的文件 ({}个):", "[i]".cyan(), stats.skipped);
                print_grouped_report(&skipped);
            }
            if !failed.is_empty() {
                println!("\n{} 失败的文件 ({}个):", "[X]".red(), stats.failed);
                print_grouped_report(&failed);
            }
        }

        ui::print_sub_header("任务总结");
        if stats.success == 0 && stats.failed == 0 && stats.skipped > 0 {
            println!(
                "{} 所有 {} 个文件均已存在且有效，无需操作。",
                "[OK]".green(),
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

        fn print_grouped_report(items: &[(String, String)]) {
            let mut grouped: HashMap<&String, Vec<&String>> = HashMap::new();
            for (filename, reason) in items {
                grouped.entry(reason).or_default().push(filename);
            }
            let mut sorted_reasons: Vec<_> = grouped.keys().collect();
            sorted_reasons.sort();

            for reason in sorted_reasons {
                println!("  - 原因: {}", reason);
                let mut filenames = grouped.get(reason).unwrap().clone();
                filenames.sort();
                for filename in filenames {
                    println!("    - {}", filename);
                }
            }
        }
    }
}

pub fn get_status_display_info(
    status: DownloadStatus,
) -> (ColoredString, fn(ColoredString) -> ColoredString, &'static str) {
    match status {
        DownloadStatus::Success => ("[OK]".green(), |s| s.green(), "下载并校验成功"),
        DownloadStatus::Resumed => ("[OK]".green(), |s| s.green(), "续传成功，文件有效"),
        DownloadStatus::Skipped => ("[i]".cyan(), |s| s.cyan(), "文件已存在，跳过"),
        DownloadStatus::Md5Failed => ("[X]".red(), |s| s.red(), "校验失败 (MD5不匹配)"),
        DownloadStatus::SizeFailed => ("[X]".red(), |s| s.red(), "校验失败 (大小不匹配)"),
        DownloadStatus::HttpError => ("[X]".red(), |s| s.red(), "服务器返回错误"),
        DownloadStatus::NetworkError => ("[X]".red(), |s| s.red(), "网络请求失败"),
        DownloadStatus::ConnectionError => ("[X]".red(), |s| s.red(), "无法建立连接"),
        DownloadStatus::TimeoutError => ("[!]".yellow(), |s| s.yellow(), "网络连接超时"),
        DownloadStatus::MergeError => ("[X]".red(), |s| s.red(), "视频分片合并失败"),
        DownloadStatus::KeyError => ("[X]".red(), |s| s.red(), "视频解密密钥获取失败"),
        DownloadStatus::TokenError => ("[X]".red(), |s| s.red(), "认证失败 (Token无效)"),
        DownloadStatus::IoError => ("[X]".red(), |s| s.red(), "本地文件读写错误"),
        DownloadStatus::UnexpectedError => ("[X]".red(), |s| s.red(), "发生未预期的程序错误"),
    }
}

// --- 下载器核心 ---

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
        let (extractor, resource_id) = self.get_extractor_info(url)?;
        self.prepare_and_run(extractor, &resource_id).await
    }

    pub async fn run_with_id(&self, resource_id: &str) -> AppResult<bool> {
        let r#type = self.context.args.r#type.as_ref().unwrap();
        let api_conf = self.context.config.api_endpoints.get(r#type).unwrap();
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
        println!(
            "\n{} 文件将保存到目录: \"{}\"",
            "[i]".cyan(),
            absolute_path.display()
        );

        let mut all_file_items = extractor
            .extract_file_info(resource_id, &self.context)
            .await?;
        if all_file_items.is_empty() {
            println!("\n{} 未能提取到任何可下载的文件信息。", "[i]".cyan());
            return Ok(true);
        }

        for item in &mut all_file_items {
            item.filepath = utils::secure_join_path(&base_output_dir, &item.filepath)?;
        }

        let indices = if all_file_items.len() == 1 {
            let single_item = &all_file_items[0];
            let filename = single_item
                .filepath
                .file_name()
                .unwrap()
                .to_string_lossy();
            println!(
                "\n{} 发现单个文件: '{}'，将直接开始下载。",
                "[i]".cyan(),
                filename
            );
            vec![0]
        } else {
            self.display_files_and_prompt_selection(&all_file_items)?
        };

        if indices.is_empty() {
            println!("\n{} 未选择任何文件，任务结束。", "[i]".cyan());
            return Ok(true);
        }

        let mut tasks_to_run: Vec<FileInfo> =
            indices.into_iter().map(|i| all_file_items[i].clone()).collect();
        tasks_to_run.sort_by_key(|item| item.ti_size.unwrap_or(0));

        let mut tasks_to_attempt = tasks_to_run.clone();
        
        self.context.manager.start_batch(tasks_to_attempt.len());

        loop {
            match self.execute_download_tasks(&tasks_to_attempt).await {
                Ok(_) => break,
                Err(AppError::TokenInvalid) => {
                    if self.context.non_interactive {
                        return Err(AppError::TokenInvalid);
                    }

                    let retry_result = self.handle_token_failure_and_retry(&tasks_to_run).await?;
                    if retry_result.should_abort {
                        return Ok(false);
                    }
                    if let Some(remaining) = retry_result.remaining_tasks {
                        tasks_to_attempt = remaining;
                    } else {
                        break;
                    }
                }
                Err(e) => return Err(e),
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
        for (path_key, api_conf) in &self.context.config.api_endpoints {
            if url.path().contains(path_key) {
                if let Some(resource_id) =
                    url.query_pairs().find(|(k, _)| k == &api_conf.id_param)
                {
                    let id = resource_id.1.to_string();
                    if utils::is_resource_id(&id) {
                        return Ok((self.create_extractor(api_conf), id));
                    }
                }
            }
        }
        Err(AppError::Other(anyhow!(
            "无法识别的URL格式或不支持的资源类型。"
        )))
    }

    fn create_extractor(
        &self,
        api_conf: &crate::config::ApiEndpointConfig,
    ) -> Box<dyn crate::extractor::ResourceExtractor> {
        match api_conf.extractor {
            ResourceExtractorType::Textbook => Box::new(
                crate::extractor::textbook::TextbookExtractor::new(
                    self.context.http_client.clone(),
                    self.context.config.clone(),
                ),
            ),
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
                "Ctrl+C".yellow()
            );
            match ui::prompt(&prompt_msg, Some("1")) {
                Ok(choice) if choice == "1" => {
                    match ui::prompt_hidden("请输入新 Token (输入不可见，完成后按回车)") {
                        Ok(new_token) if !new_token.is_empty() => {
                            *self.context.token.lock().await = new_token.clone();
                            if ui::confirm("是否保存此新 Token 以便后续使用?", false) {
                                config::save_token(&new_token);
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
                    return Ok(TokenRetryResult {
                        remaining_tasks: None,
                        should_abort: true,
                    })
                }
            }
        }

        println!("\n{} Token 已更新。正在检查剩余任务...", "[i]".cyan());
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
            println!("{} 所有任务均已完成，无需重试。", "[OK]".green());
            return Ok(TokenRetryResult {
                remaining_tasks: None,
                should_abort: false,
            });
        }

        self.context
            .manager
            .reset_token_failures(&remaining_filenames);
        println!(
            "{} 准备重试剩余的 {} 个任务...",
            "[i]".cyan(),
            remaining_tasks.len()
        );
        Ok(TokenRetryResult {
            remaining_tasks: Some(remaining_tasks),
            should_abort: false,
        })
    }

    fn display_files_and_prompt_selection(
        &self,
        items: &[FileInfo],
    ) -> AppResult<Vec<usize>> {
        let options: Vec<String> = items
            .iter()
            .map(|item| {
                let date_str = item
                    .date
                    .map_or("[ 日期未知 ]".to_string(), |d| format!("[{}]", d.format("%Y-%m-%d")));
                let filename_str = utils::truncate_text(
                    &item.filepath.file_name().unwrap().to_string_lossy(),
                    constants::FILENAME_TRUNCATE_LENGTH,
                );
                format!("{} {}", date_str, filename_str)
            })
            .collect();

        let default_sel = self.context.args.select.as_deref().unwrap_or("all");
        let user_input = if self.context.non_interactive {
            default_sel.to_string()
        } else {
            ui::selection_menu(
                &options,
                "文件下载列表",
                "支持格式: 1, 3, 2-4, all",
                default_sel,
            )
        };

        Ok(utils::parse_selection_indices(&user_input, items.len()))
    }

    async fn execute_download_tasks(&self, tasks: &[FileInfo]) -> AppResult<()> {
        let max_workers = min(self.context.config.max_workers, tasks.len());
        if max_workers == 0 {
            return Ok(());
        }

        println!(
            "\n{} 发现 {} 个待下载文件，启动 {} 个并发线程...",
            "[i]".cyan(),
            tasks.len(),
            max_workers
        );

        let task_id_counter = Arc::new(AtomicU64::new(0));
        let active_tasks = Arc::new(Mutex::new(BTreeMap::<u64, String>::new()));
        let main_pbar_style = ProgressStyle::with_template(
            "{prefix:7.bold.cyan} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos:>3}/{len:3} ({percent:>3}%) [ETA: {eta}]"
            )
            .expect("进度条模板无效")
            .progress_chars("#>-");
        let main_pbar = self.m_progress.add(ProgressBar::new(tasks.len() as u64));
        main_pbar.set_style(main_pbar_style.clone());
        main_pbar.set_prefix("总进度");
        main_pbar.enable_steady_tick(Duration::from_millis(100));

        let error_sender = Arc::new(tokio::sync::Mutex::new(None::<AppError>));
        let tasks_stream = stream::iter(tasks.to_owned());

        let update_msg = |pbar: &ProgressBar, tasks: &Mutex<BTreeMap<u64, String>>| {
            let guard = tasks.lock().unwrap();
            let task_names: Vec<String> = guard.values().cloned().collect();
            pbar.set_message(task_names.join(", "));
        };
        update_msg(&main_pbar, &active_tasks);

        tasks_stream
            .for_each_concurrent(max_workers, |task| {
                let context = self.context.clone();
                let main_pbar = main_pbar.clone();
                let m_progress = self.m_progress.clone();
                let error_sender = error_sender.clone();
                let task_id_counter = task_id_counter.clone();
                let active_tasks = active_tasks.clone();

                async move {
                    if error_sender.lock().await.is_some() {
                        return;
                    }

                    let task_id = task_id_counter.fetch_add(1, Ordering::Relaxed);
                    let task_name =
                        task.filepath.file_name().unwrap().to_string_lossy().to_string();

                    {
                        active_tasks
                            .lock()
                            .unwrap()
                            .insert(task_id, utils::truncate_text(&task_name, 30));
                        update_msg(&main_pbar, &active_tasks);
                    }

                    match Self::process_single_task(task.clone(), context.clone()).await {
                        Ok(result) => {
                            match result.status {
                                DownloadStatus::Success | DownloadStatus::Resumed => {
                                    context.manager.record_success()
                                }
                                DownloadStatus::Skipped => context.manager.record_skip(
                                    &result.filename,
                                    result.message.as_deref().unwrap_or("文件已存在"),
                                ),
                                _ => context.manager.record_failure(&result.filename, result.status),
                            }

                            if result.status != DownloadStatus::Skipped {
                                let (symbol, _, default_msg) =
                                    get_status_display_info(result.status);
                                if let Some(err_msg) = result.message {
                                    m_progress
                                        .println(format!(
                                            "\n{} 任务 '{}' 失败: {} (详情: {})",
                                            symbol, task_name, default_msg, err_msg
                                        ))
                                        .unwrap();
                                } else {
                                    m_progress
                                        .println(format!("{} {}", symbol, task_name))
                                        .unwrap();
                                }
                            }
                        }
                        Err(e) => {
                            // 唯一可能到达这里的 Err 是 AppError::TokenInvalid
                            let mut error_lock = error_sender.lock().await;
                            if error_lock.is_none() {
                                context.manager.record_failure(
                                    &task.filepath.to_string_lossy(),
                                    DownloadStatus::TokenError,
                                );
                                *error_lock = Some(e);
                            }
                        }
                    }

                    main_pbar.inc(1);

                    {
                        active_tasks.lock().unwrap().remove(&task_id);
                        update_msg(&main_pbar, &active_tasks);
                    }
                }
            })
            .await;

        main_pbar.finish_and_clear();

        if let Some(err) = error_sender.lock().await.take() {
            return Err(err);
        }

        Ok(())
    }

    async fn process_single_task(
        item: FileInfo,
        context: DownloadJobContext,
    ) -> AppResult<DownloadResult> {
        let attempt_result: AppResult<DownloadResult> = async {
            if let Some(parent) = item.filepath.parent() {
                fs::create_dir_all(parent)?;
            }

            let (action, resume_bytes, reason) =
                Self::prepare_download_action(&item, &context.args)?;
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

            let is_m3u8 = item.url.ends_with(".m3u8");
            let download_status = if is_m3u8 {
                M3u8Downloader::new(context.clone()).download(&item).await?
            } else {
                Self::download_standard_file(&item, resume_bytes, &context).await?
            };

            let final_status =
                if download_status == DownloadStatus::Success || download_status == DownloadStatus::Resumed {
                    Self::finalize_and_validate(&item, is_m3u8)?
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
                let status = match &e {
                    AppError::Network(err)
                    | AppError::NetworkMiddleware(reqwest_middleware::Error::Reqwest(err)) => {
                        if err.is_timeout() {
                            DownloadStatus::TimeoutError
                        } else if err.is_connect() {
                            DownloadStatus::ConnectionError
                        } else if err.is_status() {
                            DownloadStatus::HttpError
                        } else {
                            DownloadStatus::NetworkError
                        }
                    }
                    AppError::NetworkMiddleware(_) => DownloadStatus::NetworkError,
                    AppError::Io(_) => DownloadStatus::IoError,
                    AppError::M3u8Parse(_) | AppError::Merge(_) => DownloadStatus::MergeError,
                    AppError::Security(_) => DownloadStatus::KeyError,
                    AppError::Validation(msg) => {
                        if msg.contains("MD5") {
                            DownloadStatus::Md5Failed
                        } else {
                            DownloadStatus::SizeFailed
                        }
                    }
                    _ => DownloadStatus::UnexpectedError,
                };

                Ok(DownloadResult {
                    filename: item
                        .filepath
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                    status,
                    message: Some(e.to_string()),
                })
            }
        }
    }

    fn prepare_download_action(
        item: &FileInfo,
        args: &Cli,
    ) -> AppResult<(DownloadAction, u64, String)> {
        if !item.filepath.exists() {
            return Ok((DownloadAction::DownloadNew, 0, "".to_string()));
        }
        if args.force_redownload {
            return Ok((DownloadAction::DownloadNew, 0, "".to_string()));
        }
        if item.url.ends_with(".m3u8") {
            return Ok((
                DownloadAction::Skip,
                0,
                "视频已存在 (使用 -f 强制重下)".to_string(),
            ));
        }

        match Self::validate_file(&item.filepath, item.ti_md5.as_deref(), item.ti_size) {
            Ok(_) => Ok((
                DownloadAction::Skip,
                0,
                "文件已存在且校验通过".to_string(),
            )),
            Err(_) => {
                let actual_size = item.filepath.metadata()?.len();
                if let Some(expected_size) = item.ti_size {
                    if actual_size > 0 && actual_size < expected_size {
                        return Ok((DownloadAction::Resume, actual_size, "".to_string()));
                    }
                }
                println!(
                    "{} {} - 文件已存在但校验失败，将重新下载。",
                    "[!]".yellow(),
                    item.filepath.display()
                );
                Ok((DownloadAction::DownloadNew, 0, "".to_string()))
            }
        }
    }

    fn finalize_and_validate(item: &FileInfo, is_m3u8: bool) -> AppResult<DownloadStatus> {
        if !item.filepath.exists() || item.filepath.metadata()?.len() == 0 {
            return Ok(DownloadStatus::SizeFailed);
        }
        if is_m3u8 {
            return Ok(DownloadStatus::Success);
        }

        match Self::validate_file(&item.filepath, item.ti_md5.as_deref(), item.ti_size) {
            Ok(_) => Ok(DownloadStatus::Success),
            Err(e @ AppError::Validation(_)) => Err(e),
            Err(e) => Err(e),
        }
    }

    fn validate_file(
        filepath: &Path,
        expected_md5: Option<&str>,
        expected_size: Option<u64>,
    ) -> AppResult<()> {
        if let Some(size) = expected_size {
            if filepath.metadata()?.len() != size {
                return Err(AppError::Validation("大小不匹配".to_string()));
            }
        }
        if let Some(md5) = expected_md5 {
            let actual_md5 = utils::calculate_file_md5(filepath)?;
            if !actual_md5.eq_ignore_ascii_case(md5) {
                return Err(AppError::Validation("MD5不匹配".to_string()));
            }
        }
        Ok(())
    }

    async fn download_standard_file(
        item: &FileInfo,
        resume_from: u64,
        context: &DownloadJobContext,
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
                request_builder = request_builder
                    .header(header::RANGE, format!("bytes={}-", current_resume_from));
            }

            drop(token);

            let res = request_builder.send().await?;

            if res.status() == StatusCode::RANGE_NOT_SATISFIABLE {
                println!(
                    "{} 续传点无效，将从头开始下载: {}",
                    "[!]".yellow(),
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
                    .write(true)
                    .append(true)
                    .open(&item.filepath)?
            } else {
                File::create(&item.filepath)?
            };

            let mut stream = res.bytes_stream();

            while let Some(chunk_res) = stream.next().await {
                let chunk = chunk_res?;
                file.write_all(&chunk)?;
            }

            return Ok(if current_resume_from > 0 {
                DownloadStatus::Resumed
            } else {
                DownloadStatus::Success
            });
        }
    }
}