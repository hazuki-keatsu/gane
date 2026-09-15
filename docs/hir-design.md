---
version: v1.0
date: 2026-09-11
author: hazuki-keatsu
tag: hir
state: v0
---

# Gane HIR 设计

**适用范围：** Gane 唯一的后端中间表示，以及 `sema -> HIR -> codegen` 的边界。

## 1. 设计目标

Gane 只维护一个面向后端的 HIR。它把已经完成名字解析和类型检查的 Go-like 程序表示为可验证的、强类型的 SSA 控制流图。

```text
source -> parser -> sema -> V0 validation -> raw HIR -> verify/escape -> verified HIR -> LLVM IR -> AOT
                                                                                       \-> LLVM IR -> ORC JIT（后续）
```

HIR 是 AOT 和 JIT 的共同输入。它需要：

- 消除 `if`、`for`、字段和索引寻址等 V0 源码结构；
- 显式表示控制流、SSA value、内存访问、函数调用和失败路径；
- 定义 no-std 程序仍然必须具备的 ABI 和错误行为；
- 能局部、直接地 lowering 到 LLVM IR；
- 通过 verifier 和 escape check 建立 codegen 可以信任的不变量，并用类型包装阻止未验证 HIR 进入 backend。

HIR 不负责：

- 名字解析、作用域、Go named type 身份、方法集和 interface 匹配；
- GC、goroutine、channel、map、reflect 或 Go runtime 兼容；
- V0 阶段的 JIT、OSR、deoptimization 和 safepoint；
- 复制 LLVM IR 的全部能力。

## 2. Sema 与 HIR 的契约

只有不含 error diagnostic 的 sema 结果才能 lower 到 HIR。lowering 只查询 sema 提供的 definition、use、selection、type 和 constant facts，不重新进行名字查找或类型推导。

产生 raw HIR 前必须完成：

- 标识符和字段解析；
- named type 合法性及 underlying type 解析；
- 表达式类型与常量计算；
- 可赋值性、lvalue、调用和控制流合法性检查；
- 对当前语言子集之外特性的拒绝；
- V0 全局初始化限制检查；

保守的 stack-address escape check 在 raw HIR 上运行，因为此时地址、GEP、load/store 和调用传播已经显式化。只有同时通过普通 verifier 与 escape check 的 package 才能包装为 verified HIR，并交给 interpreter 或 codegen。raw HIR 只是构造期数据，不是合法的后端输入。

lowering 的公开输入必须同时包含 package AST、`AnalysisResult` 和经过验证的 `TargetSpec`。AST 提供待遍历的语法结构，`AnalysisResult` 只提供语义事实；lowering 不允许仅凭 AST 重做名字解析或类型推导。

Go named type 的身份只存在于 sema。例如 `type UserID int` 的赋值规则由 sema 处理；进入 HIR 后，它使用与 underlying type 相同的机器表示。名称和 source `AstNodeId` 可以作为调试信息保留，但不参与 HIR 类型相等性。

HIR 的源语言功能范围以 sema 实际接受的集合为准，不能在 HIR 文档里另行承诺更大的 Go 子集。V0 明确接受：

- `var` 局部声明和普通赋值；
- 条件 `for` 和无限 `for`；
- 无标签 `break`、`continue`；
- `if`、`++`/`--`、直接函数调用、单一标量返回值；
- blank assignment `_ = expression`，其中 expression 仍须被求值，其副作用不能消失。

V0 明确拒绝 `:=`、复合赋值、三子句 `for`、if initializer、带标签跳转、aggregate 返回值、零尺寸 aggregate 及第 16 节列出的特性。

### 2.1 可执行的前端拒绝清单

“进入 verified HIR 前拒绝”是测试契约，而不是文字约定。sema/V0 validation 和 raw HIR escape check 必须为下列输入建立 negative tests：

- 多返回值、aggregate 返回值和 aggregate 整体比较；
- 返回 stack-derived pointer 或把它存入 global；
- 非零 aggregate 全局初始化、非 null 全局 pointer 初始化及其他运行期全局初始化；
- send、range、switch、type switch、select、defer、go statement；
- short declaration、复合赋值、三子句 for、if initializer、带标签 branch；
- 空 struct、零长度 array，以及按值递归的无限尺寸类型；
- 所有 HIR V0 无法表示的 expression、statement、type 和 declaration。

前端还必须为 `break label`/`continue label` 和 send operand 类型建立专门反向测试，防止它们被误当成无标签 branch 或普通 expression 而静默通过。

### 2.2 Lowering 的求值义务

