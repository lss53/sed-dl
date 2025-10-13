// src/utils.rs

use crate::{constants, error::*};
use anyhow::Context;
use md5::{Digest, Md5};
use std::sync::LazyLock;
use regex::Regex;
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::File,
    io::{BufReader, Read},
    path::{Component, Path, PathBuf},
};

pub static UUID_PATTERN:LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-f0-9]{8}-([a-f0-9]{4}-){3}[a-f0-9]{12}$").unwrap());
static ILLEGAL_CHARS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[\\/*?:"<>|]"#).unwrap());
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

pub fn is_resource_id(text: &str) -> bool {
    UUID_PATTERN.is_match(text)
}

pub fn sanitize_filename(name: &str) -> String {
    let original_name = name.trim();
    if original_name.is_empty() { return "unknown".to_string(); }

    let stem = Path::new(original_name)
        .file_stem()
        .unwrap_or_else(|| OsStr::new(original_name))
        .to_string_lossy()
        .to_uppercase();
    let windows_reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    let mut name = if windows_reserved.contains(&stem.as_ref()) {
        format!("_{}", original_name)
    } else {
        original_name.to_string()
    };

    name = ILLEGAL_CHARS_RE.replace_all(&name, " ").into_owned();
    name = WHITESPACE_RE.replace_all(&name, " ").trim().to_string();
    name = name.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if name.is_empty() { return "unnamed".to_string(); }

    if name.as_bytes().len() > constants::MAX_FILENAME_BYTES {
        if let (Some(stem_part), Some(ext)) = (Path::new(&name).file_stem(), Path::new(&name).extension()) {
            let stem_part_str = stem_part.to_string_lossy();
            let ext_str = format!(".{}", ext.to_string_lossy());
            let max_stem_bytes = constants::MAX_FILENAME_BYTES.saturating_sub(ext_str.as_bytes().len());
            let truncated_stem = safe_truncate_utf8(&stem_part_str, max_stem_bytes);
            name = format!("{}{}", truncated_stem, ext_str);
        } else {
            name = safe_truncate_utf8(&name, constants::MAX_FILENAME_BYTES).to_string();
        }
    }
    name
}

fn safe_truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes { return s; }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) { i -= 1; }
    &s[..i]
}

pub fn truncate_text(text: &str, max_width: usize) -> String {
    let mut width = 0;
    let mut end_pos = 0;
    for (i, c) in text.char_indices() {
        width += if c.is_ascii() { 1 } else { 2 };
        if width > max_width.saturating_sub(3) {
            end_pos = i;
            break;
        }
    }
    if end_pos == 0 { text.to_string() } else { format!("{}...", &text[..end_pos]) }
}

pub fn parse_selection_indices(selection_str: &str, total_items: usize) -> Vec<usize> {
    if selection_str.to_lowercase() == "all" { return (0..total_items).collect(); }
    let mut indices = BTreeSet::new();
    for part in selection_str.split(',').map(|s| s.trim()) {
        if part.is_empty() { continue; }
        if let Some(range_part) = part.split_once('-') {
            if let (Ok(start), Ok(end)) = (range_part.0.parse::<usize>(), range_part.1.parse::<usize>()) {
                if start == 0 || end == 0 { continue; }
                let (min, max) = (start.min(end), start.max(end));
                for i in min..=max {
                    if i > 0 && i <= total_items { indices.insert(i - 1); }
                }
            }
        } else if let Ok(num) = part.parse::<usize>() {
            if num > 0 && num <= total_items { indices.insert(num - 1); }
        }
    }
    indices.into_iter().collect()
}

pub fn calculate_file_md5(path: &Path) -> AppResult<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Md5::new();
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 { break; }
        hasher.update(&buffer[..bytes_read]);
    }
    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}

pub fn secure_join_path(base_dir: &Path, relative_path: &Path) -> AppResult<PathBuf> {
    let resolved_base = dunce::canonicalize(base_dir).with_context(|| format!("基础目录 '{:?}' 不存在或无法访问", base_dir))?;
    let mut final_path = resolved_base.clone();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => final_path.push(part),
            Component::ParentDir => return Err(AppError::Security("检测到路径遍历 '..' ".to_string())),
            _ => continue,
        }
    }
    if !final_path.starts_with(&resolved_base) {
        return Err(AppError::Security(format!("路径遍历攻击检测: '{:?}'", relative_path)));
    }
    Ok(final_path)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_selection_indices() {
        // 测试基本情况
        assert_eq!(parse_selection_indices("1,3,5", 5), vec![0, 2, 4]);
        
        // 测试范围
        assert_eq!(parse_selection_indices("2-4", 5), vec![1, 2, 3]);

        // 测试 "all" 关键字 (大小写不敏感)
        assert_eq!(parse_selection_indices("all", 3), vec![0, 1, 2]);
        assert_eq!(parse_selection_indices("All", 3), vec![0, 1, 2]);

        // 测试混合、乱序和重复
        assert_eq!(parse_selection_indices("5, 1-2, 1", 5), vec![0, 1, 4]);

        // 测试无效和越界输入
        assert_eq!(parse_selection_indices("1,10,foo,-2", 5), vec![0]);

        // 测试空输入
        assert_eq!(parse_selection_indices("", 5), Vec::<usize>::new());
    }

    #[test]
    fn test_sanitize_filename() {
        // 测试非法字符
        assert_eq!(sanitize_filename("a\\b/c:d*e?f\"g<h>i|j"), "a b c d e f g h i j".to_string());

        // 测试首尾空格和点
        assert_eq!(sanitize_filename(" . my file. "), "my file".to_string());

        // 测试多个连续空格
        assert_eq!(sanitize_filename("a  b   c"), "a b c".to_string());

        // 测试 Windows 保留字 (大小写不敏感)
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt".to_string());
        assert_eq!(sanitize_filename("aux"), "_aux".to_string());

        // 测试空或只有非法字符的输入
        assert_eq!(sanitize_filename(""), "unknown".to_string());
        assert_eq!(sanitize_filename("<>|"), "unnamed".to_string());

        // 测试文件名截断 (确保不破坏UTF-8和扩展名)
        // 假设 MAX_FILENAME_BYTES = 20
        let very_long_name = "这是一个非常长的文件名.txt"; // 46 bytes
        let truncated = sanitize_filename(very_long_name);
        // "这是一个非.txt" -> 12 (4*3) + 1 + 3 = 16 bytes
        assert!(truncated.as_bytes().len() <= constants::MAX_FILENAME_BYTES);
        assert!(truncated.ends_with(".txt"));
    }
}