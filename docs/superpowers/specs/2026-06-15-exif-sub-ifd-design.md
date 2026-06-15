# EXIF Sub-IFD 支持 — 设计

**日期:** 2026-06-15
**状态:** 已批准设计,待实现计划
**范围:** `omni-meta-core` 的 EXIF codec(`codecs/exif.rs`)、数据模型(`model.rs`)、上界(`limits.rs`),以及 `normalize.rs` 的一处必要防护。

## 背景与动机

当前 `codecs/exif.rs` 仅解析 IFD0,且只解 ASCII(type 2)与 SHORT cnt==1(type 3)两种值。它**不跟随**任何指针:

- Exif sub-IFD(tag `0x8769`)—— 几乎所有拍摄参数(DateTimeOriginal、ExposureTime、FNumber、ISO、FocalLength、LensModel)都在此,IFD0 没有。
- GPS sub-IFD(tag `0x8825`)—— 经纬度/海拔/时间戳,地理位置的唯一来源。
- Interop sub-IFD(tag `0xA005`)。
- next-IFD 指针 → IFD1(缩略图 IFD)。

`ExifTag.ifd: u8` 字段早已存在但恒为 `0`,模型从一开始就为多 IFD 预留了位置。

动机覆盖:**拍摄参数、GPS 定位、原始标签完整性、缩略图 IFD** 全部纳入。

## 范围决策(关键边界)

**数据只走到 `raw.exif`,并扩展 `Value` 类型。** 即:跟随指针 + 让原始标签真正携带数据(RATIONAL/LONG/数组),但**不新增 `Unified` 字段**、**不动 `normalize` 的投影逻辑**(除一处防护见下)。理由:GPS / 拍摄参数目前是单一格式来源(只有 EXIF 提供),违反既有的"每个 Unified 字段需 ≥2 种格式来源才纳入"规则;留待 XMP 等第二来源就绪后另立计划。

**非目标:**
- 不新增 `Unified` 字段(无 gps/datetime/exposure 等)。
- 不解析 MakerNote 内部结构(仅作为 `Bytes` 原样保留,受字节上界约束)。
- 不解压、不跟随缩略图像素数据。

## 设计

### 1. 数据模型(`model.rs`)

**`ifd` 身份 → 枚举。** 把 `ExifTag.ifd: u8` 改为闭合枚举,记录来源:

```rust
pub enum IfdKind { Primary, Thumbnail, Exif, Gps, Interop }
//                 IFD0     IFD1       0x8769 0x8825 0xA005
```

风格上与既有 `Orientation` / `Field` 枚举一致,且让下游防护可读(`t.ifd == IfdKind::Primary`)。

**`Value` 扩展为 sub-IFD 实际用到的类型:**

```rust
pub enum Value {
    U16(u16),            // SHORT, cnt==1            (既有)
    U32(u32),            // LONG,  cnt==1            (新)
    Text(String),        // ASCII                    (既有)
    Rational(u32, u32),  // RATIONAL  num/den        (新)
    SRational(i32, i32), // SRATIONAL                (新)
    Bytes(Vec<u8>),      // BYTE / UNDEFINED         (新:MakerNote、GPSVersionID)
    List(Vec<Value>),    // 任意类型 cnt>1           (新:GPS lat = 3×Rational)
}
```

`List` 控制变体数量:SHORT/LONG/RATIONAL **数组**统一表示为 `List([scalar, …])`,而非每类型一个新变体。ASCII 始终是单个 `Text`;BYTE/UNDEFINED 始终是单个 `Bytes`,与 count 无关。

### 2. `normalize.rs` 的一处必要防护(非新功能)

IFD1 出现后,Orientation 这类 tag 可能**同时**存在于 IFD0 与缩略图 IFD。`normalize` 目前仅按 tag 号匹配,会误收缩略图标签。把其 EXIF 循环限制为 `t.ifd == IfdKind::Primary` 即可。**这不新增任何 `Unified` 字段,只防止新 IFD 泄漏进既有字段。**

### 3. 遍历引擎(`codecs/exif.rs`)

新的 `decode` 先读 TIFF 头(不变),再运行**扁平工作队列**代替单次 `parse_ifd`:

```rust
struct Pending { off: usize, ifd: IfdKind }

let mut queue = vec![Pending { off: ifd0, ifd: Primary }];
let mut visited: Vec<usize> = Vec::new();   // N 很小,线性扫描足矣
let mut ifd_count = 0usize;

while let Some(p) = queue.pop() {
    if ifd_count >= limits.max_ifds { break; }
    if visited.contains(&p.off) { continue; }   // 环 / 重复防护
    visited.push(p.off);
    ifd_count += 1;
    parse_entries(p, &mut queue, out, …);       // max_tags 跨所有 IFD 共享
}
```

**`parse_entries`** 走一个 IFD 的条目(既有循环,泛化):