lowering 必须保持源语言规定的副作用顺序，尤其不能重排函数调用、赋值右值和可能 trap 的检查。V0 对同一语句内多个有副作用的子表达式采用确定性的源码从左到右顺序。

- `&&`、`||` 展开为短路 CFG，不能作为普通 `BinaryOp`；
- `++`/`--` 展开为一次地址求值、一次 load、加减一和一次 store；
- `_ = expression` 先完整求值 expression，再丢弃结果；
- 嵌套字段选择展开为一串单级 `GepField`。V0 拒绝 embedded field，因此暂不产生 promoted field selection；未来支持时仍使用 sema 给出的完整 selection path；
- null、bounds、division 和 shift 检查必须出现在对应操作之前，且不能越过较早的副作用。

## 3. TargetSpec 与布局

每个 HIR package 绑定一个目标：

```rust
pub struct TargetSpec {
    triple: String,
    cpu: String,
    features: String,
    data_layout: String,
    pointer_width: u8,
    endianness: Endianness,
}
```

这些字段对 HIR consumer 只读，不能通过 struct literal 任意构造。production driver 必须先创建 LLVM TargetMachine，再由它导出的 canonical triple、data layout、pointer width、endianness、CPU 和 features 构造 `TargetSpec`。构造器只接受 32 或 64 位 pointer。测试使用显式的 `TargetSpec::for_test_32/64` fixture，不能手写互相矛盾的字段。

codegen 收到 verified HIR 后仍须确认当前 TargetMachine 导出的 triple 和 data layout 与 package 完全一致；不一致是编译错误。由此 LLVM TargetMachine 是 target facts 的唯一来源，`TargetSpec` 只是其不可变快照，而不是第二套可独立配置的真值。

源语言 `int` 在 lowering 时按 `pointer_width` 变成 `I32` 或 `I64`。HIR 不允许未确定宽度的 `Int`。

Gane 不在 HIR crate 中重复实现完整 LLVM 布局算法。HIR 只定义一个不依赖 LLVM 的查询接口：

```rust
pub trait LayoutProvider {
    fn size_of(&self, typ: TypeId) -> Result<u64, LayoutError>;
    fn align_of(&self, typ: TypeId) -> Result<u32, LayoutError>;
    fn field_offset(&self, typ: TypeId, field: u32) -> Result<u64, LayoutError>;
}
```

production codegen 使用 LLVM TargetData 实现该接口，作为真实物理 size、alignment、field offset 和 padding 的唯一事实来源。HIR 不缓存布局结果，也不直接链接 libLLVM。

verifier 只检查类型图能否形成有限布局，不查询具体字节 offset。测试 interpreter 使用结构化 object/field/element 模型执行 `AggregateZero` 和 `AggregateCopy`，因此也不需要模拟真实字节布局。可另写交叉测试，确认 production `LayoutProvider` 与 LLVM 的布局一致。

同一份 HIR 不能跨 target 复用。cross compilation 需要针对目标重新 lowering。普通 verifier 检查 pointer width 为 32/64、V0 endianness/地址空间约束以及 triple/data layout 非空；TargetMachine 与 data layout 的一致性由创建 `TargetSpec` 的 target 层和 codegen 入口共同检查。

## 4. ID 与 package 数据模型

所有 ID 是紧凑整数索引。数值 `0` 永远表示 `INVALID`，合法 arena entry 从 `1` 开始。正常 HIR 不得包含任何 `INVALID` ID。

文档中的基础名称约定如下：

```rust
use gane_parser::token::AstNodeId;

pub type Symbol = String;

// parser 分配的语法身份；None 表示合成的 HIR 实体。
pub type SourceOrigin = Option<AstNodeId>;

pub enum Endianness { Little, Big }

// TypeId、GlobalId、FunctionId、BlockId、ValueId、StackSlotId
// 均为独立的 newtype(u32)，不能互相隐式转换。

pub struct HirType {
    pub kind: HirTypeKind,
}
```

```rust
pub struct HirPackage {
    pub target: TargetSpec,
    pub types: Vec<HirType>,
    pub globals: Vec<HirGlobal>,
    pub functions: Vec<HirFunction>,
    pub entry: FunctionId,
}

// 构造器产生该类型；字段不对 backend 直接开放。
pub struct UnverifiedHirPackage(HirPackage);

// 只能由 verify_and_check_escape 成功构造。
pub struct VerifiedHirPackage(HirPackage);

pub fn verify_and_check_escape(
    package: UnverifiedHirPackage,
) -> Result<VerifiedHirPackage, Vec<HirDiagnostic>>;

pub struct HirFunction {
    pub symbol: Symbol,
    pub signature: HirSignature,
    pub linkage: Linkage,
    pub attributes: FunctionAttributes,
    pub stack_slots: Vec<StackSlot>,
    pub values: Vec<ValueDef>,
    pub blocks: Vec<HirBlock>,
    pub entry: BlockId,
}

pub struct HirBlock {
    pub parameters: Vec<ValueId>,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}
```

