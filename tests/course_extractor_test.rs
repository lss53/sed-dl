// tests/course_extractor_test.rs

use clap::Parser;
use sed_dl::{
    DownloadJobContext,
    cli::Cli,
    client::RobustClient,
    config::AppConfig,
    downloader::DownloadManager,
    downloader::negotiator::ItemNegotiator,
    error::AppResult,
    extractor::{ResourceExtractor, course::CourseExtractor},
};
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, atomic::AtomicBool};
use tokio::sync::Mutex as TokioMutex;

#[tokio::test]
async fn test_course_extractor_parses_mock_response() -> AppResult<()> {
    // --- 1. Arrange (准备阶段) ---

    // 启动一个模拟HTTP服务器
    let mut server = mockito::Server::new_async().await;
    let server_url = server.url();

    // 从文件中读取模拟的API响应
    let mock_body =
        fs::read_to_string("tests/fixtures/course_response.json").expect("无法读取模拟响应文件");

    // 定义当我们的代码请求特定URL时，模拟服务器应该如何回应
    let resource_id = "fake-course-id";
    let mock_endpoint = server
        .mock(
            "GET",
            format!("/zxx/ndrv2/resources/{}.json", resource_id).as_str(),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&mock_body)
        .create_async()
        .await;

    // --- 2. 创建一个为测试定制的 AppConfig ---
    let mut config = AppConfig::default();

    let mut new_url_templates = HashMap::new();
    new_url_templates.insert(
        "COURSE_QUALITY".to_string(),
        format!("{}/zxx/ndrv2/resources/{{resource_id}}.json", server_url),
    );
    config.url_templates = new_url_templates;

    config.server_prefixes = vec!["unused".to_string()];

    let config = Arc::new(config);

    // --- 3. 模拟命令行参数来创建 Cli 实例 ---
    let args = Arc::new(Cli::parse_from([
        "sed-dl", // 程序名
        "--url",
        "unused_url", // 满足 mode group 的要求
        "--video-quality",
        "best", // 这是测试需要的关键参数
        "--select",
        "all", // 确保是非交互模式行为
    ]));

    // --- 3. 创建测试所需的 DownloadJobContext ---
    let context = DownloadJobContext {
        manager: DownloadManager::new(),
        token: Arc::new(TokioMutex::new("fake-token".to_string())),
        config: config.clone(), // 使用我们修改过的 config
        http_client: Arc::new(RobustClient::new(config.clone())?),
        args: args.clone(),
        non_interactive: !args.interactive && !args.prompt_each,
        cancellation_token: Arc::new(AtomicBool::new(false)),
    };

    // --- 4. Act (执行阶段) ---
    // 这里的 url_template 必须与 extractor 内部获取的一致。
    // Extractor 会从 context.config 中读取我们刚刚覆盖的模板。
    let extractor_template = context
        .config
        .url_templates
        .get("COURSE_QUALITY")
        .unwrap()
        .clone();
    let extractor = CourseExtractor::new(
        context.http_client.clone(),
        context.config.clone(),
        extractor_template,
    );

    let file_infos_raw = extractor.extract_file_info(resource_id, &context).await?;

    // --- [核心修复] ---
    // 手动模拟在非交互模式下的 ItemNegotiator 过滤行为。
    // 这使得我们的测试更真实地反映了程序的完整流程。
    let negotiator = ItemNegotiator::new(&context);
    let file_infos = negotiator.pre_filter_items(file_infos_raw)?;

    // --- 5. Assert (断言阶段) ---

    // 验证模拟服务器确实被调用了一次
    mock_endpoint.assert_async().await;

    // 检查是否提取出2个文件 (1个视频, 1个PDF)
    assert_eq!(file_infos.len(), 2, "应该提取出两个文件信息");

    // 查找并验证视频文件的信息
    let video_info = file_infos
        .iter()
        .find(|f| f.url.contains("video"))
        .expect("没有找到视频文件");

    let video_path = video_info.filepath.to_string_lossy();
    assert!(video_path.contains("小学"), "路径应包含学段");
    assert!(video_path.contains("一年级"), "路径应包含年级");
    assert!(video_path.contains("语文"), "路径应包含学科");
    assert!(video_path.contains("示例课程标题"), "路径应包含课程标题");
    assert!(
        video_path.contains("示例课程标题 - 课堂录像 [720] - [张老师].ts"),
        "视频文件名格式不正确"
    );
    assert_eq!(video_info.ti_size, Some(12345678), "视频大小解析错误"); // 验证是否从 custom_properties 获取了 total_size

    // 查找并验证PDF文件的信息
    let pdf_info = file_infos
        .iter()
        .find(|f| f.url.contains("document"))
        .expect("没有找到PDF文件");

    let pdf_path = pdf_info.filepath.to_string_lossy();
    assert!(
        pdf_path.contains("示例课程标题 - 教学课件 - [张老师].pdf"),
        "PDF文件名格式不正确"
    );
    assert_eq!(pdf_info.ti_size, Some(102400), "PDF大小解析错误");
    assert_eq!(
        pdf_info.ti_md5,
        Some("d41d8cd98f00b204e9800998ecf8427e".to_string()),
        "PDF MD5解析错误"
    );

    Ok(())
}
