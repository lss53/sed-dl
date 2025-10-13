// src/downloader/m3u8.rs

use super::DownloadStatus;
use crate::{client::RobustClient, error::*, models::FileInfo, DownloadJobContext};
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyInit, KeyIvInit};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures::{stream, StreamExt};
use log::{debug, error, info, warn};
use md5::{Digest, Md5};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::Path,
    sync::Arc,
    time::Duration,
};
use indicatif::ProgressBar;
use url::Url;
use ecb;

pub(super) type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

pub(super) struct M3u8Downloader {
    context: DownloadJobContext,
}

impl M3u8Downloader {
    pub(super) fn new(context: DownloadJobContext) -> Self {
        Self { context }
    }

    pub(super) async fn download(
        &self, item: &FileInfo,
        pbar: ProgressBar,
        use_byte_progress: bool,
    ) -> AppResult<DownloadStatus> {
        info!("开始下载 M3U8 视频: {}", item.filepath.display());
        let mut url = Url::parse(&item.url)?;
        let token = self.context.token.lock().await;
        if !token.is_empty() {
            url.query_pairs_mut().append_pair("accessToken", &token);
        }
        drop(token);

        let (key, iv, playlist) = self.get_m3u8_key_and_playlist(url.clone()).await?;

        if playlist.segments.is_empty() {
            error!("M3U8文件 '{}' 不含分片", item.url);
            return Err(AppError::M3u8Parse("M3U8文件不含分片".to_string()));
        }
        info!("M3U8 包含 {} 个分片。 解密密钥: {}, IV: {}", playlist.segments.len(), if key.is_some() { "有" } else { "无" }, iv.as_deref().unwrap_or("无"));
        let segment_urls: Vec<String> = playlist.segments.iter().map(|s| s.uri.clone()).collect();

        let decryptor = if let (Some(key), Some(iv_hex)) = (key, iv) {
            let iv_bytes = hex::decode(iv_hex.trim_start_matches("0x"))
                .map_err(|e| AppError::M3u8Parse(format!("无效的IV十六进制值: {}", e)))?;
            Some(
                Aes128CbcDec::new_from_slices(&key, &iv_bytes)
                    .map_err(|e| AppError::Security(format!("AES解密器初始化失败: {}", e)))?,
            )
        } else {
            None
        };

        let temp_dir = tempfile::Builder::new().prefix("m3u8_dl_").tempdir()?;
        debug!("为M3U8下载创建临时目录: {:?}", temp_dir.path());

        self.download_segments_with_retry(
            &url, &segment_urls, temp_dir.path(), decryptor,
            pbar, use_byte_progress
        )
            .await?;

        info!("所有分片下载完成，开始合并...");
        self.merge_ts_segments(temp_dir.path(), segment_urls.len(), &item.filepath)?;
        info!("分片合并完成 -> {}", item.filepath.display());
        Ok(DownloadStatus::Success)
    }

