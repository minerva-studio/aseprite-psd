# aseprite-psd

[English](README.md)

`aseprite-psd` 用于在 Photoshop PSD/PSB 与 Aseprite 文档之间双向转换。项目同时
提供原生命令行程序，以及内附 converter、支持导入和导出流程的 Aseprite
扩展。

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
Aseprite 1.3.18.4；上游进度见 [Aseprite #6007](https://github.com/aseprite/aseprite/issues/6007)。
在该版本可用之前，请改用 **File > Import > Import PSD/PSB...** 和
**File > Export > Export PSD/PSB...**。

明确的 Import 命令会打开已修改、且未与转换临时文件关联的文档。原生
集成可用后，`File > Open` 会返回与原始 PSD 关联的文档。对于明确导入的文档，
可按 Ctrl+S 或使用 Save As 选择最终 `.aseprite` 路径；Aseprite 会默认建议 PSD
所在目录和同名文件。

明确的 Import 命令，以及原生集成可用后的 `File > Open`，都会显示同一套导入选项。
用户可选择 `Automatic association` 或 `Preserve layers`。在 Automatic association 下，
勾选 `Use metadata` 会使用精确的 metadata preset；取消后会启用下方 association 调参并
使用普通启发式路径。旧 v1 标记和无标记文件使用自动关联回退；converter 元数据损坏时
会进入恢复选择，不会被静默忽略。特别地，没有 metadata 的 PSD 不会被当作
`Preserve layers`，而是有意回退到标准的 Automatic association。需要为这类文件调整
association strategy 时，请取消 `Use metadata`。

导出默认会写入不可见、带版本的 PSD metadata。它只记录元数据版本、
逻辑图层 ID 和物化 cel 关系，不包含文件路径、用户名、设备信息或使用追踪；
Photoshop 等其他读取器可以忽略这段信息。通过 **File > Export > Aseprite ↔ Photoshop
Settings...** 可以分别控制导出时写入 metadata，以及导入时是否使用 metadata。
关闭导出标记后 PSD 仍可正常读写，但后续打开无法使用本转换器的精确图层关联。

原生集成可用后，在 `File > Open` 中取消导入对话框会明确报告
`PSD opening cancelled by user.`，不会伪装成失败的 Sprite 或半初始化文档。

当前导出请使用 **File > Export > Export PSD/PSB...**。原生集成可用后，选择
**File > Save As...** 并指定 `.psd` 或 `.psb` 将成为额外入口。扩展会分别创建
隔离的原始副本与扁平副本，调用内附 converter 并验证 Photoshop 文档，最后才通过
Aseprite 自定义格式的保存流写入。保存选项可以选择是否把当前帧写成
Photoshop 的 active frame（导出始终使用当前帧），以及选择通道压缩（`ZIP`、
`ZIP prediction`、`RLE` 或 `Raw`）。同一 Sprite 保存到同一路径时，后续 Ctrl+S
会复用上次成功的压缩和空像素图层选项；路径改变或扩展重载后会再次询问。明确的
Export 菜单命令始终独立询问。

Aseprite 可能在调用保存回调前就打开并截断原生目标文件。扩展会在写入前完整
验证 PSD，但无法为失败覆盖提供事务式回滚；需要保留旧文件时，请使用 Export 菜单
写入另一路径。

## 命令行

使用 Rust 1.88 或更高版本构建原生 CLI：

```text
cargo build --release --locked -p aseprite-psd
```

导出命令支持 `--compression raw|rle|zip|zip-prediction` 和
`--empty-layers include|omit`；省略压缩时保持现有的 ZIP（无 prediction）默认行为，
省略空图层选项时默认不导出空像素图层。`omit` 只移除所有帧都没有 cel 的像素图层，
部分帧暂时没有 cel 的图层仍保留隐藏占位，以维持帧结构一致。

测试、扩展打包、CI 和发布说明见[开发工作流](docs/development.md)。

只检查 PSD，不写输出文件：

```text
aseprite-psd inspect INPUT.psd
```

转换 PSD。除非指定 `--overwrite`，否则不会替换已有输出：

```text
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --overwrite
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --layer-association auto --linked-cels identical
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --layer-association roundtrip
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --layer-association auto --linked-cels identical --jitter-mode repair --jitter-kind all
```

使用由 Aseprite 另行生成的扁平快照导出 Aseprite 文档。输出后缀决定 PSD 或
PSB；除非明确指定 `--overwrite`，已有输出不会被替换：

```text
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite
aseprite-psd export INPUT.aseprite -o OUTPUT.psb --composite COMPOSITE.aseprite --report REPORT.json --overwrite
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --roundtrip-metadata off
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --empty-layers omit
```

使用 `aseprite-psd --help` 查看完整命令格式。

没有 Photoshop 时间轴时，帧来源必须明确选择：

- `--frame-source auto` 是默认值；存在真实 Photoshop 时间轴时使用时间轴，
  否则保持静态文档。
- `--frame-source static` 始终按单个静态帧导入。
- `--frame-source top-level` 将每个顶层图层或组作为一帧；名为
  `Background` 的顶层图层会在诊断中列出并共享到全部帧。该模式用于用户已经
  确认的 Procreate Animation Assist 等逐层动画 PSD；Procreate 标记本身不会
  自动启用该模式。

## 图层关联

- `--layer-association preserve` 是默认模式，保持源图层身份。
- `--layer-association roundtrip` 会精确恢复有效的 v2 帧分组元数据，旧 v1 标记使用
  自动关联，无标记文件保持原图层；损坏的 converter 元数据会返回需要恢复的状态。
  该模式拒绝自动关联专用的调参选项。
- `--layer-association auto` 默认使用 conservative，优先保留可编辑的逻辑身份。
- `--association-strategy compact` 显式选择在保持渲染结果的前提下尽量减少轨道。
- `--association-strategy conservative` 启用多语言复制家族、多轨和候选 Folder
  分析；身份不明确的图层仍保持分离。

自动关联不要求图层名称完美。即使使用 Photoshop 默认名称、懒得命名，或同一
图层在不同帧之间改了名字，只要跨帧结构、互斥关系、像素、位置、顺序和名称
能够提供足够的综合证据，solver 仍可能恢复正确关系。此时它会直接恢复稳定的
`layer × frame` 逻辑轨道，不需要用户手动重命名 PSD；证据不足时则保持图层身份
分离并报告不确定性，不会静默合并。

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
  Ubuntu/WSL2 Linux x64 环境验证。原生 PSD/PSB `File > Open` 和 `File >
  Save As...` 集成预计将在 Aseprite 1.3.18.4 中提供；进度见
  [Aseprite #6007](https://github.com/aseprite/aseprite/issues/6007)。更早版本必须使用
  扩展提供的明确 Import 和 Export 命令。
- macOS 包由手动 GitHub Actions workflow 构建，但目前还没有完成真实的
  Aseprite 运行时验证。
- 扩展会注册 PSD/PSB 自定义格式的加载与保存回调；需要配置导入策略时仍可使用
  明确的导入菜单命令。
- 转换会保留规范化图层树、RGBA8 cel、Photoshop 帧动画和受支持的图层状态。
  逻辑图层关联及部分坐标映射仍属实验功能，重要输出应在 Aseprite 中人工检查。
- PSD 的 16/32 bits-per-channel 输入可导入，但会由解析库降级为 Aseprite 的 RGBA8；
  该降级会进入 `UnsupportedColor/Degraded` 信息损失报告。32-bit 在此处指每通道位深，
  不同于 Aseprite RGBA 的 32 bits-per-pixel。
- PSD 导入会保留 slice 的名称、顺序、bounds 和静态 frame 0 key；Photoshop 专属的
  group、URL、target、message、alt text、background、outsets 和图层关联会明确记入
  `Slices/Degraded` 信息损失报告。resource 1050 的 version 6/7/8 已有规范驱动测试，
  但 version 7/8 仍缺少真实 Photoshop 样本验证。
- 已支持普通尺寸的 PSB version 2 输入。固定的 `psd-tools` `slices.psb` fixture 已通过
  Rust/TypeScript probe 对比及转换回读验证；超大尺寸和规范化模型之外的 Photoshop
  特性仍未验证。
- 导出会保留受支持的组、静态图层属性、帧时长、cel 可见性/位置/透明度、相同
  cel 复用，以及确定性的 tag 播放序列。Tilemap 使用独立扁平快照并报告为
  rasterized；无法继续编辑的 tag 名称/边界、slice、颜色配置和逐 cel Z-Index
  会进入信息损失报告。
- 仓库仅提交小型、固定且来源有记录的 PSD/PSB 测试素材；客户 artwork 和大型/私有
  文档仍不会提交。

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
