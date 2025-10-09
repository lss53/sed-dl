// src/models/api.rs

use chrono::{DateTime, FixedOffset};
use serde::Deserialize;

// --- 通用结构体 ---

#[derive(Deserialize, Debug, Clone)]
pub struct GlobalTitle {
    #[serde(rename = "zh-CN")]
    pub zh_cn: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Requirement {
    pub name: String,
    pub value: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct TiItemCustomProperties {
    pub requirements: Option<Vec<Requirement>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct TiItem {
    pub ti_format: String,
    pub ti_storages: Option<Vec<String>>,
    pub ti_md5: Option<String>,
    pub ti_size: Option<u64>,
    #[serde(default)]
    pub ti_file_flag: Option<String>,
    pub custom_properties: Option<TiItemCustomProperties>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Tag {
    pub tag_dimension_id: String,
    pub tag_name: String,
}


// --- 课程 (Course) API 响应结构体 ---

#[derive(Deserialize, Debug, Clone)]
pub struct CourseDetailsResponse {
    pub global_title: GlobalTitle,
    pub tag_list: Option<Vec<Tag>>,
    pub custom_properties: CourseCustomProperties,
    pub chapter_paths: Option<Vec<String>>,
    pub teacher_list: Option<Vec<Teacher>>,
    pub relations: Relations,
    pub resource_structure: Option<ResourceStructure>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CourseCustomProperties {
    pub teachingmaterial_info: Option<TeachingMaterialInfo>,
    pub lesson_teacher_ids: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct TeachingMaterialInfo {
    pub id: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Teacher {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Relations {
    #[serde(alias = "national_course_resource", alias = "course_resource")]
    pub resources: Option<Vec<CourseResource>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CourseResource {
    pub global_title: GlobalTitle,
    pub custom_properties: CourseResourceCustomProperties,
    pub update_time: Option<DateTime<FixedOffset>>,
    pub resource_type_code: String,
    pub ti_items: Option<Vec<TiItem>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CourseResourceCustomProperties {
    pub alias_name: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResourceStructure {
    pub relations: Option<Vec<ResourceStructureRelation>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResourceStructureRelation {
    pub title: String,
    pub res_ref: Option<Vec<String>>,
    pub custom_properties: ResourceStructureRelationCustomProperties,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResourceStructureRelationCustomProperties {
    pub teacher_ids: Option<Vec<String>>,
}


// --- 教材 (Textbook) API 响应结构体 ---

#[derive(Deserialize, Debug, Clone)]
pub struct TextbookDetailsResponse {
    pub id: String,
    pub title: Option<String>,
    pub global_title: Option<GlobalTitle>,
    pub ti_items: Option<Vec<TiItem>>,
    pub tag_list: Option<Vec<Tag>>,
    pub update_time: Option<DateTime<FixedOffset>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AudioRelationItem {
    pub global_title: GlobalTitle,
    pub ti_items: Option<Vec<TiItem>>,
    pub update_time: Option<DateTime<FixedOffset>>,
}