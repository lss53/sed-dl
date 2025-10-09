# 国家中小学智慧教育平台资源下载工具 (sed-dl)

<div align="center">

<p align="center">
    <a href="https://github.com/lss53/sed-dl/actions/workflows/release.yml">
        <img src="https://github.com/lss53/sed-dl/actions/workflows/release.yml/badge.svg" alt="构建状态">
    </a>
    <a href="https://github.com/lss53/sed-dl/releases/latest">
        <img src="https://img.shields.io/github/v/release/lss53/sed-dl" alt="最新版本">
    </a>
    <a href="https://github.com/lss53/sed-dl/releases/latest">
        <img src="https://img.shields.io/github/downloads/lss53/sed-dl/total" alt="下载次数">
    </a>
    <a href="https://opensource.org/licenses/MIT">
        <img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="许可证: MIT">
    </a>
</p>

**专为国家中小学智慧教育平台设计的强大命令行下载工具，基于 Rust 构建 🦀，简单高效，稳定可靠。**

</div>

---

`sed-dl` 旨在为教师、学生和家长提供便捷的离线资源下载方案，支持课程视频、电子教材及配套音频等多种资源类型，满足不同场景下的学习与教学需求。

## ✨ 核心功能

-   **全面解析**：支持同步课堂、精品课程、电子教材等多种资源类型。
-   **高效下载**：
    -   🚀 **并发下载**：支持多文件同时下载，充分利用网络带宽。
    -   🔄 **断点续传**：网络异常中断后，可自动恢复下载进度。
    -   ✅ **完整性校验**：通过 MD5 和文件大小校验，确保下载内容完整无误。
-   **视频专项优化**：
    -   🎬 **M3U8 支持**：自动解析并合并加密视频流，输出为可在主流播放器中直接播放的完整 `.ts` 视频文件。
    -   📺 **多清晰度**：支持选择 1080p、720p 等不同画质。
-   **使用便捷**：
    -   🌳 **自动归类**：按学科、年级、版本等自动生成清晰的文件目录。
    -   ✍️ **规范命名**：自动过滤非法字符，生成整洁可读的文件名。
    -   🎨 **友好界面**：彩色进度提示与状态反馈，操作过程一目了然。
-   **多模式操作**：
    -   **交互模式**：适合逐条输入链接或 ID，操作简单直观。
    -   **批量模式**：支持从文件读取多个链接，一次性完成下载任务。
    -   **命令行模式**：支持直接传入参数，便于集成或脚本调用。
-   **跨平台运行**：基于 Rust 编写，支持 Windows、macOS 和 Linux 系统。

## 📥 安装说明

### 方式一：下载预编译版本（推荐）

访问 [GitHub Releases](https://github.com/lss53/sed-dl/releases) 页面，下载对应系统的可执行文件，解压后即可使用，无需配置额外环境。

### 方式二：从源码构建

如果您未安装 Rust 环境，请先访问 [rustup.rs](https://rustup.rs/) 安装 Rust 工具链。

```bash
# 克隆项目
git clone https://github.com/lss53/sed-dl.git
cd sed-dl

# 编译发布版本
cargo build --release

# 编译结果位于 ./target/release/sed-dl
```

## 🚀 使用指南

### 1. 获取 Access Token

使用 `sed-dl` 前需获取有效的 `Access Token`，用于身份验证。

#### 获取方式：

> **1. 登录平台**：使用 Chrome/Edge/Firefox 访问并登录 [国家中小学智慧教育平台](https://auth.smartedu.cn/uias/login)。
>
> **2. 打开开发者工具**：
>    -   Windows/Linux：按 `F12` 或 `Ctrl+Shift+I`
>    -   macOS：按 `Cmd+Opt+I` (⌘⌥I)
>
> **3. 进入“控制台” (Console)** 标签页。
>
> **4. 粘贴并执行以下脚本**：
> ```javascript
> copy(JSON.parse(JSON.parse(localStorage.getItem(Object.keys(localStorage).find(i => i.startsWith("ND_UC_AUTH")))).value).access_token)
> ```
> **5. Token 将自动复制到剪贴板**，粘贴到工具中即可。程序会优先使用命令行传入的 `--token` 参数，其次是环境变量，最后才是自动保存的 Token。首次使用后，程序会自动保存 Token，后续无需重复输入。

#### 手动获取（备选）：

![令牌截图](.github/assets/token.png)

### 2. 使用示例

#### 交互模式（推荐新手）

使用 `-i` 参数启动交互模式，按提示输入链接或资源 ID。

```bash
sed-dl -i
```
![交互模式截图](.github/assets/sed-dl-i.png)

#### 下载单个资源

使用 `--url` 参数直接指定链接，程序将自动识别资源类型并下载。

```bash
# 下载同步课堂的视频与课件
sed-dl --url "https://.../classActivity?activityId=******"

# 下载电子教材及音频
sed-dl --url "https://.../tchMaterial?contentId=******"
```

#### 批量下载

将多个链接或 ID 存入文本文件（如 `links.txt`），每行一个。

**示例 `links.txt` 文件：**
```
https://.../classActivity?activityId=...
https://.../tchMaterial?contentId=...
a1b2c3d4-....-....-....-e5f6g7h8i9j0
```

使用 `-b` 或 `--batch-file` 指定文件路径。**若文件中包含资源 ID，必须使用 `--type` 指定类型。**

```bash
sed-dl -b links.txt --type "syncClassroom/classActivity" -o "D:\课程资料"
```
可用 `--type` 选项包括：`tchMaterial`、`qualityCourse`、`syncClassroom/classActivity`。

#### 查看完整帮助

```bash
sed-dl --help
```

## ⚠️ 注意事项

-   请合理使用本工具，尊重平台版权，下载资源仅限个人学习与研究。
-   `Access Token` 具有有效期，如遇 401/403 等认证错误，请重新获取。
-   本工具为开源项目，作者不对因使用本工具引发的任何问题负责。

## 🤝 参与贡献

欢迎提交问题反馈、功能建议或代码改进！

-   **报告问题**：请在 [GitHub Issues](https://github.com/lss53/sed-dl/issues) 提交 Bug 或建议。
-   **代码贡献**：欢迎 Fork 项目并提交 Pull Request，代码格式请遵循 `rustfmt` 规范。

## 📚 开源致谢

本项目使用了以下优秀开源项目的思路或代码：

-   [smartedu-download](https://github.com/52beijixing/smartedu-download)
-   [smartedu-dl-go](https://github.com/hantang/smartedu-dl-go)
-   [smartedu-dl-py](https://github.com/changsongyang/smartedu-dl-py)
-   [tchMaterial-parser](https://github.com/happycola233/tchMaterial-parser)
-   [FlyEduDownloader](https://github.com/cjhdevact/FlyEduDownloader)

## 📄 开源协议

本项目基于 [MIT License](LICENSE) 开源。

---

**免责声明**：本工具仅为个人学习与技术研究而开发，请勿用于商业用途。所有资源的版权归国家中小学智慧教育平台及相关权利方所有。