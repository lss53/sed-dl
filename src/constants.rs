// src/constants.rs

pub const UI_WIDTH: usize = 88;
pub const FILENAME_TRUNCATE_LENGTH: usize = 65;
pub const MAX_FILENAME_BYTES: usize = 200;
pub const CONFIG_DIR_NAME: &str = concat!(".", clap::crate_name!());
pub const CONFIG_FILE_NAME: &str = "config.json";
pub const DEFAULT_SAVE_DIR: &str = "downloads";
pub const UNCLASSIFIED_DIR: &str = "未分类资源";
pub const DEFAULT_AUDIO_FORMAT: &str = "mp3";
pub const DEFAULT_VIDEO_QUALITY: &str = "best";
pub const DEFAULT_SELECTION: &str = "all";
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36";

pub const HELP_TOKEN_GUIDE: &str = r#"
1. 登录平台: 使用 Chrome / Edge / Firefox 浏览器登录。
   (登录地址: https://auth.smartedu.cn/uias/login)
2. 打开开发者工具:
   - 在 Windows / Linux 上: 按 F12 或 Ctrl+Shift+I
   - 在 macOS 上: 按 Cmd+Opt+I (⌘⌥I)
3. 切换到“控制台” (Console) 标签页。
4. 复制并粘贴以下代码到控制台，然后按 Enter 运行：
----------------------------------------------
copy(
  JSON.parse(
    JSON.parse(
      localStorage.getItem(
        Object.keys(localStorage)
          .find(i => i.startsWith("ND_UC_AUTH"))
      )
    ).value
  ).access_token
)
----------------------------------------------
5. 此时 Token 已自动复制到剪贴板，可以直接粘贴使用。"#;

pub mod api {
    pub mod types {
        pub const TCH_MATERIAL: &str = "tchMaterial";
        pub const QUALITY_COURSE: &str = "qualityCourse";
        pub const SYNC_CLASSROOM: &str = "syncClassroom/classActivity";
    }
    pub mod resource_formats {
        pub const PDF: &str = "pdf";
        pub const M3U8: &str = "m3u8";
        pub const BIN: &str = "bin";
    }
    pub mod resource_types {
        pub const ASSETS_VIDEO: &str = "assets_video";
        pub const ASSETS_DOCUMENT: &str = "assets_document";
        pub const COURSEWARES: &str = "coursewares";
        pub const LESSON_PLANDESIGN: &str = "lesson_plandesign";
    }
}