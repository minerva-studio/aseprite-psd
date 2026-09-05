# 用户手册

[English](user-guide.md) · [README](../README.zh-CN.md) · [选项参考](options.zh-CN.md)

先按 PSD 的组织方式选择流程，再展开具体操作。自动关联仍属实验功能，下面是推荐起点，导入后请检查播放和图层关系。

## 你的 PSD 是怎么组织的？

| 结构与目的 | 推荐流程 |
| --- | --- |
| 普通分层插画，希望保留原图层和组 | [分层插画](#illustration) |
| Photoshop 帧动画时间轴切换不同图层的显示状态 | [时间轴逐帧动画](#timeline) |
| 每个图层或文件夹代表一帧 | [逐层或逐组动画](#hierarchy) |
| 本扩展导出的 PSD，希望恢复 Aseprite 图层关系 | [重新导入](#roundtrip) |

尚未安装？先看 [README 快速开始](../README.zh-CN.md)。

<a id="illustration"></a>

## 分层插画

**怎么辨认：** 图层和文件夹表示画面部件，没有逐帧播放的意图。

**推荐方式：** 按静态文档导入，保留源图层身份。得到单帧的 Aseprite 文档，受支持的图层与组可继续编辑。文件夹多不代表动画帧多。

<details>
<summary>如何设置</summary>

- `Frame source` → `Static document`。
- `Layer association` → `Preserve layers`，取消 `Use metadata`。
- `Jitter repair` 保持 `Off`。

</details>

如果图层实际代表不同时间点，请使用动画流程。

<a id="timeline"></a>

## 时间轴逐帧动画

**怎么辨认：** Photoshop 帧动画时间轴决定每一帧的显示状态。同一角色或部件的不同姿势可能放在不同图层或组中，通过切换可见性播放。

**推荐方式：** 从时间轴读取帧，再尝试将跨帧对应的内容整理成特征轨道，保持稳定的叠放关系。这适合希望在 Aseprite 中按部件继续编辑的文档。

<details>
<summary>如何设置：整理跨帧部件</summary>

- `Frame source` → `Photoshop timeline`；不需要设置 `Frame layer depth`。
- `Layer association` → `Automatic association`。
- 取消 `Use metadata`，将 `Association strategy` 设为 `Feature tracks`。
- `Z-order` → `stable`，`Stable order` → `consensus`。
- `Preserve Photoshop metadata` 和 `Link identical cels` 暂不勾选，`Jitter repair` 保持 `Off`。
- `Uncertain layers` 在此策略下不可用，无需调整。

这是此类组织方式的推荐尝试，不保证每个文件都能恢复预期的逻辑图层关系。

</details>

**检查结果：** 帧数、速度、部件所属轨道和遮挡关系是否符合原动画。如果部件会跨帧改变前后关系，可尝试逐帧叠放变化，见[选项参考](options.zh-CN.md)。关联不符合编辑意图时，可尝试保守关联或保留源图层作为对照。没有真实帧动画时间轴时，使用下面的流程。

<a id="hierarchy"></a>

## 逐层或逐组动画

**怎么辨认：** 图层树表示帧顺序，例如每个顶层图层是一张完整画面，或动作文件夹包含逐帧子组。仅凭名称不能确认文件夹是不是一帧。

**推荐方式：** 明确指定图层树中哪一层代表帧，再检查生成的序列，避免把身体部件当成时间点。自动帧来源不会仅因有多个组就生成动画。

<details>
<summary>如何设置</summary>

- `Frame source` → `Layer hierarchy`。
- `Frame layer depth` 选择代表帧的层级：顶层为 `0`，直接子层级为 `1`。
- 初次检查可选 `Preserve layers` 并取消 `Use metadata`，先确认帧来源；需要整理跨帧部件时再尝试自动关联。
- `Jitter repair` 保持 `Off`。

</details>

如果时间轴才是播放依据，改用时间轴流程。混合了时间轴控制内容的结构需要核对实际输出，不能仅按子文件夹数量推断帧数。

<a id="roundtrip"></a>

## 重新导入本扩展的 PSD

**怎么辨认：** 文件由本扩展导出，并保留了转换器写入的图层／帧关系信息。

**推荐方式：** 优先用保存的关系恢复图层，减少重新推断。它依赖元数据仍有效，不代表所有 Photoshop 特性都能无损往返。

<details>
<summary>如何设置</summary>

- `Frame source` → `Automatic`。
- `Layer association` → `Automatic association`，勾选 `Use metadata`。
- 需要自行选择关联策略时，取消 `Use metadata`。
- 出现 `PSD Metadata Recovery` 时，可选择自动关联、保留图层或取消；恢复后检查结果。

</details>

无标记或旧标记文件会走回退流程，见下面的详细行为说明。`Preserve Photoshop metadata` 是另一项设置，不代替 `Use metadata`。

## 检查结果与保存

1. 播放动画，检查帧数、时长、缺失内容和遮挡关系。
2. 展开图层树，确认需要独立编辑的部件仍然独立。
3. 查看信息损失提示中的路径和帧；使用 `Export Full Report...` 保存完整报告。
4. 按 Ctrl+S 或使用 Save As 保存 `.aseprite` 工作文件。

## 常见问题

| 现象 | 下一步 |
| --- | --- |
| 只有一帧 | 确认源文件有帧动画时间轴；逐层动画需要明确选择层级 |
| 关联选项呈灰色 | 自动关联下取消 Use metadata；部分控件仅用于特定策略 |
| 图层整理不符合预期 | 尝试保守关联，或保留源图层作对照 |
| 遮挡关系不对 | 检查源动画是否跨帧改变前后关系，再考虑逐帧 Z-order |
| 想减少重复 cel | 确认关联后再启用 linked cels；编辑链接内容可能影响其他帧 |
| 怀疑杂点或颜色闪动 | 先使用防抖报告，再决定是否修复；修复会改变像素 |

## 导出与文件保存

使用 **File > Export > Export PSD/PSB...** 选择目标，保留 `.aseprite` 作为工作文件。空图层、内容复用和高级设置见[选项参考](options.zh-CN.md)。当前源码中的实验选项不等于已发布版本的兼容性承诺；版本限制见下文。

<details>
<summary>导入、导出和元数据的详细行为</summary>

明确的 Import 命令会打开已修改、且未与转换临时文件关联的文档。原生 集成可用后，`File > Open` 会返回与原始 PSD 关联的文档。对于明确导入的文档， 可按 Ctrl+S 或使用 Save As 选择最终 `.aseprite` 路径；Aseprite 会默认建议 PSD 所在目录和同名文件。

明确的 Import 命令，以及原生集成可用后的 `File > Open`，都会显示同一套导入选项。 用户可选择 `Automatic association` 或 `Preserve layers`。在 Automatic association 下， 勾选 `Use metadata` 会使用精确的 metadata preset；取消后会启用下方 association 调参并 使用普通启发式路径。旧 v1 标记和无标记文件使用自动关联回退；converter 元数据损坏时 会进入恢复选择，不会被静默忽略。特别地，没有 metadata 的 PSD 不会被当作 `Preserve layers`，而是有意回退到标准的 Automatic association。需要为这类文件调整 association strategy 时，请取消 `Use metadata`。

导出默认会写入不可见、带版本的 PSD metadata。它只记录元数据版本、 逻辑图层 ID 和物化 cel 关系，不包含文件路径、用户名、设备信息或使用追踪； Photoshop 等其他读取器可以忽略这段信息。通过 **File > Export > PSD/PSB Support Settings...** 可以分别控制导出时写入 metadata，以及导入时是否使用 metadata。 关闭导出标记后 PSD 仍可正常读写，但后续打开无法使用本转换器的精确图层关联。

原生集成可用后，在 `File > Open` 中取消导入对话框会明确报告 `PSD opening cancelled by user.`，不会伪装成失败的 Sprite 或半初始化文档。

当前导出请使用 **File > Export > Export PSD/PSB...**。原生集成可用后，选择 **File > Save As...** 并指定 `.psd` 或 `.psb` 将成为额外入口。扩展会分别创建 隔离的原始副本与扁平副本，调用内附 converter 并验证 Photoshop 文档，最后才通过 Aseprite 自定义格式的保存流写入。保存选项可以选择是否包含空像素图层，后续 Ctrl+S 会复用已选格式和该选项。扩展导出会自动使用 Photoshop 兼容的 RLE 通道 压缩。0.3.1 暂不支持把 Aseprite 时间线导出为 Photoshop 时间线；受支持的导出 契约是静态分层 PSD/PSB 文档。

Aseprite 可能在调用保存回调前就打开并截断原生目标文件。扩展会在写入前完整 验证 PSD，但无法为失败覆盖提供事务式回滚；需要保留旧文件时，请使用 Export 菜单 写入另一路径。

</details>

## 当前边界

- 导入扩展包已在 Aseprite 1.3.18.3、Windows x64 和使用 glibc 的
Ubuntu/WSL2 Linux x64 环境验证。原生 PSD/PSB `File > Open` 和 `File > Save As...` 集成预计将在 Aseprite 1.3.18.4 中提供。更早版本必须使用扩展提供的 明确 Import 和 Export 命令。
- macOS 包由手动 GitHub Actions workflow 构建，但目前还没有完成真实的
Aseprite 运行时验证。
- 扩展会注册 PSD/PSB 自定义格式的加载与保存回调；需要配置导入策略时仍可使用
明确的导入菜单命令。
- 转换会保留规范化图层树、RGBA8 cel、Photoshop 帧动画和受支持的图层状态。
逻辑图层关联及部分坐标映射仍属实验功能，重要输出应在 Aseprite 中人工检查。
- PSD 的 16/32 bits-per-channel 输入可导入，但会由解析库降级为 Aseprite 的 RGBA8；
该降级会进入 `UnsupportedColor/Degraded` 信息损失报告。32-bit 在此处指每通道位深， 不同于 Aseprite RGBA 的 32 bits-per-pixel。
- PSD 导入会保留 slice 的名称、顺序、bounds 和静态 frame 0 key；Photoshop 专属的
group、URL、target、message、alt text、background、outsets 和图层关联会明确记入 `Slices/Degraded` 信息损失报告。resource 1050 的 version 6/7/8 已有规范驱动测试， 但 version 7/8 仍缺少真实 Photoshop 样本验证。
- 支持 PSB 输入。固定的 `psd-tools` `slices.psb` fixture 已通过 Rust/TypeScript
probe 对比及转换回读验证；超大画布仍受 Aseprite 可表达尺寸限制，接近 Photoshop 上限的文件性能尚未验证。
- 导出会保留受支持的组和静态图层属性。0.3.1 暂不支持把 Aseprite 时间线导出为
Photoshop 时间线。Tilemap 使用独立扁平快照并报告为 rasterized；无法继续编辑的 tag 名称/边界、slice、颜色配置和逐 cel Z-Index 会进入信息损失报告。
- 仓库仅提交小型、固定且来源有记录的 PSD/PSB 测试素材；客户 artwork 和大型/私有
文档仍不会提交。
