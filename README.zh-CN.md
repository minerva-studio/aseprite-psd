# psd2ase

`psd2ase` 计划提供一个独立的 Photoshop PSD → Aseprite 转换器。当前处于
**阶段四**：建立 Rust workspace、只读 PSD 兼容探针、Photoshop 帧动画兼容层和
`NormalizedDocument` 中间模型。

最终版本采用原生 Rust，目标是向用户提供不依赖额外运行时的单一可执行文件。
TypeScript `ag-psd` 只作为开发期差分 oracle，不会进入发布 binary。

## 当前状态

- `psd2ase --version` 和 `psd2ase --help` 已可用。
- `psd2ase inspect INPUT.psd` 只读取 PSD，不写输出文件。
- `psd2ase convert INPUT.psd` 在 parser 兼容性探针和 Aseprite writer 验证通过前
  有意保持关闭。
- 仓库不提交 PSD 或 PSB 样本。
- 阶段四已将递归图层树、图层属性、独立 RGBA8 像素所有权、帧顺序、时长、
  循环策略和逐层状态统一到 `psd2ase-core::normalize` 的中间模型；静态 PSD
  表示为无时长的单帧；Aseprite writer 仍未启用。

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
