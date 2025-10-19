// src/error.rs

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("认证失败 (Token 无效或已过期)")]
    TokenInvalid,
    #[error("未提供 Access Token，无法进行下载")]
    TokenMissing,
    #[error("网络请求失败: {0}")]
    Network(#[from] reqwest::Error),
    #[error("网络中间件错误: {0}")]
    NetworkMiddleware(#[from] reqwest_middleware::Error),
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("临时文件持久化失败: {0}")]
    TempFilePersist(#[from] tempfile::PersistError),
    #[error("JSON 解析错误: {0}")]
    Json(#[from] serde_json::Error),
    #[error("无法解析来自 '{url}' 的API响应: {source}")]
    ApiParseFailed {
        url: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("URL 解析错误: {0}")]
    Url(#[from] url::ParseError),
    #[error("Base64 解码错误: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("M3U8 解析错误: {0}")]
    M3u8Parse(String),
    #[error("视频分片合并失败: {0}")]
    Merge(String),
    #[error("文件校验失败: {0}")]
    Validation(String),
    #[error("安全错误: {0}")]
    Security(String),
    #[error("用户中断")]
    UserInterrupt,
    #[error("{0}")] // 只打印内部信息，不加任何前缀
    UserInputError(String),
    #[error("未知错误: {0}")]
    Other(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;
