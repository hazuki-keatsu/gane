---
version: v1.2
date: 2026-09-17
author: hazuki-keatsu
tag: codegen
state: v0
---

# Gane LLVM Codegen 设计

**适用范围：** `gane_codegen` 将 `VerifiedIrPackage` lowering 为当前宿主目标的、通过 LLVM verifier 的 LLVM IR 文本。

## 1. 目标与边界

第一阶段只完成下面的闭环：

```text
VerifiedIrPackage
    -> host LLVM TargetMachine / TargetData
    -> LLVM Module
    -> LLVM verifier
    -> .ll text
```

codegen 必须保持 [IR 设计](./ir-design.md) 已定义的整数、控制流、内存、调用和 trap 语义。它不能重新进行名字解析、类型推导或 escape analysis，也不能接收未经验证的 IR。

本阶段不实现 object emission、链接、优化 pipeline、JIT、extern/FFI、debug info 和 cross compilation。它们在 `.ll` 生成稳定后分别设计，不能提前扩展 IR 或公共 API。

## 2. LLVM 工具链

使用 Inkwell 作为 LLVM C API 的 Rust 封装。Inkwell 不携带 LLVM；构建环境仍须安装匹配版本的 `libLLVM` 和 `llvm-config`。第一阶段固定：

- Inkwell `0.9.0`；
- LLVM `22.1.x`；
- Inkwell feature `llvm22-1`；
- 只初始化当前宿主 target。

首版保留 Inkwell 默认的 target feature 集合；运行时只调用 native target 初始化。若未来为了缩小依赖关闭默认 feature，必须先确认每个支持 host 架构都显式启用了对应 target feature，不能把 Cargo feature 裁剪与 cross compilation 混为一谈。

`crates/codegen/Cargo.toml` 增加：

```toml
[dependencies]
gane_ir = { path = "../ir" }
inkwell = { version = "0.9.0", features = ["llvm22-1"] }
```

macOS 示例：

```sh
brew install llvm@22
LLVM_SYS_221_PREFIX="$(brew --prefix llvm@22)" cargo test --workspace
```

Linux 和 Windows 使用各自的 LLVM 22 安装方式，并把 `LLVM_SYS_221_PREFIX` 指向包含 `bin/llvm-config`、headers 和 libraries 的 LLVM prefix。Inkwell feature、`llvm-config` major version 和 `libLLVM` major version 必须一致。Rust 编译器内部使用的 LLVM 和只有 `clang`、没有 `llvm-config` 的 Xcode toolchain 都不能替代该依赖。

## 3. 公共 API

codegen crate 遵循仓库约定：公共类型放在 `src/interface.rs`，`lib.rs` 只声明私有模块并 `pub use interface::*`。

```rust
pub struct LlvmBackend {
    // private: TargetMachine, TargetData, canonical TargetSpec
}

impl LlvmBackend {
    pub fn for_host() -> Result<Self, CodegenError>;

    pub fn target_spec(&self) -> &TargetSpec;

    pub fn emit_llvm_ir(
        &self,
        package: &VerifiedIrPackage,
    ) -> Result<String, CodegenError>;
}

pub enum CodegenError {
    TargetInitialization(String),
    TargetMismatch {
        package: TargetSpec,
        backend: TargetSpec,
    },
    Lowering(String),
    InvalidModule(String),
}
```

不公开 Inkwell 的 `Context`、`Module`、type 或 value。它们的 lifetime 只存在于一次 `emit_llvm_ir` 调用内；验证成功后返回 LLVM IR 文本。

`LlvmBackend::for_host`：

1. 初始化 native LLVM target；
2. 通过 LLVM host query 读取默认 triple、host CPU 和 host features，再创建 TargetMachine；
3. 创建 `TargetMachine`，使用 `OptimizationLevel::None`、默认 relocation mode 和默认 code model；
4. 从 `TargetMachine` 的 `TargetData` 读取 data layout、pointer width 和 endianness；
5. 构造 canonical `TargetSpec`。

`emit_llvm_ir` 必须先比较 `package.target()` 和 backend 的完整 `TargetSpec`。任何字段不一致都返回 `TargetMismatch`，不能尝试修补或重写 package target。

## 4. 实现阶段与 lowering 顺序

私有实现保持最小边界：