函数参数由 entry block parameters 表示。`Direct` 参数对应同类型 block parameter；`IndirectByValue` 参数对应 `Ptr<parameter.typ>` block parameter，且该 pointer 指向调用语义创建的 callee-private 副本。普通 block parameters 用于 CFG 汇合和循环回边。

## 5. 类型系统

HIR V0 类型为：

```rust
pub enum HirTypeKind {
    Void,
    I1,
    I8,
    I16,
    I32,
    I64,
    Ptr { pointee: TypeId, address_space: u32 },
    Array { length: u64, element: TypeId },
    Struct { fields: Vec<TypeId> },
}
```

规则：

- `I1` 表示 bool 和条件；`CondBranch` 只接受 `I1`。
- `I8`～`I64` 只表示位宽。signedness 由 operation opcode 决定，与 LLVM integer type 一致。
- LLVM 为可寻址的 `I1` 分配目标规定的存储空间；Gane V0 不另设 `I8` bool。
- `Ptr` 保留 address space；V0 只生成 address space `0`。
- Array/struct 采用源码字段顺序，其物理布局由 `LayoutProvider` 查询。
- V0 不包含浮点类型。加入浮点时必须同时定义常量、运算、比较、NaN 和 ABI 语义。

`Void` 和各整数类型必须 canonical intern。任何能按照 sema 合法流入同一个 HIR operand position 的类型，lowering 都必须稳定映射到同一个 `TypeId`；例如两次独立出现但 sema 判为 identical 的 `[2]int` 必须共享 HIR type。两个 sema 判为不同的 named aggregate 可以保留不同 `TypeId`，即使它们布局相同；V0 不要求对来源不同的递归 aggregate 图求结构图同构。HIR operand 的类型匹配使用 `TypeId`，lowering 必须复用其 sema-type-equivalence-to-HIR-type 映射。printer 和布局缓存以 `TypeId` 为 key，允许存在来源不同但布局相同的 aggregate。

递归类型通过“先分配 ID、后填充定义”构造。类型图只允许通过 `Ptr` 形成环，例如 `struct Node { next *Node }`；array/struct 的按值环会形成无限尺寸，必须由前端拒绝。构造器在填充结束前不能暴露 raw package，printer 先声明 type ID 再打印定义，因而必须支持前向引用。

Array 和 struct 在 V0 中只作为内存对象存在，不作为普通 SSA aggregate value：

- 可以创建 stack slot/global，并访问其字段或元素；
- 不允许整体 aggregate load、普通 SSA 参数、直接返回或整体比较；
- aggregate 形参使用第 7 节定义的 `IndirectByValue`；
- aggregate 显式写零使用 `AggregateZero`，赋值使用 `AggregateCopy`；stack slot 初始零值由第 6 节统一保证。

## 6. 基础定义

```rust
pub struct StackSlot {
    pub typ: TypeId,
    pub name: Option<Symbol>,
    pub origin: SourceOrigin,
}

pub struct HirSignature {
    pub parameters: Vec<HirParameter>,
    pub results: Vec<TypeId>,
    pub calling_convention: CallingConvention,
}

pub struct HirParameter {
    pub typ: TypeId,
    pub passing: PassingMode,
}

pub enum PassingMode {
    Direct,
    IndirectByValue,
}

pub enum CallingConvention { Gane }
pub enum Linkage { Internal, Exported }

pub struct FunctionAttributes {
    pub no_return: bool,
    pub no_unwind: bool,
    pub memory: MemoryEffect,
}

pub enum MemoryEffect { Unknown, ReadOnly, ReadNone }
```

每个 `StackSlot` 在函数入口处按 `slot.typ` 具有 Gane 零值；这属于 HIR 语义，不是 lowering 的可选约定。LLVM backend 必须生成相应初始化，不能把 alloca 的未初始化内容暴露为 LLVM `undef`。backend 可以在保持该语义的前提下依赖后续优化删除被首次赋值完全覆盖的初始化。`IndirectByValue` 参数所指的 callee-private 副本由调用语义初始化，不属于 `stack_slots` 的隐式输入。

V0 不支持 variadic，且所有函数都使用 Gane calling convention。用户源码 annotation 不能直接产生会影响优化正确性的 attributes。

全局声明：

