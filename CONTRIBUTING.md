# 为 sed-dl 做出贡献

我们非常欢迎并感谢您对本项目的兴趣！您的每一份贡献都对我们至关重要。

## 如何贡献

### 报告 Bug

- 请先在 [Issues 页面](https://github.com/lss53/sed-dl/issues) 搜索，确保您的问题尚未被报告。
- 如果没有，请创建一个新的 Issue，并尽可能详细地描述问题，包括：
  - 您的操作系统。
  - `sed-dl` 的版本 (`sed-dl -V`)。
  - 复现问题的详细步骤。
  - 相关的错误日志或截图。

### 提交功能建议

- 请在 [Issues 页面](https://github.com/lss53/sed-dl/issues) 创建一个新的 Issue。
- 详细描述您想要的功能，以及它能解决什么问题。

### 贡献代码

1. **Fork** 本仓库。
2. **Clone** 您 fork 的仓库到本地: `git clone https://github.com/YOUR_USERNAME/sed-dl.git`
3. 创建一个新的分支: `git checkout -b feature/your-amazing-feature`
4. 进行代码修改。在提交前，请确保：
   - 代码已通过 `cargo fmt` 格式化。
   - 代码已通过 `cargo clippy -- -D warnings` 检查，没有警告。
   - 代码已通过 `cargo test` (如果存在测试)。
5. **Commit** 您的修改: `git commit -m "feat: Add some amazing feature"`
6. **Push** 到您的分支: `git push origin feature/your-amazing-feature`
7. 在 GitHub 上创建一个 **Pull Request**。

感谢您的贡献！