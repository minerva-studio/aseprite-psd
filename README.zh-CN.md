# psd2ase

[English](README.md)

`psd2ase` 用于在 Photoshop PSD/PSB 与 Aseprite 文档之间双向转换。项目同时
提供原生命令行程序，以及内附 converter、支持导入和原生 Save As 的 Aseprite
扩展。

## 快速开始：Aseprite 扩展

1. 打开 [最新 GitHub Release](https://github.com/minerva-studio/psd-to-ase/releases/latest)。
2. 下载对应平台的扩展：
   - Windows x64 使用 `psd2ase-aseprite-windows-x64.aseprite-extension`。
   - 使用 glibc 的 Linux x64 使用 `psd2ase-aseprite-linux-x64.aseprite-extension`。
3. 打开下载的扩展包并安装到 Aseprite；如果菜单命令没有立即出现，请重启
   Aseprite。
4. 选择 **File > Import > Import PSD to Aseprite...**，然后选择 PSD。
5. 首次使用时，允许 Aseprite 启动扩展内附的外部 converter。

导入结果会作为已修改、但未与转换临时文件关联的文档打开。按 Ctrl+S 或使用
Save As 选择最终 `.aseprite` 路径；Aseprite 会默认建议 PSD 所在目录和同名文件。

扩展默认使用 `preserve`，保持源图层相互独立。导入对话框也提供下文所述的
实验性自动关联模式。

带有该标记的 PSD 再次导入时默认使用 `auto`；普通 PSD 仍默认使用 `preserve`。

导出默认会写入不可见、带版本的 PSD round-trip 元数据。它只记录元数据版本、
逻辑图层 ID 和物化 cel 关系，不包含文件路径、用户名、设备信息或使用追踪；
Photoshop 等其他读取器可以忽略这段信息。通过 **File > Export > PSD to Aseprite
Settings...** 可以关闭后续导出的标记。关闭后 PSD 仍可正常读写，但再次打开时
无法自动识别本转换器生成的图层关联。

导出时选择 **File > Save As...**，并指定 `.psd` 或 `.psb`。扩展会分别创建
隔离的原始副本与扁平副本，调用内附 converter 并验证 Photoshop 文档，最后
才通过 Aseprite 自定义格式的保存流写入。保存选项可以选择是否把当前帧写成
Photoshop 的 active frame（导出始终使用当前帧），以及选择通道压缩（`ZIP`、
`ZIP prediction`、`RLE` 或 `Raw`）；此后 Ctrl+S 会继续使用所选格式和选项。

## 命令行

使用 Rust 1.88 或更高版本构建原生 CLI：

```text
cargo build --release --locked -p psd2ase
```

导出命令支持 `--compression raw|rle|zip|zip-prediction`；省略时保持现有的
ZIP（无 prediction）默认行为。

也可以一条命令构建 Windows x64 Aseprite 扩展；脚本会先构建 release
converter，再把它嵌入扩展包：

```text
bash tools/package-aseprite-extension.sh --platform windows-x64
```

产物写入 `dist/psd2ase-aseprite-windows-x64.aseprite-extension`。如果
converter 已经在其他位置构建完成，可改用 `--binary PATH --no-build`。

只检查 PSD，不写输出文件：

```text
psd2ase inspect INPUT.psd
```

转换 PSD。除非指定 `--overwrite`，否则不会替换已有输出：

```text
psd2ase convert INPUT.psd -o OUTPUT.aseprite
psd2ase convert INPUT.psd -o OUTPUT.aseprite --overwrite
psd2ase convert INPUT.psd -o OUTPUT.aseprite --layer-association auto --linked-cels identical
psd2ase convert INPUT.psd -o OUTPUT.aseprite --layer-association auto --linked-cels identical --jitter-mode repair --jitter-kind all
```

使用由 Aseprite 另行生成的扁平快照导出 Aseprite 文档。输出后缀决定 PSD 或
PSB；除非明确指定 `--overwrite`，已有输出不会被替换：

```text
psd2ase export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite
psd2ase export INPUT.aseprite -o OUTPUT.psb --composite COMPOSITE.aseprite --report REPORT.json --overwrite
psd2ase export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --roundtrip-metadata off
```

使用 `psd2ase --help` 查看完整命令格式。

## 图层关联

- `--layer-association preserve` 是默认模式，保持源图层身份。
- `--layer-association auto --association-strategy compact` 启用紧凑的跨帧
  逻辑图层规划。
- `--association-strategy conservative` 启用多语言复制家族、多轨和候选 Folder
  分析；身份不明确的图层仍保持分离。
- 稳定轨道顺序默认使用跨帧重叠共识。使用 `--stable-order anchor` 可改用锚点帧
  顺序，使用 `strict` 可在证据无法确定时拒绝转换。
- `--z-order auto` 启用实验性的逐 cel Z-Index，并且必须配合自动关联。
  conservative 模式还可使用 `--uncertain-layers flat` 禁用候选 Folder。

`--linked-cels identical` 会在自动关联生成的同一个输出图层内无损复用完全
相同的 RGBA 像素缓冲；位置、透明度和逐 cel Z-Index 仍按帧保留。默认值为
`off`，只有尺寸和字节都完全一致时才会建立链接。它要求同时使用
`--layer-association auto`，因为 `preserve` 会独立输出每个源图层，没有跨图层
cel 复用候选。

## 导入防抖

防抖默认关闭。`--jitter-mode report` 只报告疑似问题，`assist` 只把稳定化
结果作为自动图层关联的证据，`repair` 才会改变导出的 cel。可用
`--jitter-kind alpha|color|all` 选择低 Alpha 孤立杂点或同一逻辑轨道内的
轻微颜色差异，并用 `--jitter-profile conservative|balanced` 选择阈值预设。
颜色防抖只在自动关联已经确认的同一轨道、同尺寸和同位置 cel 之间进行；修复
时选择真实代表 cel，不合成新颜色。高级阈值可通过
`--jitter-alpha-threshold`、`--jitter-max-speck-area`、
`--jitter-max-changed-ratio` 和 `--jitter-max-channel-delta` 覆盖。

## 当前边界

- 导入扩展包已在 Aseprite 1.3.18.3、Windows x64 和使用 glibc 的
  Ubuntu/WSL2 Linux x64 环境验证。在该 API 进入 Aseprite 稳定版前，
  PSD/PSB Save As 还要求 Aseprite #6008 所实现的自定义格式保存回调。
- 本版本没有打包或测试 macOS。
- 扩展会注册 PSD/PSB 自定义格式的加载与保存回调；需要配置导入策略时仍可使用
  明确的导入菜单命令。
- 转换会保留规范化图层树、RGBA8 cel、Photoshop 帧动画和受支持的图层状态。
  逻辑图层关联及部分坐标映射仍属实验功能，重要输出应在 Aseprite 中人工检查。
- 导出会保留受支持的组、静态图层属性、帧时长、cel 可见性/位置/透明度、相同
  cel 复用，以及确定性的 tag 播放序列。Tilemap 使用独立扁平快照并报告为
  rasterized；无法继续编辑的 tag 名称/边界、slice、颜色配置和逐 cel Z-Index
  会进入信息损失报告。
- 仓库有意不提交 PSD 或 PSB 测试素材。

## 开发

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo run -p psd2ase -- --version
cargo run -p psd2ase -- --help
```

解析和写入依赖为 Minerva 维护并固定 commit 的 `ag-psd` fork；`aseprite-io`
仍使用已发布 crate。上游仓库和许可证详情记录在
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)。项目采用
[MIT License](LICENSE)。
