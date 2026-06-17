# omni-meta fuzz harness

cargo-fuzz（libfuzzer）鲁棒性套件。验证 §5 不变量：任意字节下永不 panic /
不超 Limits / 不死循环；并跨四适配器校验**提取一致性**。需 nightly 工具链。

本目录自成独立 workspace（`Cargo.toml` 内空 `[workspace]`），与主 stable/`no_std`
workspace 隔离。

## 前置
- `rustup toolchain install nightly`
- `cargo install cargo-fuzz`

## target
| target | 入口 | 性质 |
|---|---|---|
| `differential` | 公共 API 四适配器 oracle | `unified`/`raw`/`format` 逐字段一致，或全 Err |
| `read_slice_bounded` | `read_slice` + 计数分配器 | 不越分配上界；容器标签 ≤ max_tags |
| `isobmff` | `__fuzzing::drive_bmff` | BMFF 走盒不 panic/有界 |
| `ebml` | `__fuzzing::drive_ebml` | EBML 走元素不 panic/有界 |
| `exif` | `__fuzzing::decode_exif` | TIFF/IFD codec 有界 |
| `xmp` | `__fuzzing::decode_xmp` | XMP 扫描有界 |

全部 target 经 `FuzzAlloc` 全局分配器守护分配上界（`ALLOC_CEILING`）。

### differential 的一致性口径（重要）
`differential` 比较四适配器（slice / blocking / seek / push）的输出：**`unified`/`raw`/
`format` 必须逐字段严格相等**——这是真正的契约（不论顺序读、可 seek 还是 push，提取出的
元数据一致）。`warnings` **不**参与跨适配器比较：它们是 best-effort 诊断，其确切集合合法地
取决于各适配器执行模型——前向只读流式 vs 全缓冲随机访问在「数据边界处如何报告不完整」上有
本质差异（后向 seek 到已弃字节、EOF 空窗口 vs 部分元素等）。warning 的正确性由各
codec/format 单测分别保证。详见 `omni-meta-fixtures` 的 `metadata_agree`。

## 跑法
```bash
cargo +nightly run --bin seeds          # 生成种子语料（首次/更新 fixtures 后）
cargo +nightly fuzz run differential    # 跑某 target
cargo +nightly fuzz run differential -- -max_total_time=60   # 限时
```

## 复现与最小化
```bash
cargo +nightly fuzz run <target> artifacts/<target>/<crash-file>   # 复现
cargo +nightly fuzz tmin <target> artifacts/<target>/<crash-file>  # 最小化输入
cargo +nightly fuzz cmin <target>                                  # 最小化语料
```

## 已暴露并修复的缺陷（首轮）
- `read_seek`：skip 越过 EOF 时原生 seek 无声越尾、误判 Truncated → 按文件实长夹断。
- 驱动：前向 Skip/SeekTo 越尾的警告 KIND 在 slice 与流式间不一致 → 统一为 Truncated。
- 驱动：近 `u64::MAX` 的 skip 致越尾 offset 计算溢出 panic → 全部改 `saturating_add`。

## CI 接入点（尚未接线）
生产硬化支柱 2（CI）将以 `cargo +nightly fuzz run <t> -- -runs=N -max_total_time=T`
做短时冒烟。本目录已就绪。