```text
interface.rs  public API and errors
target.rs     host TargetMachine and TargetSpec construction
lower.rs      Verified IR -> LLVM Module
```

一次 module lowering 按以下顺序执行：

1. 创建固定名为 `gane` 的 LLVM `Context` 和 `Module`，设置 target triple 与 data layout；不设置输入文件相关的 module identifier 或 `source_filename`；
2. 建立 `TypeId -> LLVM type` cache；
3. 预声明全部 struct、globals、functions、`__gane_trap`、`llvm.trap` 与目标宽度的 `llvm.memmove`；
4. 填充 struct body 和 global initializer；
5. lowering 每个 function body；
6. 定义 hosted `main` 和已声明的 trap helper；
7. 调用 LLVM module verifier；
8. 返回 `module.print_to_string()` 的文本。

所有 arena 都按 ID 顺序遍历，使相同输入和相同 target 的输出稳定。首版不运行 LLVM optimization pass。

实现按可验证的垂直切片交付，而不是按单条 instruction 分割：

1. **工具链与 target smoke test：** 接入 Inkwell，创建 host `TargetMachine`，生成并验证只含 `i32 main() { return 0; }` 的 module。
2. **声明骨架：** 先在 `gane_ir` verifier 加入“entry block 不能是 branch target”的反向测试和拒绝规则；再实现类型 cache、递归 struct 声明、globals、function declaration、预声明 helper/intrinsic、hosted wrapper 和 target mismatch。此阶段 function body 只支持空 return。
3. **普通 SSA/CFG：** 实现常量、wrapping add/sub/mul、bitwise、compare、cast、call、return、branch 和 block parameter 到 phi 的映射；不实现会引发 LLVM poison/UB 的运算和内存访问。
4. **安全 legalization：** 先实现 trap helper、guard block 和“实际 LLVM tail block”记账，再加入 div/rem、shift 及它们的 total semantics。
5. **标量内存：** 在已有 guard 基础上实现 stack/global address、入口零初始化、load/store、field/index GEP、null 和 bounds semantics。
6. **aggregate：** 以 `TargetData` 实现 typed zero 和 `memmove`；不做 alias analysis、`memcpy` 或 `memset` 优化。
7. **driver 与行为测试：** driver 写出 `.ll`，并通过测试专用的 `lli` 子进程将 interpreter 与 LLVM IR 的正常完成/trap 行为做差分。

每阶段都必须通过 LLVM module verifier；只有第 7 阶段完成后，危险操作的语义才视为验收完成。

## 5. Target 与类型 lowering

类型映射如下：

| Gane IR | LLVM IR |
|---|---|
| `Void` | `void`，只用于函数返回 |
| `I1` / `I8` / `I16` / `I32` / `I64` | 同宽 integer type |
| `Ptr { address_space, .. }` | 对应 address space 的 opaque pointer |
| `Array { length, element }` | `[length x element]` |
| `Struct { fields }` | 按 `TypeId` 命名的 LLVM struct |

先为所有 struct 创建 opaque named struct，再填充 body，从而支持仅通过 pointer 成环的递归类型。LLVM pointer 使用 opaque pointer；IR 的 pointee `TypeId` 仍用于选择 load/store 类型和 GEP source element type。

物理 size、ABI alignment、field offset 和 padding 只能查询 LLVM `TargetData`。codegen 不复制 LLVM layout 算法，也不在 IR 中缓存布局结果。alloca、load 与 store 使用其访问类型的 ABI alignment；不手写平台常量或由 Rust 类型猜测 alignment。

## 6. Globals、functions 与 hosted entry

先声明所有 symbols，再生成定义，保证函数和 global 可以前向引用。

- Gane globals 和 functions 使用 internal linkage；
- `GlobalInitializer::Zero` 使用目标类型的 LLVM typed zero；
- scalar initializer 根据目标类型生成 integer、bool 或 null pointer constant；
- V0 中 `IrGlobal::mutable` 没有经 verifier 保证的不可写语义；所有 Gane global 都生成 LLVM `global`，不得生成 LLVM `constant`；
- 函数只接受 verifier 已保证的 scalar 参数和零或一个 scalar result；
- `FunctionAttributes::no_return` 映射为 LLVM `noreturn` attribute；
- call 只能指向当前 package 内预声明的函数。

