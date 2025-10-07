// src/downloader/m3u8.rs
use super::{DownloadStatus, FileInfo};
use crate::{client::RobustClient, error::*, DownloadJobContext};
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyInit, KeyIvInit};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures::{stream, StreamExt};
use md5::{Digest, Md5};
use std::{
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::Path,
    sync::Arc,
    time::Duration,
};
use url::Url;

pub(super) struct M3u8Downloader {
    context: DownloadJobContext,
}

impl M3u8Downloader {
    pub(super) fn new(context: DownloadJobContext) -> Self {
        Self { context }
    }

    pub(super) async fn download(&self, item: &FileInfo) -> AppResult<DownloadStatus> {
        let mut url = Url::parse(&item.url)?;
        let token = self.context.token.lock().await;
        if !token.is_empty() {
            url.query_pairs_mut().append_pair("accessToken", &token);
        }
        drop(token); // 释放锁

        let (key, iv, playlist) = self.get_m3u8_key_and_playlist(url.clone()).await?;

        if playlist.segments.is_empty() {
            return Err(AppError::M3u8Parse("M3U8文件不含分片".to_string()));
        }
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

        self.download_segments_with_retry(&url, &segment_urls, temp_dir.path(), decryptor)
            .await?;

        self.merge_ts_segments(temp_dir.path(), segment_urls.len(), &item.filepath)?;
        Ok(DownloadStatus::Success)
    }

    async fn download_segments_with_retry(
        &self,
        base_url: &Url,
        urls: &[String],
        temp_path: &Path,
        decryptor: Option<Aes128CbcDec>,
    ) -> AppResult<()> {
        let mut failed_indices: Vec<usize> = (0..urls.len()).collect();

        for attempt in 0..=self.context.config.max_retries {
            if failed_indices.is_empty() {
                break;
            }
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            let stream = stream::iter(failed_indices.clone())
                .map(|i| {
                    let url = base_url.join(&urls[i]).unwrap();
                    let ts_path = temp_path.join(format!("{:05}.ts", i));
                    let client = self.context.http_client.clone();
                    let decryptor = decryptor.clone();
                    tokio::spawn(async move {
                        Self::download_ts_segment(client, url, &ts_path, decryptor)
                            .await
                            .map_err(|_| i)
                    })
                })
                .buffer_unordered(self.context.config.max_workers * 2);

            let results: Vec<_> = stream.collect().await;
            failed_indices = results
                .into_iter()
                .filter_map(|res| res.unwrap().err())
                .collect();
        }

        if !failed_indices.is_empty() {
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
    ) -> AppResult<()> {
        let data = client.get(url).await?.bytes().await?;
        let final_data = if let Some(d) = decryptor {
            d.decrypt_padded_vec_mut::<Pkcs7>(&data)
                .map_err(|e| AppError::Security(format!("分片解密失败: {}", e)))?
        } else {
            data.to_vec()
        };
        fs::write(ts_path, &final_data)?;
        Ok(())
    }

    fn merge_ts_segments(
        &self,
        temp_dir: &Path,
        num_segments: usize,
        output_path: &Path,
    ) -> AppResult<()> {
        let temp_output_path = output_path.with_extension("tmp");
        let mut writer = BufWriter::new(File::create(&temp_output_path)?);
        for i in 0..num_segments {
            let ts_path = temp_dir.join(format!("{:05}.ts", i));
            if !ts_path.exists() {
                return Err(AppError::Merge(format!(
                    "丢失视频分片: {:?}",
                    ts_path.file_name().unwrap()
                )));
            }
            let mut reader = File::open(ts_path)?;
            io::copy(&mut reader, &mut writer)?;
        }
        writer.flush()?;
        fs::rename(temp_output_path, output_path)?;
        Ok(())
    }

    async fn get_m3u8_key_and_playlist(
        &self,
        m3u8_url: Url,
    ) -> AppResult<(Option<Vec<u8>>, Option<String>, m3u8_rs::MediaPlaylist)> {
        let playlist_text = self
            .context
            .http_client
            .get(m3u8_url.clone())
            .await?
            .text()
            .await?;
        let playlist = m3u8_rs::parse_playlist_res(playlist_text.as_bytes())
            .map_err(|e| AppError::M3u8Parse(e.to_string()))?;

        if let m3u8_rs::Playlist::MediaPlaylist(media) = playlist {
            let key_info = media.segments.iter().find_map(|seg| {
                seg.key.as_ref().and_then(|k| {
                    if let m3u8_rs::Key {
                        uri: Some(uri),
                        iv,
                        ..
                    } = k
                    {
                        Some((uri.clone(), iv.clone()))
                    } else {
                        None
                    }
                })
            });

            if let Some((uri, iv)) = key_info {
                use serde_json::Value;
                let key_url = m3u8_url.join(&uri)?;
                let nonce_url = format!("{}/signs", key_url);
                let signs_data: Value = self.context.http_client.get(&nonce_url).await?.json().await?;
                let nonce = signs_data
                    .get("nonce")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AppError::M3u8Parse("密钥服务器响应中未找到 'nonce'".to_string()))?;

                let key_filename = key_url
                    .path_segments()
                    .and_then(|segments| segments.last())
                    .ok_or_else(|| {
                        AppError::M3u8Parse(format!("无法从密钥URL中提取文件名: {}", key_url))
                    })?;

                let sign_material = format!("{}{}", nonce, key_filename);
                let mut hasher = Md5::new();
                hasher.update(sign_material.as_bytes());
                let result = hasher.finalize();
                let sign = &format!("{:x}", result)[..16];

                let final_key_url = format!("{}?nonce={}&sign={}", key_url, nonce, sign);
                let key_data: Value = self
                    .context
                    .http_client
                    .get(&final_key_url)
                    .await?
                    .json()
                    .await?;
                let encrypted_key_b64 = key_data
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        AppError::M3u8Parse("密钥服务器响应中未找到加密密钥 'key'".to_string())
                    })?;

                let encrypted_key = BASE64.decode(encrypted_key_b64)?;
                type EcbDec = ecb::Decryptor<aes::Aes128>;
                let cipher = EcbDec::new(sign.as_bytes().into());
                let decrypted_key = cipher
                    .decrypt_padded_vec_mut::<Pkcs7>(&encrypted_key)
                    .map_err(|e| AppError::Security(format!("AES密钥解密失败: {}", e)))?;

                return Ok((Some(decrypted_key), iv, media));
            }
            return Ok((None, None, media));
        }
        Err(AppError::M3u8Parse(
            "预期的M3U8文件不是媒体播放列表".to_string(),
        ))
    }
}