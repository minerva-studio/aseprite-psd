# 选项参考

[English](options.md) · [用户手册](user-guide.zh-CN.md) · [README](../README.zh-CN.md)

先在手册中选择流程，再查具体设置。这里描述当前源码中的控件，安装包可能落后于源码。

## 界面选项速查

| 控件 | 用途 / CLI 对应 |
| --- | --- |
| Frame source | Automatic → `auto`；Photoshop timeline → `timeline`（要求帧动画数据）；Static document → `static`；Layer hierarchy → `layer-depth:N` |
| Frame layer depth | 仅用于层级模式：顶层为 0，直接子层级为 1 |
| Layer association | Preserve layers → `preserve`；不用 metadata 的 Automatic association → `auto` |
| Use metadata | 选择 `roundtrip` 路径，停用高级关联控件 |
| Preserve Photoshop metadata | `--preserve-photoshop-metadata`；与转换器关系恢复是不同设置 |
| Association strategy | conservative 优先保留可编辑身份；compact 优先减少轨道；Feature tracks → `feature`，按跨帧特征关系组织轨道 |
| Z-order | stable 使用稳定轨道顺序；auto 允许实验性逐 cel Z-Index |
| Stable order | consensus 使用跨帧重叠共识；anchor 使用锚点帧；strict 在顺序证据不足时拒绝转换 |
| Uncertain layers | conservative 专用；group 生成候选 Folder，flat 不生成 |
| Link identical cels | 仅用于不用 metadata 的自动关联；对应 `--linked-cels identical` |
| Jitter repair | 界面提供 off / report / repair；CLI 另有 assist。repair 会改变像素 |
| Export empty pixel layers | 勾选 → `--empty-layers include`，否则 omit |
| Content reuse | Frame folders → none；Reuse Linked Cels only → linked；Merge identical content → aggressive。后两者为实验性；编辑共享内容可能影响多个帧 |

## 命令行

使用 Rust 1.88 或更高版本构建原生 CLI：

```text
cargo build --release --locked -p aseprite-psd
```

导出命令支持 `--compression raw|rle|zip|zip-prediction` 和 `--empty-layers include|omit`；省略压缩时默认使用 Photoshop 兼容的 RLE。 ZIP 模式仍可用于诊断，但不属于 Photoshop 兼容目标。省略空图层选项时默认不导出 空像素图层。`omit` 按帧过滤没有 cel、cel 不透明度为 0，或 RGBA 像素 alpha 全为 0 的像素层，并递归裁掉空组；仅因隐藏但仍有非透明像素的图层会保留。`include` 保留 完整的空/透明状态布局。

版本更新记录见 [Changelog](../CHANGELOG.md)；测试、扩展打包、CI 和发布说明见 [开发工作流](development.md)。

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

使用由 Aseprite 另行生成的扁平快照导出 Aseprite 文档。输出后缀决定 PSD 或 PSB；除非明确指定 `--overwrite`，已有输出不会被替换：

```text
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite
aseprite-psd export INPUT.aseprite -o OUTPUT.psb --composite COMPOSITE.aseprite --report REPORT.json --overwrite
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --roundtrip-metadata off
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --empty-layers omit
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --content-reuse linked
```

使用 `aseprite-psd --help` 查看完整命令格式。

动画导出支持 `--content-reuse none|linked|aggressive`。`none` 为每个时间点 保留独立的物理帧文件夹；`linked` 只在源 Aseprite cel 明确共享链接目标且完整 显示状态一致时复用；`aggressive` 还会复用像素和显示属性完全相同的独立状态。 时间轴帧数、顺序和播放设置不会缩短；两种复用模式均为实验性功能。

没有 Photoshop 时间轴时，帧来源必须明确选择：

- `--frame-source auto` 是默认值；存在真实 Photoshop 时间轴时使用时间轴，
否则保持静态文档。
- `--frame-source static` 始终按单个静态帧导入。
- `--frame-source top-level` 将每个顶层图层或组作为一帧；名为
`Background` 的顶层图层会在诊断中列出并共享到全部帧。该模式用于用户已经 确认的 Procreate Animation Assist 等逐层动画 PSD；Procreate 标记本身不会 自动启用该模式。

## 图层关联

- `--layer-association preserve` 是默认模式，保持源图层身份。
- `--layer-association roundtrip` 会精确恢复有效的 v2 帧分组元数据，旧 v1 标记使用
自动关联，无标记文件保持原图层；损坏的 converter 元数据会返回需要恢复的状态。 该模式拒绝自动关联专用的调参选项。
- `--layer-association auto` 默认使用 conservative，优先保留可编辑的逻辑身份。
- `--association-strategy compact` 显式选择在保持渲染结果的前提下尽量减少轨道。
- `--association-strategy conservative` 启用多语言复制家族、多轨和候选 Folder
分析；身份不明确的图层仍保持分离。

自动关联不要求图层名称完美。即使使用 Photoshop 默认名称、懒得命名，或同一 图层在不同帧之间改了名字，只要跨帧结构、互斥关系、像素、位置、顺序和名称 能够提供足够的综合证据，solver 仍可能恢复正确关系。此时它会直接恢复稳定的 `layer × frame` 逻辑轨道，不需要用户手动重命名 PSD；证据不足时则保持图层身份 分离并报告不确定性，不会静默合并。

- 稳定轨道顺序默认使用跨帧重叠共识。使用 `--stable-order anchor` 可改用锚点帧
顺序，使用 `strict` 可在证据无法确定时拒绝转换。
- `--z-order auto` 启用实验性的逐 cel Z-Index，并且必须配合自动关联。
conservative 模式还可使用 `--uncertain-layers flat` 禁用候选 Folder。

`--linked-cels identical` 会在自动关联生成的同一个输出图层内无损复用完全 相同的 RGBA 像素缓冲；位置、透明度和逐 cel Z-Index 仍按帧保留。默认值为 `off`，只有尺寸和字节都完全一致时才会建立链接。它要求同时使用 `--layer-association auto`，因为 `preserve` 会独立输出每个源图层，没有跨图层 cel 复用候选。

## 导入防抖

防抖默认关闭。`--jitter-mode report` 只报告疑似问题，`assist` 只把稳定化 结果作为自动图层关联的证据，`repair` 才会改变导出的 cel。可用 `--jitter-kind alpha|color|all` 选择低 Alpha 孤立杂点或同一逻辑轨道内的 轻微颜色差异，并用 `--jitter-profile conservative|balanced` 选择阈值预设。 颜色防抖只在自动关联已经确认的同一轨道、同尺寸和同位置 cel 之间进行；修复 时选择真实代表 cel，不合成新颜色。高级阈值可通过 `--jitter-alpha-threshold`、`--jitter-max-speck-area`、 `--jitter-max-changed-ratio` 和 `--jitter-max-channel-delta` 覆盖。