    fn merge_ts_segments(&self, temp_dir: &Path, num_segments: usize, output_path: &std::path::PathBuf) -> AppResult<()> {
        let temp_output_path = output_path.with_extension("tmp");
        let mut writer = BufWriter::new(File::create(&temp_output_path)?);
        for i in 0..num_segments {
            let ts_path = temp_dir.join(format!("{:05}.ts", i));
            if !ts_path.exists() {
                let filename = ts_path.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "未知分片".to_string());
                return Err(AppError::Merge(format!("丢失视频分片: {}", filename)));
            }
            let mut reader = File::open(ts_path)?;
            io::copy(&mut reader, &mut writer)?;
        }
        writer.flush()?;
        fs::rename(temp_output_path, output_path)?;
        Ok(())
    }

    async fn fetch_and_parse_playlist(&self, url: &Url) -> AppResult<m3u8_rs::MediaPlaylist> {
        debug!("获取并解析 M3U8 文件: {}", url);
        let playlist_text = self.context.http_client.get(url.clone()).await?.text().await?;
        
        match m3u8_rs::parse_playlist_res(playlist_text.as_bytes()) {
            Ok(m3u8_rs::Playlist::MediaPlaylist(media)) => Ok(media),
            Ok(_) => Err(AppError::M3u8Parse("预期的M3U8文件不是媒体播放列表".to_string())),
            Err(e) => Err(AppError::M3u8Parse(e.to_string())),
        }
    }

    async fn fetch_and_decrypt_key(&self, base_url: &Url, key_uri: &str) -> AppResult<Vec<u8>> {
        debug!("在M3U8中找到加密信息. Key URI: {}", key_uri);
        let key_url = base_url.join(key_uri)?;

        // 1. 获取 nonce
        let nonce_url = format!("{}/signs", key_url);
        debug!("获取 nonce from: {}", nonce_url);
        let signs_data: Value = self.context.http_client.get(&nonce_url).await?.json().await?;
        let nonce = signs_data.get("nonce").and_then(Value::as_str)
            .ok_or_else(|| AppError::M3u8Parse("密钥服务器响应中未找到 'nonce'".to_string()))?;

        // 2. 计算 sign
        let key_filename = key_url.path_segments().and_then(|s| s.last())
            .ok_or_else(|| AppError::M3u8Parse(format!("无法从密钥URL中提取文件名: {}", key_url)))?;
        let sign_material = format!("{}{}", nonce, key_filename);
        let mut hasher = Md5::new();
        hasher.update(sign_material.as_bytes());
        let result = hasher.finalize();
        let sign = &format!("{:x}", result)[..16];
        debug!("计算得到 sign: {}", sign);

        // 3. 获取加密的 key
        let final_key_url = format!("{}?nonce={}&sign={}", key_url, nonce, sign);
        debug!("获取最终密钥 from: {}", final_key_url);
        let key_data: Value = self.context.http_client.get(&final_key_url).await?.json().await?;
        let encrypted_key_b64 = key_data.get("key").and_then(Value::as_str)
            .ok_or_else(|| AppError::M3u8Parse("密钥服务器响应中未找到加密密钥 'key'".to_string()))?;

        // 4. 解密 key
        let encrypted_key = BASE64.decode(encrypted_key_b64)?;
        type EcbDec = ecb::Decryptor<aes::Aes128>;
        let cipher = EcbDec::new(sign.as_bytes().into());
        let decrypted_key = cipher.decrypt_padded_vec_mut::<Pkcs7>(&encrypted_key)
            .map_err(|e| AppError::Security(format!("AES密钥解密失败: {}", e)))?;
        
        debug!("密钥解密成功");
        Ok(decrypted_key)
    }

    async fn download_segments_with_retry(
        &self,
        base_url: &Url,
        urls: &[String],
        temp_path: &Path,
        decryptor: Option<Aes128CbcDec>,
        pbar: ProgressBar,
        use_byte_progress: bool,
    ) -> AppResult<()> {
        let mut failed_indices: Vec<usize> = (0..urls.len()).collect();
        for attempt in 0..=self.context.config.max_retries {
            if failed_indices.is_empty() { break; }
            if attempt > 0 {
                warn!("第 {} 次重试下载 {} 个失败的分片...", attempt, failed_indices.len());
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let stream = stream::iter(failed_indices.clone())
                .map(|i| {
                    let url_res = base_url.join(&urls[i]);
                    let ts_path = temp_path.join(format!("{:05}.ts", i));
                    let client = self.context.http_client.clone();
                    let decryptor = decryptor.clone();
                    let pbar_clone = pbar.clone();
                    
                    tokio::spawn(async move {
                        let url = match url_res {
                            Ok(url) => url,
                            Err(e) => return (i, Err(AppError::from(e))),
                        };
                        match Self::download_ts_segment(
                            client, url, &ts_path, decryptor, 
                            pbar_clone, use_byte_progress
                        ).await {
                            Ok(_) => (i, Ok(())),
                            Err(e) => (i, Err(e)),
                        }
                    })
                })
                .buffer_unordered(self.context.config.max_workers * 2);
            let results: Vec<_> = stream.collect().await;
            failed_indices = results
                .into_iter()
                .filter_map(|handle_res| {
                    match handle_res {
                        Ok((_index, Ok(_))) => None, 
                        Ok((index, Err(_))) => Some(index),
                        Err(_) => {
                            error!("一个下载任务 panic 或被取消");
                            None
                        }
                    }
                })
                .collect();
        }

        if !failed_indices.is_empty() {
            error!("{} 个分片最终下载失败", failed_indices.len());
            return Err(AppError::Merge(format!(
                "{} 个分片最终下载失败",
                failed_indices.len()
            )));
        }
        Ok(())
    }

    async fn download_ts_segment(
        client: Arc<RobustClient>,
        url: Url,
        ts_path: &Path,
        decryptor: Option<Aes128CbcDec>,
        pbar: ProgressBar,
        use_byte_progress: bool,
    ) -> AppResult<()> {
        let data = client.get(url).await?.bytes().await?;
        
        if use_byte_progress {
            pbar.inc(data.len() as u64);
        }
        
        let final_data = if let Some(d) = decryptor {
            d.decrypt_padded_vec_mut::<Pkcs7>(&data)
                .map_err(|e| AppError::Security(format!("分片解密失败: {}", e)))?
        } else {
            data.to_vec()
        };
        fs::write(ts_path, &final_data)?;
        Ok(())
    }

    async fn get_m3u8_key_and_playlist(
        &self, 
        m3u8_url: Url
    ) -> AppResult<(Option<Vec<u8>>, Option<String>, m3u8_rs::MediaPlaylist)> {
        // 步骤 1: 获取并解析播放列表
        let media_playlist = self.fetch_and_parse_playlist(&m3u8_url).await?;

        // 步骤 2: 检查是否加密，并提取加密信息
        let Some((key_uri, iv)) = media_playlist.segments.iter().find_map(|seg| {
            seg.key.as_ref().and_then(|k| {
                if let m3u8_rs::Key { uri: Some(uri), iv, .. } = k {
                    Some((uri.clone(), iv.clone()))
                } else {
                    None
                }
            })
        }) else {
            // 如果没有找到加密信息，直接返回
            debug!("M3U8 未加密");
            return Ok((None, None, media_playlist));
        };

        // 步骤 3: 如果已加密，获取并解密密钥
        let decrypted_key = self.fetch_and_decrypt_key(&m3u8_url, &key_uri).await?;

        // 步骤 4: 返回所有结果
        Ok((Some(decrypted_key), iv, media_playlist))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut};

    #[test]
    fn test_aes_cbc_decryption_logic() {
        // --- Arrange ---
        // !!! 警告: 以下是占位符数据。你需要用从真实网络请求中捕获的数据来替换它们 !!!
        
        // 一个已知的16字节 (128位) AES密钥 (以十六进制表示)
        let hex_key = "32333538396135356464346134343933"; 
        // 一个已知的16字节IV (以十六进制表示)
        let hex_iv = "00000000000000000000000000000000";
        // 一段已知的、用上述密钥和IV加密的数据 (通常是一个 .ts 分片的内容)
        let hex_encrypted_data = "bc5c40cb8621101fc486c33ee9e13e85fa91be59351f74192939dd4f0dea23f7"; // 这里需要填入真实的加密数据
        // 这段加密数据解密后应该得到的原始数据
        let hex_expected_decrypted_data = "54686973206973206120746573742121"; // 这里需要填入对应的原始数据
        
        // 将十六进制字符串转换为字节数组
        let key = hex::decode(hex_key).unwrap();
        let iv = hex::decode(hex_iv).unwrap();
        let mut encrypted_data = hex::decode(hex_encrypted_data).unwrap();
        let expected_decrypted_data = hex::decode(hex_expected_decrypted_data).unwrap();

        // --- Act ---
        type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
        let cipher = Aes128CbcDec::new_from_slices(&key, &iv).unwrap();
        
        // 执行解密
        let decrypted_data = cipher.decrypt_padded_vec_mut::<Pkcs7>(&mut encrypted_data).unwrap();

        // --- Assert ---
        assert_eq!(decrypted_data, expected_decrypted_data, "解密后的数据与预期不符");
    }
}