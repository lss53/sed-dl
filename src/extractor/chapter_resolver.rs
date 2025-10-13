// src/extractor/chapter_resolver.rs

use crate::{client::RobustClient, config::AppConfig, error::*, utils};
use dashmap::DashMap;
use log::{debug, warn};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};

pub struct ChapterTreeResolver {
    http_client: Arc<RobustClient>,
    config: Arc<AppConfig>,
    cache: DashMap<String, Value>,
}

impl ChapterTreeResolver {
    pub fn new(http_client: Arc<RobustClient>, config: Arc<AppConfig>) -> Self {
        Self {
            http_client,
            config,
            cache: DashMap::new(),
        }
    }

    async fn get_tree_data(&self, tree_id: &str) -> AppResult<Value> {
        if let Some(entry) = self.cache.get(tree_id) {
            debug!("章节树缓存命中: {}", tree_id);
            return Ok(entry.value().clone());
        }
        debug!("章节树缓存未命中，从网络获取: {}", tree_id);
        let url_template = self.config.url_templates.get("CHAPTER_TREE").unwrap();
        let data: Value = self
            .http_client
            .fetch_json(url_template, &[("tree_id", tree_id)])
            .await?;

        self.cache.insert(tree_id.to_string(), data.clone());
        Ok(data)
    }

    pub async fn get_full_chapter_path(
        &self,
        tree_id: &str,
        chapter_path_str: &str,
    ) -> AppResult<PathBuf> {
        let tree_data = self.get_tree_data(tree_id).await?;
        let lesson_node_id = chapter_path_str.split('/').next_back().unwrap_or("");
        debug!(
            "在树 '{}' 中查找节点 '{}' 的完整路径",
            tree_id, lesson_node_id
        );

        let nodes_to_search =
            if let Some(nodes) = tree_data.get("child_nodes").and_then(|v| v.as_array()) {
                nodes
            } else if let Some(nodes) = tree_data.as_array() {
                nodes
            } else {
                warn!("章节树 '{}' 结构未知或为空", tree_id);
                return Ok(PathBuf::new());
            };

        if let Some(path) = self.find_path_in_tree(nodes_to_search, lesson_node_id, vec![]) {
            let path_buf: PathBuf = path.iter().collect();
            debug!("找到完整章节路径: {:?}", path_buf);
            Ok(path_buf)
        } else {
            warn!(
                "在树 '{}' 中未找到节点 '{}' 的路径",
                tree_id, lesson_node_id
            );
            Ok(PathBuf::new())
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    fn find_path_in_tree(
        &self,
        nodes: &[Value],
        target_id: &str,
        current_path: Vec<String>,
    ) -> Option<Vec<String>> {
        for node in nodes {
            let title = node
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("未知章节");
            let mut new_path = current_path.clone();
            new_path.push(utils::sanitize_filename(title));
            if node.get("id").and_then(|v| v.as_str()) == Some(target_id) {
                return Some(new_path);
            }
            if let Some(child_nodes) = node.get("child_nodes").and_then(|v| v.as_array())
                && let Some(found_path) = self.find_path_in_tree(child_nodes, target_id, new_path) {
                return Some(found_path);
            }
        }
        None
    }
}