```rust
pub struct HirGlobal {
    pub symbol: Symbol,
    pub typ: TypeId,
    pub mutable: bool,
    pub initializer: GlobalInitializer,
    pub linkage: Linkage,
}

pub enum GlobalInitializer {
    Zero,
    Scalar(Constant),
}

V0 全局初始化必须能在编译期完成；不支持隐式运行期初始化函数。具体只允许：

- scalar 的 bool/integer 常量；
- null pointer；
- 任意支持类型的零值；
- array/struct 只能使用完整零值，pointer 不能初始化为 global/field address。

其余全局 initializer 即使现有 sema 能求出类型，也必须由 V0 validation 拒绝。后续若支持地址常量或 aggregate 常量，需要先扩展 `GlobalInitializer`，不能在 codegen 中特殊处理 AST。

常量、操作和 trap：

```rust
pub enum Constant {
    Bool(bool),
    Integer(u64), // 对应类型位宽下的原始 bit pattern
    Null,
}

pub enum UnaryOp { Neg, BitNot, LogicalNot }

pub enum TrapReason {
    DivisionByZero,
    NegativeShift,
    BoundsError,
    NullDereference,
    ExplicitPanic,
}
```

`Constant` 的解释由 `Const` 指令携带的 `TypeId` 决定。V0 最大整数宽度为 64；verifier 必须检查 bool、integer、null 与目标类型相容，且 integer bit pattern 不含目标位宽以外的有效位。

## 7. 调用和 aggregate 值语义

`Direct` 参数的 call operand 类型必须等于参数类型。`IndirectByValue` 只用于 array/struct：call operand 必须是 `Ptr<parameter.typ>`，但调用的语义是被调用者获得一份私有副本，修改它不能改变调用者对象。

```text
caller object --pointer--> call IndirectByValue
                              |
                              +-- semantic copy --> callee private object
```

LLVM backend 可以使用 `byval` 实现该副本，也可以生成显式 memcpy；这是 codegen 决策，不得省略语义副本。

struct/array 赋值通过以下指令表达：

```rust
AggregateCopy {
    destination: ValueId, // Ptr<typ>
    source: ValueId,      // Ptr<typ>
    typ: TypeId,
}
```

它表示完整、允许重叠的值复制，包含所有字段和数组元素；`destination == source` 是合法的 no-op。LLVM backend 可以始终 lower 为 `llvm.memmove`，或在证明不重叠时使用 `llvm.memcpy`/逐字段复制。该规则覆盖 `a = a` 和通过 pointer 产生别名的情况。

需要把一个已经存在的 aggregate 对象重新设为零值时使用：

```rust
AggregateZero {
    destination: ValueId, // Ptr<typ>
    typ: TypeId,
}
```

它把 aggregate 设为 Gane 零值，包括递归地清零整数、bool、pointer、array 和 struct 字段。只有 TargetMachine 明确保证该类型所有字段的零值都采用全零 bit pattern 时，backend 才能使用 `llvm.memset`；否则必须使用 typed zero/逐字段 store。使用 `memset` 时必须依据 production `LayoutProvider` 的实际 alloc size，不得自行计算大小。

V0 禁止 aggregate 返回值；sema 必须在进入 HIR 前诊断。未来的 `sret` 需要独立 ABI 设计，不能伪装成普通 SSA aggregate result。

## 8. SSA value 与内存

`ValueId` 是 SSA value，每个 value 恰好定义一次。`StackSlotId` 是栈帧存储对象，不是 value。对 slot 取地址才产生 pointer value：

```text
%p = stack_addr slot0
store %p, 1
%x = load %p
```

字段和数组元素先计算地址：

```text
%field_p = gep_field %struct_p, 1
%item_p  = gep_index %array_p, %index
```

HIR 不使用递归 Place/lvalue 树。

```rust
pub struct ValueDef {
    pub typ: TypeId,
    pub origin: ValueOrigin,
    pub source: SourceOrigin,
}

pub enum ValueOrigin {
    BlockParameter { block: BlockId, index: u32 },
    InstructionResult { block: BlockId, instruction: u32, index: u32 },
}

pub struct Instruction {
    pub results: Vec<ValueId>,
    pub kind: InstructionKind,
    pub source: SourceOrigin,
}
```

初版 lowering 可以把所有源码局部变量放入 stack slot。HIR mem2reg 是可选优化；LLVM backend 应保证 alloca 位于 entry block，并优先使用 LLVM mem2reg/SROA。

## 9. 指令集

```rust
pub enum InstructionKind {
    Const { value: Constant, typ: TypeId },
    Unary { op: UnaryOp, operand: ValueId },
    Binary { op: BinaryOp, left: ValueId, right: ValueId },
    Compare { predicate: ComparePredicate, left: ValueId, right: ValueId },
    IntCast { kind: IntCastKind, operand: ValueId, target: TypeId },

