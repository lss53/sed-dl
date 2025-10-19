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

    // fixture 里有4个资源 * 2个课时 = 8个资源。其中2个视频各有3个流。
    // 所以总文件数 = (3视频流 + 1课件 + 1任务单 + 1练习) * 2课时 = 12 个文件
    assert_eq!(file_infos.len(), 12, "应该提取出所有 12 个资源（包括各分辨率视频）");

    // --- 验证第一课时的文件 ---
    let lesson1_files: Vec<_> = file_infos
        .iter()
        .filter(|f| f.filepath.to_string_lossy().contains("第一课时"))
        .collect();
    // 3个视频流 + 课件 + 任务单 + 练习 = 6个文件
    assert_eq!(lesson1_files.len(), 6, "第一课时应该有 6 个文件 (3视频 + 3文档)");

    // 随机抽查一个第一课时的文件，验证文件名和教师
    let task_sheet_1 = lesson1_files
        .iter()
        .find(|f| f.filepath.to_string_lossy().contains("学习任务单"))
        .expect("没有找到第一课时的学习任务单");
    
    // 只获取文件名进行断言
    let filename_1 = task_sheet_1.filepath.file_name()
        .expect("文件路径没有文件名")
        .to_str().unwrap();
        
    // 断言文件名，而不是整个路径
    assert_eq!(filename_1, "基因指导蛋白质的合成[第一课时] - 学习任务单 - [姚亭秀].pdf");

    // --- 验证第二课时的文件 ---
    let lesson2_files: Vec<_> = file_infos
        .iter()
        .filter(|f| f.filepath.to_string_lossy().contains("第二课时"))
        .collect();
    assert_eq!(lesson2_files.len(), 6, "第二课时应该有 6 个文件 (3视频 + 3文档)");

    // 随机抽查一个第二课时的文件，验证文件名和教师
    let video_2_720p = lesson2_files
        .iter()
        .find(|f| f.filepath.to_string_lossy().contains("视频课程") && f.filepath.to_string_lossy().contains("[720]"))
        .expect("没有找到第二课时的720p视频课程");
    
    // 只获取文件名进行断言
    let filename_2 = video_2_720p.filepath.file_name()
        .expect("文件路径没有文件名")
        .to_str().unwrap();
        
    // 断言文件名，而不是整个路径
    assert_eq!(filename_2, "基因指导蛋白质的合成[第二课时] - 视频课程 [720] - [刘媛媛].ts");

    Ok(())
}