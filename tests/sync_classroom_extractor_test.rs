// tests/sync_classroom_extractor_test.rs

use sed_dl::{
    cli::Cli,
    client::RobustClient,
    config::AppConfig,
    downloader::DownloadManager,
    error::AppResult,
    extractor::{sync_classroom::SyncClassroomExtractor, ResourceExtractor},
    DownloadJobContext,
};
use clap::Parser;
use std::{
    fs,
    sync::{atomic::AtomicBool, Arc},
};
use tokio::sync::Mutex as TokioMutex;

#[tokio::test]
async fn test_sync_classroom_extractor_parses_correctly() -> AppResult<()> {
    // --- 1. Arrange (准备阶段) ---

    let mut server = mockito::Server::new_async().await;
    let server_url = server.url();

    let mock_body = fs::read_to_string("tests/fixtures/sync_classroom_response.json")
        .expect("无法读取模拟响应文件");

    let resource_id = "fake-sync-classroom-id";
    let mock_endpoint = server
        .mock(
            "GET",
            format!("/zxx/ndrv2/national_lesson/resources/details/{}.json", resource_id).as_str(),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&mock_body)
        .create_async()
        .await;

    // --- 2. 创建测试所需的上下文 ---
    let mut config = AppConfig::default();
    config.url_templates.insert(
        "COURSE_SYNC".to_string(),
        format!(
            "{}/zxx/ndrv2/national_lesson/resources/details/{{resource_id}}.json",
            server_url
        ),
    );
    let config = Arc::new(config);

    let args = Arc::new(Cli::parse_from(["sed-dl", "--id", resource_id, "--type", "syncClassroom/classActivity"]));

    let context = DownloadJobContext {
        manager: DownloadManager::new(),
        token: Arc::new(TokioMutex::new("fake-token".to_string())),
        config: config.clone(),
        http_client: Arc::new(RobustClient::new(config.clone())?),
        args,
        non_interactive: true,
        cancellation_token: Arc::new(AtomicBool::new(false)),
    };

    // --- 3. Act (执行阶段) ---
    let extractor_template = context.config.url_templates.get("COURSE_SYNC").unwrap().clone();
    let extractor = SyncClassroomExtractor::new(
        context.http_client.clone(),
        context.config.clone(),
        extractor_template,
    );

    let file_infos = extractor.extract_file_info(resource_id, &context).await?;

    // --- 4. Assert (断言阶段) ---

    mock_endpoint.assert_async().await;

    // (视频(3) + 课件(1) + 任务单(1) + 练习(1)) * 2个课时 = 12个文件
    assert_eq!(file_infos.len(), 12, "应该提取出所有 12 个资源（包括各分辨率视频）");

    // --- 验证第一课时的文件 ---
    let lesson1_files: Vec<_> = file_infos
        .iter()
        .filter(|f| f.filepath.to_string_lossy().contains("第一课时"))
        .collect();
    assert_eq!(lesson1_files.len(), 6, "第一课时应该有 6 个文件 (3视频 + 3文档)");

    // 随机抽查一个第一课时的文件，验证文件名和教师
    let task_sheet_1 = lesson1_files
        .iter()
        .find(|f| f.filepath.to_string_lossy().contains("学习任务单"))
        .expect("没有找到第一课时的学习任务单");
    let path_str_1 = task_sheet_1.filepath.to_string_lossy();
    assert!(path_str_1.starts_with("基因指导蛋白质的合成[第一课时] - 学习任务单"));
    assert!(path_str_1.contains("[姚亭秀].pdf"));

    // --- 验证第二课时的文件 ---
    let lesson2_files: Vec<_> = file_infos
        .iter()
        .filter(|f| f.filepath.to_string_lossy().contains("第二课时"))
        .collect();
    assert_eq!(lesson2_files.len(), 6, "第二课时应该有 6 个文件 (3视频 + 3文档)");

    // 随机抽查一个第二课时的文件，验证文件名和教师
    let video_2 = lesson2_files
        .iter()
        .find(|f| f.filepath.to_string_lossy().contains("视频课程"))
        .expect("没有找到第二课时的视频课程");
    let path_str_2 = video_2.filepath.to_string_lossy();
    assert!(path_str_2.starts_with("基因指导蛋白质的合成[第二课时] - 视频课程"));
    assert!(path_str_2.contains("[刘媛媛].ts")); // 假设我们只关心ts文件

    Ok(())
}