backend 为每个已定义的 Gane function 与 hosted wrapper 写入 `target-cpu` 和 `target-features` string attributes，值来自 canonical `TargetSpec`。这使 `.ll` 保留创建 TargetMachine 时的 CPU/features，而不仅用它们做 mismatch 比较。

hosted codegen 生成：

```llvm
define i32 @main() {
entry:
  call void @gane.main()
  ret i32 0
}
```

若 `gane.main` 带 `no_return`，wrapper 在 call 后生成 `unreachable`，而不是不可达的 `ret i32 0`。codegen 拒绝 package symbol 与 `main`、`__gane_trap` 或 `llvm.*` 保留名称冲突。第一阶段不为这些 backend-only 保留名称修改 IR verifier。

## 7. Function、CFG 与 block parameters

每个函数维护三组映射：

```text
BlockId     -> LLVM BasicBlock
ValueId     -> LLVM BasicValue
StackSlotId -> alloca pointer
```

每个函数先创建 synthetic LLVM `prologue` block。它负责：

- 为全部 stack slots 创建 `alloca`；
- 对每个 slot store typed zero，满足 IR 的入口零初始化语义；
- 跳转到 IR entry block。

每个 IR block 预先创建对应的 LLVM block。IR 的 entry block 没有 incoming edge；它的 parameters 直接映射为 LLVM function parameters。只有非 entry block parameters 预先创建 phi：

- `Branch` 和 `CondBranch` 按 arguments 给目标 phi 添加 incoming；
- loop backedge 与普通 edge 使用同一规则。

synthetic prologue 是 LLVM function 的物理 entry block；它只做 allocas、零初始化并跳转到 IR entry block。IR terminator 不得跳回 function entry，因此不需要为 entry parameter 构造 phi。

这项映射的正确性依赖 [IR 设计](./ir-design.md) §14 的 entry-target verifier 规则；该 verifier 修改必须先于 backend 实现落地。

一条 IR 指令可能为了 trap guard 展开出多个 LLVM blocks。向目标 phi 添加 incoming 时，必须使用 source IR block lowering 完成后的实际 LLVM tail block。

## 8. 普通指令 lowering

| IR instruction | LLVM lowering |
|---|---|
| `Const` | integer、bool 或 null constant |
| `Neg` | `sub 0, value` |
| `BitNot` | `xor value, -1` |
| `LogicalNot` | `icmp eq value, 0` |
| `Add` / `Sub` / `Mul` | 无 flags 的整数运算 |
| bitwise operations | `and` / `or` / `xor`；BitClear 为 `left & ~right` |
| `Compare` | 对应 signed/unsigned `icmp`；pointer 只允许 `eq/ne` |
| `IntCast` | `trunc` / `sext` / `zext` |
| `StackAddr` / `GlobalAddr` | 查询预声明地址 |
| `Load` / `Store` | null guard 后的 typed load/store |
| `GepField` | null guard 后的 `getelementptr struct, ptr base, iPTR 0, i32 field` |
| `GepIndex` | null/bounds guard 后的 `getelementptr [N x T], ptr base, iPTR 0, iPTR index` |
| `Call` | 直接调用预声明函数，绑定零或一个 result |
| `Return` | `ret void` 或单值 `ret` |
| `Unreachable` | LLVM `unreachable` |

加、减、乘和左移不得添加 `nsw`、`nuw`。GEP 不添加 `inbounds`，除非未来 IR 能提供 backend 可直接信任的相应证明。

表中的 `iPTR` 是 target pointer width 的 `i32` 或 `i64`。`GepIndex` 的 source element type 是完整 array，因此必须有第一个零 index 进入 array object、第二个 index 选择 element；只传一个 index 会按 array 的大小步进，语义错误。

## 9. LLVM safety legalization、trap 与 total semantics

LLVM 的部分 instruction 对合法 Gane IR 输入会产生 UB 或 poison。codegen 必须在危险 instruction 本身实现 IR 的 total semantics，不能依赖 canonical frontend 已经生成 guard，也不能只依赖 LLVM verifier。所有 guard 都先 branch 到 continuation 或 trap block；可能 poison 的 value 不得流入 branch condition、load/store pointer、call argument/callee、return 或 GEP index。V0 不做值域分析、支配性 guard 识别或 guard elimination：即使 canonical lowering 已经有检查，backend 仍为每个危险 IR instruction 发出本地防御检查；重复 guard 是有意的正确性优先策略。

