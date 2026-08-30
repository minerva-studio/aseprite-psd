# psd2ase

`psd2ase` 计划提供一个独立的 Photoshop PSD → Aseprite 转换器。当前处于
**阶段六**：建立 Rust workspace、PSD 兼容探针、Photoshop 帧动画兼容层、
`NormalizedDocument` 中间模型、基础 writer 和实验性逻辑图层关联。

最终版本采用原生 Rust，目标是向用户提供不依赖额外运行时的单一可执行文件。
TypeScript `ag-psd` 只作为开发期差分 oracle，不会进入发布 binary。

## 当前状态

- `psd2ase --version` 和 `psd2ase --help` 已可用。
- `psd2ase inspect INPUT.psd` 只读取 PSD，不写输出文件。
- `psd2ase convert INPUT.psd [-o OUTPUT] [--overwrite]` 已可生成经过回读验证的
  实验性 `.aseprite` 输出。
- `psd2ase convert INPUT.psd --layer-association auto` 可实验性地把跨帧源图层
  关联为长期逻辑轨道；默认仍保留 PSD 源图层树。
- auto 模式默认使用稳定轨道顺序，不写 cel `z_index`；只有显式使用
  `--z-order auto` 才启用实验性的逐 cel z-order，且该选项必须配合
  `--layer-association auto`。
- Stable 默认使用跨帧实际重叠像素的顺序共识；可使用
  `--stable-order anchor` 回退到旧的锚点顺序，或使用
  `--stable-order strict` 在顺序证据无法确定时直接失败。
- auto 关联使用版本化的多语言复制后缀词表，识别 `Copy`、`拷贝`、`副本`、
  `コピー`、`복사` 等名称家族；复制后缀只作为弱证据，同帧重复名称不会强制合并，
  模糊关联会保留独立轨道并在报告中列出原名、基础名和候选证据。
- 仓库不提交 PSD 或 PSB 样本。
- 阶段四已将递归图层树、图层属性、独立 RGBA8 像素所有权、帧顺序、时长、
  循环策略和逐层状态统一到 `psd2ase-core::normalize` 的中间模型；静态 PSD
  表示为无时长的单帧；阶段五 writer 使用 100ms 序列化默认值，并把
  `pixels.left/top` 作为 provisional cel 原点。

## 构建

```text
cargo fmt --all -- --check
cargo test --workspace
cargo run -p psd2ase -- --version
```

解析和写入依赖在 crates.io 上的实际包名分别是 `ag-psd` 与 `aseprite-io`；上游仓库
和许可证记录见 `THIRD_PARTY_LICENSES.md`。

## 阶段门槛

启用转换写入前，必须使用提供的真实 PSD 做差分探针，比较画布元数据、完整图层树、
图层属性、每个图层的像素哈希以及 Photoshop 动画元数据。没有通过这一步，
不会声称已经完成转换器。
