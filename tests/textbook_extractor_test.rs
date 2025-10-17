// tests/textbook_extractor_test.rs

use sed_dl::{
    cli::Cli,
    client::RobustClient,
    config::AppConfig,
    downloader::DownloadManager,
    error::AppResult,
    extractor::{textbook::TextbookExtractor, ResourceExtractor},
    DownloadJobContext,
};
use clap::Parser;
use std::sync::{atomic::AtomicBool, Arc};
use tokio::sync::Mutex as TokioMutex;

#[tokio::test]
async fn test_textbook_extractor_parses_pdf_and_audio() -> AppResult<()> {
    // --- 1. Arrange (准备阶段) ---

    let mut server = mockito::Server::new_async().await;
    let server_url = server.url();

    let details_body =
        std::fs::read_to_string("tests/fixtures/textbook_details_response.json").unwrap();
    let audio_body =
        std::fs::read_to_string("tests/fixtures/textbook_audio_response.json").unwrap();

    let resource_id = "fake-textbook-id";

    // 模拟第一个 API 端点：教材详情
    let details_mock = server
        .mock(
            "GET",
            format!(
                "/zxx/ndrv2/resources/tch_material/details/{}.json",
                resource_id
            )
            .as_str(),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&details_body)
        .create_async()
        .await;

    // 模拟第二个 API 端点：关联音频
    let audio_mock = server
        .mock(
            "GET",
            format!("/zxx/ndrs/resources/{}/relation_audios.json", resource_id).as_str(),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&audio_body)
        .create_async()
        .await;

    // --- 2. 创建测试所需的上下文 ---
    let mut config = AppConfig::default();
    config.url_templates.insert(
        "TEXTBOOK_DETAILS".to_string(),
        format!(
            "{}/zxx/ndrv2/resources/tch_material/details/{{resource_id}}.json",
            server_url
        ),
    );
    config.url_templates.insert(
        "TEXTBOOK_AUDIO".to_string(),
        format!(
            "{}/zxx/ndrs/resources/{{resource_id}}/relation_audios.json",
            server_url
        ),
    );
    let config = Arc::new(config);

    let args = Arc::new(Cli::parse_from(["sed-dl", "--id", resource_id, "--type", "tchMaterial"]));

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
    let extractor = TextbookExtractor::new(context.http_client.clone(), context.config.clone());
    let file_infos = extractor.extract_file_info(resource_id, &context).await?;

    // --- 4. Assert (断言阶段) ---

    // 验证两个 API 端点都被调用了
    details_mock.assert_async().await;
    audio_mock.assert_async().await;

    // 总共应该有 1 个 PDF + 2 个音频（每个音频有多种格式）
    // lesson1 (mp3, m4a), lesson2 (mp3) -> 3 个音频文件
    assert_eq!(file_infos.len(), 4, "应该提取出 1 个 PDF 和 3 个音频文件");

    // 验证 PDF
    let pdf_info = file_infos.iter().find(|f| f.url.contains("pdf")).expect("没有找到 PDF 文件");
    let pdf_path_str = pdf_info.filepath.to_string_lossy();
    assert!(pdf_path_str.contains("小学\\一年级\\语文")); // 验证分类目录
    assert!(pdf_path_str.ends_with("语文一年级上册.pdf")); // 验证文件名

    // 验证音频
    let audio_files: Vec<_> = file_infos.iter().filter(|f| f.url.contains("mp3") || f.url.contains("m4a")).collect();
    assert_eq!(audio_files.len(), 3, "应该有 3 个音频文件");

    // 抽查第一个音频的 MP3
    let lesson1_mp3 = audio_files.iter().find(|f| f.url.contains("lesson1.mp3")).expect("没有找到第一课的 MP3");
    let lesson1_mp3_path = lesson1_mp3.filepath.to_string_lossy();
    assert!(lesson1_mp3_path.contains("语文一年级上册 - [audio]")); // 验证音频子目录
    assert!(lesson1_mp3_path.ends_with("[1] 第一课 课文朗读.mp3")); // 验证文件名和序号

    // 抽查第一个音频的 M4A
    let lesson1_m4a = audio_files.iter().find(|f| f.url.contains("lesson1.m4a")).expect("没有找到第一课的 M4A");
    assert!(lesson1_m4a.filepath.to_string_lossy().ends_with("[1] 第一课 课文朗读.m4a"));
    
    // 抽查第二个音频的 MP3
    let lesson2_mp3 = audio_files.iter().find(|f| f.url.contains("lesson2.mp3")).expect("没有找到第二课的 MP3");
    assert!(lesson2_mp3.filepath.to_string_lossy().ends_with("[2] 第二课 生字.mp3"));

    Ok(())
}