    StackAddr { slot: StackSlotId },
    GlobalAddr { global: GlobalId },
    GepField { base: ValueId, field: u32 },
    GepIndex { base: ValueId, index: ValueId },
    Load { pointer: ValueId },
    Store { pointer: ValueId, value: ValueId },
    AggregateZero { destination: ValueId, typ: TypeId },
    AggregateCopy { destination: ValueId, source: ValueId, typ: TypeId },

    Call { callee: Callee, arguments: Vec<ValueId> },
}

pub enum Callee {
    Function(FunctionId),
}

pub enum IntCastKind { Truncate, SignExtend, ZeroExtend }

pub enum BinaryOp {
    Add, Sub, Mul,
    SignedDiv, UnsignedDiv,
    SignedRem, UnsignedRem,
    Shl, ArithmeticShr, LogicalShr,
    BitAnd, BitOr, BitXor, BitClear,
}

pub enum ComparePredicate {
    Equal, NotEqual,
    SignedLess, SignedLessEqual, SignedGreater, SignedGreaterEqual,
    UnsignedLess, UnsignedLessEqual, UnsignedGreater, UnsignedGreaterEqual,
}
```

结果数量：

- `Const`、`Unary`、`Binary`、`Compare`、`IntCast`、地址计算和 `Load` 产生一个结果；
- `Store`、`AggregateZero` 和 `AggregateCopy` 不产生结果；
- `Call` 的 result 顺序与 signature results 顺序一致；V0 只允许零或一个标量 result。

类型规则：

- 本节所称 integer 只包括 `I8/I16/I32/I64`，不包括表示 bool 的 `I1`。
- `Unary::Neg/BitNot` 接受一个 integer 并产生同类型结果；`LogicalNot` 只接受并产生 `I1`。
- 除 shift 外，`Binary` 的两个 operand 必须是相同 integer 类型，result 也是该类型。`BitClear(a, b)` 的语义是 `a & ~b`。
- shift 的 left 是任意 integer，right 必须是规范化后的 `I64` 非负 bit pattern，result 与 left 同类型。lowering 对有符号源码 count 先生成负数 guard，再 sign/zero extend 到 `I64`；不能在证明值可表示之前 truncate count。
- `Compare::Equal/NotEqual` 接受两个相同类型的 integer、两个 `I1` 或两个相同 pointer 类型；ordering predicate 只接受两个相同类型的 integer。`Compare` 始终产生 `I1`，整数 signedness 由 predicate 明确给出。
- `IntCast` 的 source/target 都必须是 integer。`Truncate` 要求 target 更窄，`SignExtend/ZeroExtend` 要求 target 更宽；相同宽度的转换不生成指令。result 类型必须等于 `target`。
- `Load Ptr<T>` 产生 `T`，`Store` 的 value 必须为同一 `T`；V0 的 `T` 必须是标量。
- `GepField` 接受 `Ptr<Struct>`，只处理一个字段；嵌套字段路径在 lowering 时展开为多个 `GepField`。
- `GepIndex` 只接受 `Ptr<Array>`，返回 `Ptr<Element>`。`&array[i]` 使用 array 地址；V0 不支持普通 `Ptr<Element>` 的指针算术。
- `GepIndex` 的 index 统一为 pointer-width integer，其 bit pattern 按 unsigned 解释。lowering 在完成负数检查后，以 `IntCast` 规范化索引宽度，再做无符号上界比较；若 source 更宽，必须先证明值可表示，不能直接 truncate。Array length 必须能被目标 pointer-width unsigned integer 表示。
- `Call` 只允许直接调用；V0 不支持函数值和函数指针。
- entry block parameters 必须与 signature parameters 一一对应：`Direct(T)` 对应 `T`，`IndirectByValue(T)` 对应 `Ptr<T>`；entry block 不能有额外 parameter。
- 调用 `no_return` callee 不产生 result，必须是 block 的最后一条 instruction，且该 block 以 `Unreachable` 终结；`no_return` function signature 不能声明 result。
- 对可能为 null 的 pointer 执行 `Load`、`Store` 或 GEP 前，canonical lowering 必须生成 `pointer != null` 的显式分支，失败分支以 `Trap(NullDereference)` 终结。只有能够由来源证明非 null 的 `StackAddr`、`GlobalAddr` 等地址可以省略检查。

所有指令自身具有完整、无 LLVM poison/UB 的 HIR 语义，backend 不能把 canonical guard 当成正确性的唯一来源：null `Load/Store/GEP` 必须得到 `Trap(NullDereference)`，越界 `GepIndex` 必须得到 `Trap(BoundsError)`，整数危险操作遵循第 10 节。backend 可以利用支配它的显式 guard 消除重复检查；无法证明时必须生成本地防御检查。

## 10. 整数语义与显式检查

HIR 必须保持 Go V0 子集的整数语义，不能把 LLVM poison 暴露给语言层：

- 加、减、乘和左移按位宽确定性截断；默认不添加 LLVM `nsw`/`nuw`。
- integer division 向零截断。
- 除数为零时进入 `Trap(DivisionByZero)`。
- 对有符号 `MIN / -1`，结果是 `MIN`；对 `MIN % -1`，结果是 `0`。两者都不能直接发出会产生 poison 的 LLVM `sdiv/srem`。
- 源码 shift count 为负时进入 `Trap(NegativeShift)`；进入 shift instruction 的 count 已按第 9 节规范化成非负 `I64`。
- shift count 不按机器位宽取模。count 大于等于 left 位宽时，左移和逻辑右移结果为 `0`，算术右移结果按符号为 `0` 或全 `1`；只有 count 小于位宽时才直接生成 LLVM shift。

canonical lowering 把源语言检查展开为普通比较和显式 CFG。例如安全数组访问：

```text
%non_negative = cmp_sge %index, 0
condbr %non_negative, check_upper(), bounds_fail()

