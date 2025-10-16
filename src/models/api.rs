// src/models/api.rs

use chrono::{DateTime, FixedOffset};
use serde::Deserialize;

// --- 通用结构体 ---

#[derive(Deserialize, Debug, Clone)]
pub struct ZhCn {
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

#[derive(Deserialize, Debug, Clone)]
pub struct Teacher {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct TeachingMaterialInfo {
    pub id: String,
}

// --- 课程通用结构体 ---

#[derive(Deserialize, Debug, Clone)]
pub struct CourseDetailsCustomProperties {
    pub teachingmaterial_info: Option<TeachingMaterialInfo>,
    pub lesson_teacher_ids: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CourseResourceCustomProperties {
    pub alias_name: Option<String>,
    #[serde(default)]
    pub height: Option<String>, // 新增 height 字段
}

#[derive(Deserialize, Debug, Clone)]
pub struct CourseResource {
    pub id: String,
    pub global_title: ZhCn,
    pub resource_type_code: String,
    pub update_time: DateTime<FixedOffset>,
    pub custom_properties: CourseResourceCustomProperties,
    pub ti_items: Option<Vec<TiItem>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResourceStructureRelationCustomProperties {
    pub teacher_ids: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResourceStructureRelation {
    pub title: String,
    pub res_ref: Option<Vec<String>>,
    pub custom_properties: ResourceStructureRelationCustomProperties,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResourceStructure {
    pub relations: Option<Vec<ResourceStructureRelation>>,
}

// --- 精品课 (qualityCourse) 专用模型 ---

#[derive(Deserialize, Debug, Clone)]
pub struct CourseDetailsRelations {
    #[serde(default, rename = "course_resource")]
    pub resources: Vec<CourseResource>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CourseDetailsResponse {
    pub global_title: ZhCn,
    pub tag_list: Option<Vec<Tag>>,
    pub custom_properties: CourseDetailsCustomProperties,
    pub chapter_paths: Option<Vec<String>>,
    pub relations: CourseDetailsRelations,
    pub resource_structure: Option<ResourceStructure>,
    pub teacher_list: Option<Vec<Teacher>>,
}

// --- 同步课 (syncClassroom) 专用模型 ---

#[derive(Deserialize, Debug, Clone)]
pub struct SyncClassroomRelations {
    #[serde(default, rename = "national_course_resource")]
    pub resources: Vec<CourseResource>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct SyncClassroomResponse {
    pub global_title: ZhCn,
    pub tag_list: Option<Vec<Tag>>,
    pub custom_properties: CourseDetailsCustomProperties,
    pub chapter_paths: Option<Vec<String>>,
    pub relations: SyncClassroomRelations,
    pub resource_structure: Option<ResourceStructure>,
    #[serde(default)]
    pub teacher_list: Vec<Teacher>,
}

// --- 教材 (Textbook) 专用模型 ---

#[derive(Deserialize, Debug, Clone)]
pub struct TextbookDetailsResponse {
    pub id: String,
    pub title: Option<String>,
    pub global_title: Option<ZhCn>,
    pub ti_items: Option<Vec<TiItem>>,
    pub tag_list: Option<Vec<Tag>>,
    pub update_time: DateTime<FixedOffset>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AudioRelationItem {
    pub global_title: ZhCn,
    pub ti_items: Option<Vec<TiItem>>,
    pub update_time: DateTime<FixedOffset>,
}
