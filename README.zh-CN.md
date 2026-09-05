# aseprite-psd

[English](README.md)

`aseprite-psd` 用于在 Photoshop PSD/PSB 与 Aseprite 文档之间双向转换。项目同时
提供原生命令行程序，以及内附 converter、支持导入和导出流程的 Aseprite
扩展。

## 为什么使用 aseprite-psd？

| 功能 | [Tin-01](https://github.com/Tin-01/aseprite-psd-scripts) | [Resprite](https://resprite.fengeon.com/zh/docs/files/psd) | aseprite-psd |
| --- | --- | --- | --- |
| 使用方式 | Aseprite Lua 脚本 | Resprite 内置功能 | Aseprite 扩展 + 独立 CLI |
| 输入格式 | RGB/RGBA 8-bpc、PackBits PSD 子集 | 分层 PSD | PSD、PSB、Raw/RLE/ZIP |
| Photoshop Frame Animation | 单帧导入路径 | 文档未说明 | 重建为 Aseprite 帧 |
| 自动图层关联 | 无 | 文档未说明 | logical tracks、候选 Folder 和诊断 |
| Linked cels | 无 | 文档未说明 | 相同像素可恢复为 linked cels |
| 16/32 bits-per-channel | 不支持 | 文档未说明 | 导入并明确降级为 RGBA8 |
| PSD slices | 无 | 文档未说明 | 保留名称、顺序、bounds 和静态 key |
| 信息损失报告 | debug log | 文档未说明 | 带版本的结构化报告 |
| 输出验证 | 文档未说明 | 文档未说明 | Aseprite reread 与结构验证 |

也期待 [Aseprite 原生 PSD 支持](https://github.com/aseprite/aseprite/issues/114)早日完成。

## 快速开始：Aseprite 扩展

1. 打开 [最新 GitHub Release](https://github.com/minerva-studio/aseprite-psd/releases/latest)。
2. 如需最简单的安装方式，下载 `aseprite-psd-universal.aseprite-extension`；
   也可以选择体积更小的单平台扩展：
   - Windows x64 使用 `aseprite-psd-windows-x64.aseprite-extension`。
   - 使用 glibc 的 Linux x64 使用 `aseprite-psd-linux-x64.aseprite-extension`。
   - Apple Silicon macOS 使用 `aseprite-psd-macos-arm64.aseprite-extension`。
   - Intel macOS 使用 `aseprite-psd-macos-x64.aseprite-extension`。
3. 打开下载的扩展包并安装到 Aseprite；如果菜单命令没有立即出现，请重启
   Aseprite。
4. 选择 **File > Import > Import PSD/PSB...**，然后选择 Photoshop 文档。
5. 首次使用时，允许 Aseprite 启动扩展内附的外部 converter。

macOS 构建产物当前未进行代码签名或 notarization，下载后可能受到 Gatekeeper
限制。

通过 `File > Open` 和 `File > Save As...` 直接打开、保存 PSD/PSB，预计需要
Aseprite 1.3.18.4。在该版本可用之前，请改用 **File > Import > Import PSD/PSB...**
和 **File > Export > Export PSD/PSB...**。

## 使用文档

- [用户手册](docs/user-guide.zh-CN.md)：按 PSD 组织方式选择导入流程、检查结果、导出和常见问题。
- [选项参考](docs/options.zh-CN.md)：界面选项、CLI 和高级设置。
- [Changelog](CHANGELOG.md) 与 [开发工作流](docs/development.md)。

## 开发

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo run -p aseprite-psd -- --version
cargo run -p aseprite-psd -- --help
```

解析和写入依赖为 Minerva 维护并固定 commit 的 `ag-psd` fork；`aseprite-io`
仍使用已发布 crate。上游仓库和许可证详情记录在
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)。项目采用
[MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 双许可证。
