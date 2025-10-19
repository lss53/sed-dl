// tests/client_retry_test.rs

use sed_dl::client::RobustClient;
use sed_dl::config::AppConfig;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread")]
async fn test_client_handles_429_rate_limiting_with_retry_after() {
    // --- 1. Arrange (准备阶段) ---

    // 启动一个模拟HTTP服务器
    let mut server = mockito::Server::new_async().await;
    let server_url = server.url();

    // 配置模拟服务器的行为:
    // 第一次GET请求 -> 返回 429 Too Many Requests，并附带 "Retry-After: 1" 头
    let mock_429 = server
        .mock("GET", "/test")
        .with_status(429)
        .with_header("Retry-After", "1")
        .with_body("Rate limited!")
        .create_async()
        .await;

    // 第二次GET请求 -> 返回 200 OK
    let mock_200 = server
        .mock("GET", "/test")
        .with_status(200)
        .with_body("Success!")
        .create_async()
        .await;
    
    // --- 2. 创建一个为测试定制的 RobustClient ---
    // 使用默认的 AppConfig，它包含了我们的重试设置
    let config = Arc::new(AppConfig::default()); 
    let client = RobustClient::new(config).expect("Failed to create client");

    // --- 3. Act (执行阶段) ---
    
    let start_time = Instant::now();

    // 发起请求。我们期望客户端内部会自动处理 429 错误并重试
    let response = client
        .get(format!("{}/test", server_url))
        .await
        .expect("Request should eventually succeed");

    let elapsed = start_time.elapsed();

    // --- 4. Assert (断言阶段) ---

    // 验证最终的响应是成功的
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "Success!");

    // 验证两个模拟端点都被调用了恰好一次
    mock_429.assert_async().await;
    mock_200.assert_async().await;

    // 验证总耗时：应该大于 Retry-After 头指定的 1 秒
    // 我们设置一个合理的范围，比如 1.0 到 1.5 秒，以考虑网络和调度延迟
    assert!(
        elapsed >= Duration::from_secs(1),
        "Elapsed time should be at least 1 second due to Retry-After header. Was: {:?}",
        elapsed
    );
    assert!(
        elapsed < Duration::from_secs(2), // 给一个宽裕的上限
        "Elapsed time should be reasonably close to 1 second. Was: {:?}",
        elapsed
    );
    
    println!("Test passed: Rate limiting was handled correctly in {:?}.", elapsed);
}