check_upper:
  %normalized = int_cast %index to usize
  %in_range = cmp_ult %normalized, %length
  condbr %in_range, access(), bounds_fail()

bounds_fail:
  trap BoundsError

access:
  %item = gep_index %array_ptr, %normalized
```

HIR 不设置 producer-only 的 `BoundsCheck` 或 `DivCheck` 伪指令。`GepIndex`、division、remainder 和 shift 本身仍是 total operation：即使非 canonical producer 没有生成可证明的 guard，interpreter/backend 也必须产生第 9、10 节规定的结果或 trap，不能触发宿主 UB/LLVM poison。

普通 verifier 不尝试从任意 `Compare + CondBranch` 证明后续 divisor、shift amount 或 index 的值域；那需要完整值域分析。lowering golden/conformance tests 负责确认 canonical producer 生成本节规定的 guard，interpreter 与 LLVM backend 直接实现 total instruction semantics，并以除零、`MIN/-1`、负 shift、超宽 shift、null 和越界的差分测试验证。LLVM backend 即使无法从 CFG 证明安全，也不能发出可能把合法 HIR 执行变成 LLVM poison/UB 的代码。

## 11. 控制流与 SSA

```rust
pub enum Terminator {
    Branch { target: BlockId, arguments: Vec<ValueId> },
    CondBranch {
        condition: ValueId,
        then_target: BlockId,
        then_arguments: Vec<ValueId>,
        else_target: BlockId,
        else_arguments: Vec<ValueId>,
    },
    Return { values: Vec<ValueId> },
    Trap { reason: TrapReason },
    Unreachable,
}
```

`Trap` 是 terminator。`Unreachable` 只标记按已验证语义不可到达的位置，不能替代用户可观察的失败。

V0 的 `Trap` 语义是立即异常终止当前程序，不执行恢复或用户清理逻辑。`TrapReason` 在 interpreter 和 debug build 中可用于诊断，但 release executable 不保证稳定 exit code 或保留 reason；hosted backend 可以调用内部 `__gane_trap(reason)` hook，freestanding backend 可以退化为目标 trap instruction。程序不得依赖不同 trap reason 的可观察差异。

HIR 使用 block parameter 而不是 phi：

```text
entry:
  condbr %ok, then(), else()
then:
  br join(1)
else:
  br join(2)
join(%result: i64):
  return %result