- **sub-IFD 指针 tag** —— `0x8769`→`Exif`、`0x8825`→`Gps`(仅在 `Primary` 内承认)、`0xA005`→`Interop`(仅在 `Exif` 内承认):读其 LONG 偏移并**入队**。指针条目本身是**结构性**的,**不**作为 `ExifTag` 发出(偏移对消费者无意义)。
- **其他 tag**:走泛化值读取器,push `ExifTag { ifd: p.ifd, tag, value }`。
- 条目走完后读 next-IFD 偏移。**仅 `Primary` 跟随**它 → 入队为 `Thumbnail`(IFD1)。sub-IFD 忽略其 next 指针(即只有 IFD0→IFD1,不追深链)。

### 4. 泛化值读取器

按 EXIF type 计算 `unit_size`(BYTE/ASCII/UNDEFINED=1,SHORT=2,LONG/SLONG=4,RATIONAL/SRATIONAL=8;未知 type → 跳过该 tag),再 `total = cnt * unit_size`(checked 乘法)。

- `total ≤ 4` → 从 `valoff` 内联读;否则从偏移读。**两路都对 `tiff` 做边界检查**,且 `total ≤ max_payload_bytes`。
- `cnt==1` → 标量变体;`cnt>1` → `List`(ASCII→单个 `Text`,BYTE/UNDEFINED→单个 `Bytes`)。
- 任何失败 → 丢弃该 tag,绝不 panic(与现行契约一致)。

### 5. 上界(`limits.rs`)

新增一个字段 `max_ifds`(默认 ~16)。其含义是**被解析的 IFD 总数**(扁平计数,每 pop 并解析一个 IFD 即 +1),**不是嵌套深度、也不单指 next-IFD 链长度**——扁平工作队列无递归栈,故无"深度"轴。它把 Primary + Exif + Gps + Interop + IFD1 + (IFD1 自身的 sub-IFD…) 的**总个数**封顶,无论某 IFD 经 sub-IFD 指针还是 next 指针到达都各算 1。典型正常文件该值 <10。既有 `max_tags` 现在跨**所有** IFD 统计(`out.len() >= max_tags` 检查不变,只是覆盖更多)。安全姿态:

- 环在 `visited` 处终止 → 无死循环。
- IFD 总数受 `max_ifds` 限制。
- 单值字节受 `max_payload_bytes` 限制;`cnt * unit_size` 用 checked 乘法防溢出。
- 全程无无界递归。

## 测试

参照既有 `exif.rs` 单测风格 + 仓库的差分测试框架。

**解码正确性(新 fixture):**
- IFD0 → Exif sub-IFD(`0x8769`)含 ASCII `DateTimeOriginal` 与 RATIONAL `FNumber`;断言二者 `ifd == Exif` 且 `Value::Rational(num, den)` 正确。
- GPS sub-IFD(`0x8825`)含 3×RATIONAL 纬度 → 断言 `Value::List([Rational, Rational, Rational])`、`ifd == Gps`。
- 经 next 指针的 IFD1 → tag 落得 `ifd == Thumbnail`。

**值读取器:** `LONG`→`U32`、`SRATIONAL`→`SRational`、`UNDEFINED`→`Bytes`、SHORT 数组→`List`、未知 type → 丢弃。

**硬化(安全主线):**
- **环:** IFD0 的 Exif 指针指回 IFD0(以及 A↔B 双 IFD 环)→ 终止、不挂起、不 panic(`visited` 防护)。
- `max_ifds` 与 `max_tags` 跨 IFD 强制生效。
- 越界 sub-IFD 指针 / next 指针 → 丢弃,不 panic。
- sub-IFD 偏移与头部重叠 / 荒谬 `cnt`(`cnt * unit_size` 溢出)→ checked 乘法丢弃。

**回归防护:** Orientation 同时存在于 IFD0 与 IFD1 → `Unified.orientation` 只反映 IFD0(验证 `normalize` 的 `Primary` 防护)。

**差分:** 扩展既有 4 适配器差分测试,使 sub-IFD 标签在 slice/push/blocking/seek 下表现一致。

## 受影响文件

- `omni-meta-core/src/model.rs` —— `IfdKind` 枚举;`Value` 新增变体;`ExifTag.ifd` 类型变更。
- `omni-meta-core/src/codecs/exif.rs` —— 工作队列遍历、泛化值读取器、指针 tag 处理。
- `omni-meta-core/src/limits.rs` —— 新增 `max_ifds`。
- `omni-meta-core/src/normalize.rs` —— `Primary` 防护(一行)及测试构造体更新。
- 既有测试中所有 `ExifTag { ifd: 0, … }` 构造点 → 改用 `IfdKind::Primary`。

## 决策记录

1. **枚举优于 u8** 表示 IFD 身份 —— 与代码库风格一致,防护可读。
2. **单个 `List` 变体**承载所有数组,而非每类型一个变体 —— 控制变体爆炸。
3. **指针 tag 不作为数据发出** —— 偏移对消费者无意义,属结构性。
4. **仅 Primary 跟随 next-IFD 链** —— 只取 IFD0→IFD1,不追深缩略图链。
5. **扁平工作队列(非递归)** —— 最强 DoS 姿态:有界平坦循环,环在 `visited` 处死亡,总量受 `max_ifds` + `max_tags` 限制。
6. **数据止于 raw,不动 Unified** —— 守住"≥2 来源"规则。
