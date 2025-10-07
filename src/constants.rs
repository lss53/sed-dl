// src/constants.rs

pub const UI_WIDTH: usize = 88;
pub const FILENAME_TRUNCATE_LENGTH: usize = 65;
pub const MAX_FILENAME_BYTES: usize = 200;
pub const CONFIG_DIR_NAME: &str = ".sed-dl";
pub const CONFIG_FILE_NAME: &str = "config.json";
pub const DEFAULT_SAVE_DIR: &str = "downloads";
pub const UNCLASSIFIED_DIR: &str = "未分类资源";
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