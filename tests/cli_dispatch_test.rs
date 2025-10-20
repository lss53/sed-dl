// tests/cli_dispatch_test.rs

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

// 辅助函数，避免重复
fn main_command() -> Command {
    Command::cargo_bin(env!("CARGO_PKG_NAME")).unwrap()
}

// --- 测试基本 CLI 行为 ---

#[test]
fn test_help_flag() {
    let mut cmd = main_command();
    cmd.arg("--help");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("显示此帮助信息并退出"));
}

#[test]
fn test_token_help_command() {
    let mut cmd = main_command();
    cmd.arg("--token-help");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("登录平台: 使用 Chrome / Edge / Firefox"));
}

#[test]
fn test_missing_mode_shows_help() {
    let mut cmd = main_command();
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("Usage: sed-dl <MODE> [OPTIONS]"));
}

#[test]
fn test_id_mode_requires_type() {
    let mut cmd = main_command();
    cmd.arg("--id").arg("some-uuid");
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("the following required arguments were not provided:\n  --type <TYPE>"));
}


// --- 测试核心分发逻辑 ---

#[test]
fn test_single_url_mode_dispatch() {
    let mut cmd = main_command();
    let fake_url = "http://127.0.0.1:9999/some/path/tchMaterial/some/resource?contentId=00000000-0000-0000-0000-000000000000";
    cmd.arg("--url").arg(fake_url);
    cmd.assert()
        .failure()
        // --- 修正断言 ---
        // 验证程序是否能正确处理 404 Not Found 并给出友好提示
        .stderr(predicate::str::contains("资源不存在 (链接或ID错误)"));
}

#[test]
fn test_single_id_mode_dispatch() {
    let mut cmd = main_command();
    cmd.arg("--id")
        .arg("00000000-0000-0000-0000-000000000000")
        .arg("--type")
        .arg("tchMaterial");
    cmd.assert()
        .failure()
        // --- 修正断言 ---
        // 验证程序是否能正确处理 404 Not Found 并给出友好提示
        .stderr(predicate::str::contains("资源不存在 (链接或ID错误)"));
}

#[test]
fn test_batch_mode_dispatch() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("links.txt");
    let mut file = File::create(&file_path).unwrap();
    let fake_url = "http://127.0.0.1:9999/some/path/tchMaterial/some/resource?contentId=00000000-0000-0000-0000-000000000000";
    writeln!(file, "{}", fake_url).unwrap();

    let mut cmd = main_command();
    cmd.arg("-b")
        .arg(&file_path)
        .arg("--type")
        .arg("tchMaterial"); 
    
    cmd.assert()
        .failure()
        .stderr(
            predicate::str::contains("个任务元数据解析失败")
        );
}