```

循环变量同样通过 header block parameter 和回边 argument 传递。

## 12. no-std、entry ABI 与未来 FFI

无标准库不代表没有 ABI。V0 规定：

- 源语言入口仍是 `func main()`，HIR 内部符号为 mangled Gane ABI `void gane.main()`；
- hosted codegen 自动生成 C ABI wrapper `i32 main()`，调用 `gane.main()` 并在正常结束后返回 `0`；
- freestanding `_start`、初始化栈和退出/停机方式属于未来 target-specific 设计；
- 没有隐式 heap allocation；`new`、`make` 和发生逃逸的局部地址由 sema 拒绝；
- panic、越界、除零等失败在 V0 进入对应 `Trap`；
- V0 没有 I/O，也不接受无函数体的函数声明。

所有 V0 调用都指向同一 package 内带函数体的 Gane ABI function。FFI 以后必须以独立的 binding 设计引入，明确链接 symbol、calling convention、平台 target、可传递类型、ownership/escape 规则和可信 ABI metadata；不能以“缺少函数体”隐式表示 extern。

栈地址的 V0 生命周期规则：

- 禁止从函数返回 stack-derived pointer；
- 禁止把 stack-derived pointer 存入 global；
- 可以在当前函数内读写和传给内部调用；内部函数同样必须满足不逃逸规则；
- stack-derived taint 必须穿过 `GepField`、`GepIndex`、stack slot 的 store/load 和函数参数传播。

这些规则由独立的保守 escape check 负责，而不是普通 HIR verifier。它在 raw HIR 构造后运行，对内部调用图计算 noescape summary；递归调用组通过不动点迭代求解。无法证明不逃逸时一律拒绝，不自动提升到 heap。普通 verifier 与 escape check 均成功后才能构造 `VerifiedHirPackage`。

verifier 只检查 HIR 中直接可见的类型和 attribute 一致性，不声称重新完成跨函数 escape analysis。escape check 必须有返回局部地址、经临时 slot 传播、存 global、内部调用传播和递归调用的专项测试。

## 13. AOT 与未来 JIT

`VerifiedHirPackage` 在构造完成后视为不可变输入。AOT baseline 与未来 JIT optimized pipeline 各自从同一份 verified HIR lowering，不共享已经被某一侧修改的 LLVM module：

```text
Verified HIR -> baseline instrumentation/passes -> LLVM module -> AOT
Verified HIR -> profile-guided passes           -> LLVM module -> ORC JIT
```

V0 不实现 JIT。未来第一个版本只做函数级替换：

- JIT-eligible call 必须经过稳定 entry stub；
- AOT 阶段禁止跨该 stub 内联；
- 非 JIT 函数仍可直接调用和内联；
- profile counter 在多线程环境使用原子更新；单线程 target 可使用普通计数；
- 替换只改变 stub 指向的实现，不改变函数 signature 和 ABI。

函数是否被源程序取地址与 JIT stub 是两件事；V0 禁止函数值，因此不存在 source-level address-taken function。

JIT eligibility 是未来 codegen policy，不是 HIR 字段；不得为了标记 eligibility 反向扩展 HIR 基础语义。该策略应以独立配置或 side table 选择需要经过 stub 的函数。

OSR、deoptimization、safepoint 和代码回收必须另立设计，不能提前改变 HIR V0。

## 14. Verifier 不变量

interpreter、codegen 和 JIT 的 API 只接受 `VerifiedHirPackage`。`verify_and_check_escape(raw_package)` 至少执行以下普通 verifier 检查，并在其后执行第 12 节的 escape check：

1. 所有 ID 有效，`0`/`INVALID` 不出现在正常 HIR。
2. TargetSpec 的 pointer width 为 32/64，triple/data layout 非空；类型的私有构造保证其来源受控，codegen 另行匹配真实 TargetMachine。
3. entry 唯一、存在，且是 Gane ABI `func main()` 对应的 `void()` 函数。
4. 每个 block 恰有一个 terminator，所有跳转目标存在。
5. branch arguments 与目标 block parameters 的数量和类型完全匹配。
6. 每个 `ValueId` 恰定义一次，`ValueDef.origin` 与真实定义位置一致。
7. 对可达 block，所有 use 被 definition 支配；同 block 的 instruction use 位于 definition 之后。
8. verifier 先计算 entry 可达性；不可达 block 不允许跨 block 使用 value，只能使用自身参数和本 block 先前定义的 value。
9. 每条 instruction 的 operand/result 数量和类型满足第 9 节完整类型矩阵。
10. constant 与 `TypeId` 相容且能在目标位宽中表示。
11. call 参数的 logical type、passing mode、结果顺序和 calling convention 与 callee signature 一致。
12. 每个 function 的 entry block parameters 与 signature 完全匹配；普通 CFG block 只由 incoming edge 定义 parameters。
13. return 的数量和类型与 signature 一致；V0 不允许 aggregate 或多结果返回。`no_return` function/call 满足第 9 节的 result、位置和 terminator 约束。
14. `StackAddr` 结果类型是 `Ptr<slot.typ>`；普通 verifier 不重复执行第 12 节的跨函数 escape analysis。所有 stack slot 具有第 6 节规定的入口零值语义。
15. `AggregateZero` 的目标以及 `AggregateCopy` 两端是相同 aggregate 类型的 pointer，且类型具有确定布局；copy 允许重叠。
16. 所有调用目标都是 package 内的 Gane ABI function；V0 不包含 extern、C ABI 或 variadic。
17. `Trap` 只作为 terminator。verifier 不承诺通过值域分析证明 canonical guard；危险指令的 total semantics 由 backend/interpreter 和第 10 节 conformance tests 保证。
18. package 不包含 sema poison type、未解析类型或不属于第 6 节白名单的 global initializer。
19. global 和 function 的最终链接 symbol 全局唯一，内部 Gane symbol 也不得发生 mangling collision。
20. `Void` 不能用于 SSA value、参数、block parameter、stack slot、global、array element 或 struct field；无返回值由空 results 表示。
21. primitive types 已 canonical intern；类型图只通过 pointer 成环，所有 array/struct 均为非零有限尺寸。verifier 不检查不同 aggregate ID 的结构图同构；sema representation 映射稳定性由 lowering tests 检查。
22. pointer 只能进行 `Equal/NotEqual` 比较；signed/unsigned ordering predicate 只接受 integer。
23. Array length 能由目标 pointer-width unsigned integer 表示。

verifier 必须包含反向测试：跨分支非法 use、错误 block argument、错误 GEP、错误 store/call、不可表示常量、symbol collision、非法递归/零尺寸类型和 `Void` value 都应被拒绝。escape violations 属于独立 escape check 的反向测试，不混入普通 verifier 测试集。

## 15. 文本格式与实现阶段

HIR 从阶段 1 起提供确定性的文本打印。相同输入和 target 必须产生稳定输出：ID 按 arena 顺序编号，block/instruction 顺序不依赖 HashMap iteration，调试名称不能影响语义。

文本格式用于：

- `--emit-hir` 调试；
- lowering golden tests；
- verifier 错误定位；
- interpreter/codegen 差分测试。

实现顺序：

1. 补齐第 2 节的 sema/V0 validation negative tests，保证无 error 的 sema 结果不含 HIR V0 无法表示的源码结构。
2. 定义 ID、类型、raw/verified package、受控 `TargetSpec` 和 builder，实现普通 verifier 与稳定 printer。
3. 实现 `sema + AST -> raw HIR`，只覆盖第 2 节的 V0 子集，建立 lowering golden tests；lowering 遇到缺失的 sema fact 必须返回 diagnostic，不能 panic 或自行推导。
4. 在 raw HIR 上实现独立 escape check，只有 verifier 与 escape check 均成功才产生 `VerifiedHirPackage`。
5. 实现最小 interpreter；先覆盖纯整数、结构化局部内存、total dangerous operations 和控制流，不要求真实物理布局。
6. 实现 host target 的 LLVM lowering、wrapper main、object 生成和链接，完成 AOT 闭环，并与 interpreter 做差分测试。
7. 增加 array/struct、`AggregateZero` 和 `AggregateCopy` 的端到端测试。
8. AOT 稳定后再评估可选 HIR pass；局部提升优先使用 LLVM mem2reg/SROA。
9. 最后添加 profile instrumentation 和函数级 JIT。

## 16. HIR V0 冻结范围

V0 支持：

- `bool`、`int`、`byte`、pointer、array、struct；
- `var` 局部/全局变量、普通赋值、blank assignment、`++`/`--`、if、条件/无限 for、无标签 break/continue；
- 常量、整数运算/比较/cast、地址计算、标量 load/store、aggregate zero/copy；
- 直接函数调用、零或一个标量返回值；
- hosted `main` wrapper；
- 无 heap、无 GC，错误直接 trap。

V0 明确不支持：

- `:=`、复合赋值、三子句 for、if initializer、range、switch、defer、带标签跳转；
- 无函数体声明、extern/FFI 和 C ABI；
- float、string、slice、map、interface、method、closure、函数值；
- aggregate SSA value、aggregate 返回和完整 C aggregate ABI；
- import/package loader、Go 标准库、reflect、panic/recover；
- goroutine、channel、select、GC 和隐式 heap allocation；
- JIT、OSR、deoptimization 和 safepoint。

V0 还拒绝空 struct、零长度 array、非零 aggregate global initializer、非 null pointer global initializer，以及不能由目标 pointer width 表示长度的 array。

每增加一种语言特性，必须先规定其值表示、复制/生命周期规则、失败行为和 ABI，再扩展 HIR。

V0 的解冻条件是：host executable 能稳定生成、链接和运行；第 2、10、14 节要求的 negative/conformance tests 完整；核心整数、控制流、内存和 aggregate 语义在 interpreter 与 LLVM backend 间通过差分测试。在此之前只允许修复规格矛盾和实现 bug，不增加新语言能力。
