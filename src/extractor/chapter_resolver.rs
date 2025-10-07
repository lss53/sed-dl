// src/extractor/chapter_resolver.rs

use crate::{client::RobustClient, config::AppConfig, error::*, utils};
use dashmap::DashMap;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

/// 章节树解析器，带缓存
pub struct ChapterTreeResolver {
    http_client: Arc<RobustClient>,
    config: Arc<AppConfig>,
    cache: DashMap<String, (Value, SystemTime)>,
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
            if entry.1.elapsed().unwrap_or_default() < Duration::from_secs(3600) {
                return Ok(entry.0.clone());
            }
        }
        let url_template = self.config.url_templates.get("CHAPTER_TREE").unwrap();
        let data = self
            .http_client
            .fetch_json(url_template, &[("tree_id", tree_id)])
            .await?;
        self.cache
            .insert(tree_id.to_string(), (data.clone(), SystemTime::now()));
        Ok(data)
    }

    pub async fn get_full_chapter_path(
        &self,
        tree_id: &str,
        chapter_path_str: &str,
    ) -> AppResult<PathBuf> {
        let tree_data = self.get_tree_data(tree_id).await?;
        let lesson_node_id = chapter_path_str.split('/').last().unwrap_or("");

        let nodes_to_search = if let Some(nodes) = tree_data.get("child_nodes").and_then(|v| v.as_array())
        {
            nodes
        } else if let Some(nodes) = tree_data.as_array() {
            nodes
        } else {
            return Ok(PathBuf::new());
        };

        if let Some(path) = self.find_path_in_tree(nodes_to_search, lesson_node_id, vec![]) {
            Ok(path.iter().collect())
        } else {
            Ok(PathBuf::new())
        }
    }

    fn find_path_in_tree<'a>(
        &self,
        nodes: &'a [Value],
        target_id: &str,
        current_path: Vec<String>,
    ) -> Option<Vec<String>> {
        for node in nodes {
            let title = node.get("title").and_then(|v| v.as_str()).unwrap_or("未知章节");
            let mut new_path = current_path.clone();
            new_path.push(utils::sanitize_filename(title));

            if node.get("id").and_then(|v| v.as_str()) == Some(target_id) {
                return Some(new_path);
            }

            if let Some(child_nodes) = node.get("child_nodes").and_then(|v| v.as_array()) {
                if let Some(found_path) = self.find_path_in_tree(child_nodes, target_id, new_path) {
                    return Some(found_path);
                }
            }
        }
        None
    }
}