| LLVM 危险点 | V0 codegen 规则 |
|---|---|
| `add/sub/mul/shl` 的 `nsw`/`nuw` | 一律不添加 flag，保持 modulo bit-width 语义 |
| `sdiv/srem` 的零除或 `MIN / -1` | 先 guard 零除；用安全 divisor 和 `select` 避免 LLVM overflow operand |
| `udiv/urem` 的零除 | 先 guard 零除 |
| shift amount 为负或不小于 bit width | 先 guard negative；用安全 amount 执行实际 shift，再选择 IR 规定的 wide-shift 结果 |
| `exact` division/shift | 一律不添加 `exact` |
| `getelementptr inbounds` | 一律不添加 `inbounds`；deref 前保持 null/bounds guard |
| `undef` / `poison` | 不生成它们，也不以可能 poison 的中间值作为 observable operand |
| byte-wise zeroing | 首版使用 typed zero store，让 LLVM 表示类型化的 integer zero 与 pointer null；不自行假设字节表示，也不改写为 `memset` |
| potentially overlapping copy | 始终使用 `llvm.memmove`，不使用 `memcpy` |

`select` 只能选择已先行合法化的值；它不是把一个已用于 observable position 的 poison value 变回安全值的手段。

module 内生成一个 helper：

```llvm
define internal void @__gane_trap(i32 %reason) cold noreturn {
  call void @llvm.trap()
  unreachable
}
```

五种 `TrapReason` 映射为内部固定整数。reason 只保留调试价值，不承诺稳定的进程退出码。显式 `Trap` terminator 和动态 guard 都调用该 helper；`Unreachable` 仍直接生成 LLVM `unreachable`。

在 lowering 任一 function body 前，module 已有以下 intrinsic declaration，并已创建带 internal linkage、但稍后才填充 body 的 `__gane_trap` function value：

```llvm
declare void @llvm.trap()
declare void @llvm.memmove.p0.p0.iPTR(ptr, ptr, iPTR, i1)
```

其中 `iPTR` 是 target pointer width 的 `i32` 或 `i64`。在 body lowering 前创建 `__gane_trap` 保证 guard 可直接 call；最终 module 中它是调用 `llvm.trap` 后 `unreachable` 的 definition，而非单独的 internal declaration。V0 只产生 address space 0，因此 memmove 名称固定为 `p0.p0`；未来扩展地址空间时必须按 intrinsic overload 规则另行设计。

### 9.1 Division 与 remainder

- divisor 为零时进入 `DivisionByZero` trap；
- unsigned div/rem 在通过零检查后直接生成；
- signed `MIN / -1` 的 div 结果为 `MIN`，rem 结果为 `0`；
- 使用 `select` 把特殊情况的 divisor 替换为安全值，再执行 LLVM `sdiv/srem`，最后选择规定结果；LLVM 指令本身永远不能接收到 overflow operands。

### 9.2 Shift

- IR verifier 规定 shift count 是 `I64`，而 LLVM shift 的两个 operands 必须同宽；因此 count 的范围检查必须先在 `I64` 上完成，不能先 truncate；
- 把 `I64` count 按 signed integer 检查，小于零进入 `NegativeShift` trap；
- 令 `in_range = icmp ult i64 count, bit_width`，再令 `safe_count_i64 = select in_range, count, 0`；
- 把 `safe_count_i64` truncate 到 left operand 的位宽，得到 LLVM `shl/lshr/ashr` 使用的 count；
- `in_range == false` 时，`shl/lshr` 选择零；`ashr` 按 left 符号选择零或全 1；
- 这样既保留超宽 shift 的 IR 结果，也保证 LLVM shift 从不接收超宽或负 count。

### 9.3 Pointer 与 bounds

- `GepField`、`GepIndex`、load、store 和 aggregate 操作在使用 pointer 前检查 null；
- null 进入 `NullDereference` trap；
- `GepIndex` 用 unsigned `index < length` 检查 bounds，失败进入 `BoundsError`；
- 通过检查后生成非 `inbounds` GEP。

## 10. Aggregate memory operations

`AggregateZero` 在 null guard 后对 destination 做一次 typed `zeroinitializer` store。stack aggregate 的入口零初始化使用相同方式。第一阶段不使用 `memset`。

`AggregateCopy`：

1. 分别检查 source 和 destination 非 null；
2. 用 LLVM `TargetData` 查询 aggregate alloc size 和 ABI alignment；
3. size constant 使用 target pointer width；
4. 始终生成 `llvm.memmove`。

`memmove` 保证重叠复制正确，并允许 `destination == source`。首版不尝试 alias analysis，也不改写为 `memcpy` 或逐字段复制。

## 11. Driver 集成

driver 在 parse 和 sema 成功后创建 `LlvmBackend`，再用 `backend.target_spec().clone()` 调用 `lower_package`，替换 interpreter-only 阶段使用的 `TargetSpec::for_test_64()`。

完整 pipeline 为：

```text
parse
-> sema
-> create host LlvmBackend
-> lower with backend TargetSpec
-> verify_and_check_escape
-> write <input>.ir.txt
-> emit and verify LLVM IR
-> write <input>.ll
-> interpret VerifiedIrPackage
```

codegen error 使用现有 compiler-stage 约定退出 1；输出文件失败退出 2。driver 继续运行 interpreter，但不执行 `.ll`，不调用 linker。

driver 使用真实 host `TargetSpec` 后，`.ir.txt` 中的 triple/data layout 不再等同于 `TargetSpec::for_test_64()`；跨 host 的 driver 测试不得对整份 target-dependent dump 做 golden 比较。

## 12. 测试与验收

codegen tests 使用手写 verified IR 覆盖：

- host `TargetSpec` 构造和 target mismatch；
- IR verifier 拒绝跳转到 function entry block；
- primitive、pointer、array、struct 和 pointer-recursive struct；
- zero/scalar globals；所有 global 即使 `mutable == false` 也不得生成 LLVM `constant`；
- void/单 scalar result functions、calls 和 hosted `main`；
- `gane.main` 为 `no_return` 时，hosted wrapper 在 call 后生成 `unreachable`；
- if/join phi、loop backedge、entry parameters 和 stack zero initialization；
- guarded then/backedge 到 join/header phi 的 incoming 必须来自 guard continuation block；相同 target 的两个 `CondBranch` edge 也必须各自写入 incoming；
- wrapping arithmetic、signed/unsigned compare 和 integer casts；
- load/store、field/index GEP、aggregate zero 和 overlapping aggregate copy；
- division by zero、signed `MIN / -1`、negative/wide shift、bounds、null、显式 trap 和 unreachable。

每个成功测试都调用 LLVM `Module::verify()`。文本断言只检查关键 opcode、guard、linkage 和 attribute，不保存容易随 LLVM 小版本改变的整份 `.ll` golden；这是对仓库“source-to-IR golden”习惯的有意偏离，因为 LLVM 小版本会改变合法文本格式。module verifier 只证明 LLVM IR 合法，不能证明 Gane 的 total semantics。

危险操作测试还必须调用 LLVM 22 附带的 `lli` 子进程：测试程序在结果不符合预期时显式 trap，正常程序应成功结束，预期 trap 的程序应异常结束。测试通过 `LLVM_SYS_221_PREFIX/bin/lli`（Windows 为 `lli.exe`）定位该工具；这是测试 harness，不是产品 JIT 或 object/link 功能。每个案例都由同一 host `LlvmBackend::target_spec()` 构造 verified package，并同时运行 interpreter，比较两者的正常完成或 trap 结果；不得使用 `TargetSpec::for_test_64()` 作为 codegen 成功测试的 target。

driver integration test 从 source 完整执行到 codegen 和 interpreter，并确认 `.ir.txt`、`.ll`、target triple、data layout、`gane.main` 和 hosted `main` 存在。

验收命令：

```sh
LLVM_SYS_221_PREFIX="$(brew --prefix llvm@22)" cargo test -p gane_codegen
LLVM_SYS_221_PREFIX="$(brew --prefix llvm@22)" cargo test -p gane_driver
LLVM_SYS_221_PREFIX="$(brew --prefix llvm@22)" cargo test --workspace
cargo fmt --check
```

完成实现和验证后更新 `PROGRESS.md`。object emission、链接和 interpreter/AOT 差分执行属于下一里程碑。
