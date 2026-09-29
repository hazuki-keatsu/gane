---
version: v1.3
date: 2026-09-26
author: hazuki-keatsu
tag: ir
state: v0
---

# 加强 IR 逃逸检测的实施计划

本计划以当前 `InstructionKind` / `Terminator` 为覆盖范围，面向所有通过普通 verifier 的 IR，包括手写 IR，不依赖 sema 或 canonical lowering 的生成习惯。

相关基线：[IR 设计](ir-design.md)第 7、9、12、14 节，以及 `crates/ir/src/escape.rs`、`verify.rs`、`ir.rs`。

## 1. 目标与范围

建立可独立验证的性质：任何通过逃逸检查的 package，都不能通过当前 IR 操作让函数本次调用的局部对象地址被保存在该调用之外。允许同步调用期间使用借入地址；不自动提升到 heap。

采用“对象来源 + points-to 集合 + 内存内容传播 + 函数效果摘要”的有限数据流分析。生命周期作为分析侧表中的来源信息，不进入 `IrTypeKind`，不改变 LLVM ABI。

首版接受保守误报，但不能把无法分析的路径当作安全。覆盖全部现有 opcode 不等于精确判定所有安全程序；后者不作为验收承诺。

保留现有调用边界策略：callee 返回借入指针，也视为参数逃逸；调用方不能向这样的参数传入栈地址。即使调用方随即丢弃结果，仍拒绝。允许 `identity(&local)` 返回后在 caller 内使用，是后续精度/契约扩展，不在本次实现中暗中放开。

无需实现独占借用、move、析构或完整 borrow checker。生命周期约束的含义是“来源对象必须覆盖使用期间”，并不意味着函数返回的所有指针都必须是全局生命周期：返回 caller 借入指针理论上可以安全，只是首版仍采用上述较严格策略。

## 2. 现有实现的缺口与反例

当前实现通过 `Taint.values` 和 `Taint.slots` 标记污染，`storage()` 仅识别 `StackAddr`、`GlobalAddr` 及其 GEP 链。地址经过 `Load`、block parameter 或 `Call` 后，`storage()` 无法识别原对象。

这至少影响两类路径：

1. 从被污染 pointer slot 的别名加载指针时，`Load` 可能丢失污染。
2. 从 aggregate 别名复制其内容，或在 callee 中读取参数指向的 pointer/aggregate 内容时，可能丢失嵌套的栈地址。

必须区分“源对象在栈上”和“源对象的内容包含栈地址”。复制栈上的整数数组到 global 是安全的；不能仅因为 `AggregateCopy.source` 带有 stack-derived 标记便报错。先前讨论中仅复制一个未存入指针的栈 aggregate，不足以构成逃逸反例。

以下是需要用 `IrBuilder` 构造的回归用例。文本沿用现有 printer 格式，省略 target 和未使用的 primitive 声明；实际测试必须提供完整类型和 value 定义：

```text
type !6 = i64
type !7 = ptr(addrspace=0, !6)
type !8 = struct {!7}
type !9 = ptr(addrspace=0, !8)
global @1 "gane.saved": !8 = zero

func @1 "gane.main"() -> () [no_return=false] entry ^1 {
  slot $1: !6
  slot $2: !8
  ^1():
    %1 = stack_addr $1
    %2 = stack_addr $2
    %3 = gep_field %2, 0
    store %3, %1
    br ^2(%2)
  ^2(%4: !9):
    %5 = global_addr @1
    aggregate_copy %5, %4, !8
    return
}
entry @1
```

此时 `$2` 内确实含有 `$1` 的地址。当前 `storage(%4)` 是 `Unknown`，检查源 slot 的路径会失效。新分析必须证明 `%4` 和 `%2` 指向同一抽象对象，继而发现复制到 global 的内容包含 `$1`。

第二个反例是 `publish(p **int)` 执行 `global = *p`。caller 把 `&local` 存入 pointer slot，再把该 slot 地址传入 `publish`。摘要不能只追踪参数值本身，必须包含参数可达内存中的指针。

## 3. 有限抽象域

### 3.1 对象、指针值和内容分别建模

每个函数分析使用以下有限节点：

- `Local(slot)`：本次函数调用中的栈对象；在跨函数索引中附带 `FunctionId`。
- `Borrowed(i)`：第 i 个 pointer 参数，以及从其可达内存读出的外部来源。它是调用边界的符号来源，不代表非栈或全局对象。
- `Static`：global 对象的保守合并节点，包括其子对象。
- `Unknown`：无法建立来源的 top；不等于空集合或 null。

维护两个不同的映射：

```text
P[value]  = 该 pointer SSA value 可能指向的对象集合
M[object] = 该对象内部可能保存的 pointer 所指向的对象集合
```

例如 `p = &x; q = &p`，分别有 `P(q) = {Local(p)}` 和 `M(Local(p)) = {Local(x)}`。不能用一个 stack-derived bool 合并二者。

首版按整个根对象合并字段和数组元素。GEP 保留根对象，pointer 字段加载读取根对象的内容集合。数组不按长度展开，也不按运行时 index 创建节点；递归 pointer 类型不递归展开节点。这会产生字段/元素间误报，但抽象域有限。

`Borrowed`、`Static` 之间可能别名：caller 的两个参数可能指向同一对象，参数也可能指向 global。首版将所有外部对象视图共享一个 `ExternalMemory` 内容集合，避免假定参数互不别名。初值包含所有 pointer 参数的 `Borrowed(i)` 和 `Static`；从外部内存间接读出的 pointer 因而保留可能依赖的所有输入来源。它可以覆盖任意多级解引用，但可能把读取某个参数的效果归因给其他参数。

`Local` 与本次调用前已存在的外部内存不混合。递归调用时，上层调用的局部对象属于下层的 `Borrowed`，不能因为两层具有相同 `FunctionId` / `StackSlotId` 就认为它们是同一个活跃栈帧。

### 3.2 内容闭包与未知值

定义 `Reach(S)` 为从对象集合 S 沿 `M` 边得到的自反传递闭包；用 visited 集合处理自引用和环。

- 传递 pointer / 返回 pointer：检查 `Reach(P(value))`，包含指针自身指向的对象。
- 复制 aggregate 的内容：起点是 `union M(source_object)`，不能把 source object 自身的地址直接算作被复制内容。
- 保持全局对象图的不变量：`Reach({Static})` 中不得出现 `Local`。

空集合在不动点中表示“尚无非 null 指向事实”，不能在迭代中途据此签发安全结果。合法、零初始化内存最终没有指针来源可以是空集合。无法支持的来源必须传播 `Unknown`，遇到与逃逸有关的读写、调用、return 时拒绝证明；不能因为集合查询失败就返回 noescape。

## 4. 每个 IR 操作的转移规则

所有集合仅做并集，即 weak update。首版不实现覆盖删除、路径条件求解或 strong update。

| 操作 | 分析规则 |
|---|---|
| `Const(Null)` | 没有非 null 指向来源 |
| bool/integer constant、`Unary`、`Binary`、`IntCast` | 不产生 pointer 事实；合法类型由普通 verifier 保证 |
| `Compare` | 结果是 bool，不把 operand 的来源传播到结果 |
| `StackAddr` | `P(result) += Local(slot)` |
| `GlobalAddr` | `P(result) += Static` |
| `GepField` / `GepIndex` | `P(result) += P(base)`；保守合并子对象 |
| pointer `Load` | 对每个 `o ∈ P(pointer)`，`P(result) += M(o)`；外部对象读共享内容 |
| integer/bool `Load` | 不把对象污染变成整数来源 |
| pointer `Store` | 对每个目标对象，`M(target) += P(value)`；保留该写操作的检查位置 |
| integer/bool `Store` | 不增加 pointer 边；不清除已有边 |
| `AggregateCopy` | 对所有可能的源/目标对象，`M(dst) += M(src)`；只在按值包含 pointer 的类型上传播内容 |
| `AggregateZero` | 首版保留已有 may-pointer 边；初始零值仍为空，不凭清零证明逃逸安全 |
| `Call` | 实例化第 5 节摘要；传播返回来源并更新 caller 可达内存 |
| `Branch` / `CondBranch` | 实参 `P` 并入对应 block parameter；两个条件分支均处理 |
| `Return` | 记录返回指针及其内容闭包中的来源，供摘要与诊断使用 |
| `Trap` / `Unreachable` | 不产生返回来源或后继边 |

`contains_pointer(typ)` 只递归 array/struct 的按值成员，遇到 `Ptr` 立即为 true；无需追入 pointee。源 aggregate 无 pointer 字段时，即使根对象其他部分被合并污染，也跳过此次内容复制的 pointer 传播。

`AggregateCopy` 允许重叠与自复制。采用只增不减的内容并集，不存在“先清目标导致源信息丢失”；即使合并对象使自复制含义变宽，也只能增加误报。

分析每个函数 entry 可达的 CFG；不做条件常量剪枝。package 中每个函数都要分析，包括未被 main 调用的函数。不可达块仍由普通 verifier 检查其结构，不作为可执行逃逸路径。

## 5. 函数摘要与调用效果

首版摘要包含以下有限事实，均由函数体计算，不能信任调用方提供的 annotation：

```text
captures[i]       输入 i 的地址或可达内容可能到达 return / 外部存储 / 捕获调用
returns_from[i]   pointer 返回值可能依赖输入 i 的地址或可达内容
returns_static   pointer 返回值可能来自 global
writes_external  函数可能修改外部可达的 pointer 内容
invalid_local    本函数局部地址可能离开本次调用
unknown_effect   无法建立可靠的效果上界
```

`captures` 包含 `returns_from`。两个字段分别服务于调用是否合法与返回值来源传播，不能在“不允许栈参数被返回”的策略下遗漏 return capture。

### 5.1 构建摘要

pointer entry parameter i 的种子是 `Borrowed(i)`。执行函数内转移规则，反复迭代到不动点：

- 将值/内容写到 `Borrowed` 或 `Static`，视为向本次调用之外保存；其内容闭包中的 `Borrowed(i)` 标记 capture，`Local` 标记本函数非法。
- 返回值的闭包同样标记 capture、返回依赖或非法局部来源。
- 向 callee 的捕获参数传参，根据实参闭包映射到当前函数的来源。
- callee 效果和本地外部 pointer 写入共同更新 `writes_external`。

这样可以描述 `global = *param`，而无需为任意深度构造无限的 `Param.deref.deref...` 路径。共享外部内存的保守归因必须明确记录在代码注释和测试中。

### 5.2 在 caller 中应用摘要

对实参 i 计算 `Ai = Reach(P(argument_i))`：

1. `captures[i]` 为 true 且 Ai 含 caller 的 `Local`：拒绝调用；Ai 中的借入来源继续传播到 caller 自身摘要。
2. pointer 返回值来源并入 `returns_from` 对应的 Ai；`returns_static` 则增加 `Static`。`invalid_local` 的 callee 导致整个 package 拒绝，不能把其返回值当作静态地址。
3. 若 `writes_external`，对所有实参闭包中可访问的内存对象以及外部内存做保守 weak update：将捕获参数对应的实参闭包与 `Static` 并入其可能 pointer 内容。未标记 capture 的输入来源不能被 callee 保存到调用外，因此不应仅因它是实参就加入写回来源。目标集合、capture 位与源集合都必须随固定点增长重新处理。
4. `unknown_effect` 不能默认为 noescape；在当前只允许内部直接调用的 IR 下，正常实现不应需要依赖此状态才能完成证明。

第 3 步是明确的 havoc 上界，可能假设 callee 写入了实际未写入的指针。callee 无法合法地把自己的局部地址写回 caller；该情况由 callee 的 `invalid_local` 独立拒绝。因此调用写回的来源上界可以限定为被捕获的输入可达来源与 global。若某个输入只因 return 被标记 capture，仍把它纳入写回上界，允许这类精度损失。

只更新 capture 而忽略内存效果会漏掉“callee 修改了 pointer slot，caller 后续 load”的路径。首版选择上述保守效果，不同时实现完整的逐字段参数到参数赋值摘要。若后续误报过多，再将 `writes_external` 细分为目标参数与内容来源关系。

### 5.3 递归与收敛

全包摘要从空事实开始；反复分析所有函数，将新摘要事实与旧值做并集，直到没有变化。每次函数分析也必须在当前摘要下求得本地不动点。只有整个包稳定后才能作出最终通过判定和输出诊断。

对象节点数、SSA value 数和摘要位数均有限，集合只增长，因此必然终止。递归对象使用有限根节点和内容环；递归调用使用借入来源替换，不展开调用栈。

初版可复用当前全扫描方式，用标准库集合实现。禁止以固定扫描次数后“默认通过”作为超时策略；若未来设置资源上限，耗尽必须返回明确的分析失败。SCC/worklist 优化仅在实测需要时引入，且必须保持同一不动点结果。

## 6. 生命周期检查与诊断

实际判断发生在来源闭包与逃逸位置相遇时：

| 位置 | 约束 |
|---|---|
| 本函数局部 slot | 可以保存本函数局部地址或借入地址；内容继续跟踪 |
| global / 参数可达外部内存 | 不能保存本次调用的 `Local`，借入来源形成 capture 摘要 |
| return | 不能返回本次调用的 `Local`；借入来源形成 return/capture 摘要 |
| 内部调用 | 依据 callee 摘要检查实参的直接来源和可达内容 |

存入 global 后再覆盖/清零也拒绝，符合当前保守的“禁止向外保存栈地址”规则。内容分析必须反复检查 sink，避免先扫描 sink、后发现内容边时漏报。

推导规则只实现一份；摘要收集和最终诊断消费同一组 sink 事实，替换当前 `instructions_escape()` / `report_escapes()` 的重复判断。输出沿用 `IrDiagnostic`，至少指出函数、block、instruction/terminator、逃逸类别、源 slot 或参数索引；先不新增公共诊断框架。

诊断排序和去重固定。可为首次加入的事实记录一个前驱，输出“stack slot → store → load/branch → copy/call → sink”的短证据链；不用枚举所有路径。证据链是解释，不替代保守集合判定。

## 7. 文件边界与重写策略

允许整体替换 `escape.rs` 中的 `Taint`、`storage()`、`propagate()` 和 bool-only 摘要求解。保留 `check()` 接口及 `verify_and_check_escape()` 的调用顺序。

- 优先在私有 `escape` 模块完成对象域、规则和求解器；测试过大时移至 `escape/tests.rs`。
- 复用现有强类型 ID、`IrBuilder`、printer 与 `IrDiagnostic`。内部节点 key 必须区分对象种类，跨函数数据必须带函数身份。
- 不修改 parser/sema 来规避 IR 反例，不要求 lowering 增加可信生命周期标注。
- 不修改 backend/interpreter 的输入包装或指针运行时表示。
- 不引入通用图框架、第三方分析器或 Rust 编译器内部依赖。

当前问题集中在分析信息丢失；没有证据需要重写整个编译器。若实施时发现普通 verifier 本身缺少某条必要类型不变量，应单列小修复及测试，不能把未证明的前提偷偷加入逃逸分析。

## 8. 分阶段实施与验收

### 阶段 A：固定契约并复现缺口

- [x] 用 raw `IrBuilder` 构造第 2 节两个反例及经 load/多级指针的变体。
- [x] 每个反例先断言 `verify(&raw).is_ok()`，再记录旧 escape 的结果，区分已确认漏检与推测路径。
- [x] 增加纯整数 aggregate 复制到 global 的正例，避免以“拒绝所有 stack source”掩盖缺口。
- [x] 固定借入返回仍算 capture、weak update 不消除污染等兼容规则。

阶段 A 的验收是有可运行的反例，不以代码阅读替代复现。上述用例已确认通过普通 verifier，且当前 escape check 会漏检四条危险路径。

### 阶段 B：替换函数内传播

- [x] 实现有限对象节点、P/M 集合、`Reach`、pointer-content 类型查询。
- [x] 为 GEP、pointer load/store、CFG 参数、aggregate copy/zero 实现第 4 节规则。
- [x] 实现共享外部内存及 Unknown 处理，禁止默认丢弃来源。
- [x] 用同一 sink 收集逻辑生成逃逸事实与诊断。

验收：单函数别名反例已拒绝，非逃逸局部访问和无 pointer 的复制仍通过；逆 block 存储顺序的回归确认集合收敛。跨函数返回来源与写回效果仍由阶段 C 实现。

### 阶段 C：替换跨函数摘要

- [x] 实现 capture、return dependency、external write、local violation 摘要。
- [x] 应用摘要时同时更新实参约束、返回来源和 caller 内存。
- [x] 实现双层不动点，覆盖直接递归、互递归及输入参数别名。
- [x] 分析所有函数，包括 main 不调用的函数；直到全包稳定后才构造最终诊断。

验收：跨函数间接读取和 aggregate 复制的漏检已修复，非逃逸递归正例仍通过；写效果的保守 havoc 由专门测试固定，不以忽略写效果消除误报。

### 阶段 D：覆盖矩阵与边界验证

| 类别 | 必测拒绝场景 | 必测接受场景 |
|---|---|---|
| 直接来源 | return 局部地址、存入 global | 局部 load/store、global 地址返回 |
| 内存别名 | pointer-slot 地址经 load 或 block 参数后取出局部指针并返回 | 相同路径加载 null / global pointer |
| CFG | 汇合或循环回边带入逃逸来源 | 只在本函数使用的 pointer block parameter |
| aggregate | 含栈指针的 struct/array 经别名复制到 global | 纯整数 aggregate、仅含 null/global pointer 的 aggregate |
| 多级内容 | `**T`、嵌套结构、pointer 环间接保存/返回局部指针 | 循环对象图在函数内使用且分析终止 |
| 调用 | `global = *param`、callee 经参数做 aggregate copy | 只读/只写整数的内部 helper |
| 参数别名 | 两参数指向同一 slot，经其中一个写入再从另一个读出外传 | 别名参数只在调用期间读取 |
| 调用写回 | callee 改写 pointer slot，caller 再加载外传 | 参数均为 global 地址时的 global pointer 写回 |
| 递归 | 递归链中任一函数外传本帧地址、上下层同名 slot 混淆 | 现有直接/互递归 noescape 程序 |
| 策略 | 清零前已外传、borrowed identity 接收栈参数 | 全局参数传给 borrowed identity |
| 完整性 | 任意 Unknown 不能使危险路径静默通过 | null、整数运算/比较不产生虚假 pointer 事实 |

- [ ] 多个受污染 branch argument 必须全部传播；不要使用有副作用的 `.any(insert)` 作为“一次遍历处理全部元素”的实现。
- [ ] 每类 aggregate 用例同时覆盖 struct/array、嵌套字段、动态 index、自复制与可能重叠复制。
- [ ] 变形测试：同一路径分别经过直接值、局部 pointer slot、GEP、block 参数；危险路径均拒绝。
- [ ] 调整函数声明顺序、合法 block 排列与分支参数顺序，接受/拒绝结果保持一致。
- [ ] 新增 opcode 时让穷尽 `match` 强制审查其逃逸语义，不用兜底 `_ => {}` 吞掉扩展。

测试以 `IrBuilder` 构造的 IR 为主，源码测试只补充真实 lowering 路径。不能用当前 interpreter 作为悬空引用检测 oracle：它会在函数返回时弹出 frame，但 frame 索引可以被后续调用复用，执行成功不等于生命周期安全。若后续加入动态活跃帧检测，也只能作为有界执行的补充证据。

### 阶段 E：替换上线与同步文档

- [ ] 删除旧算法，保留并扩展现有回归；新旧结果差异分类为修复漏检、减少误报或新增保守误报。
- [ ] 定向运行 `cargo test -p gane_ir`，再执行 `cargo check --workspace`、`cargo test --workspace`、`cargo fmt --check`；如 LLVM 环境阻塞，明确记录未完成项。
- [ ] 确认 driver 仍经 `verify_and_check_escape` 才把 package 交给 interpreter/codegen。
- [ ] 在 `docs/ir-design.md` 第 12/14 节同步新的分析契约，并纠正“逃逸局部地址由 sema 拒绝”的过时表述。
- [ ] 在 `PROGRESS.md` 单列加强逃逸检测里程碑；完成上述验收后标记完成，不覆盖原简化版已完成的历史。

本次仅新增计划，不将上述实施阶段标记为完成。

## 9. 终验标准与后续精度提升

终验需要给出逐条 transfer 的来源覆盖论证：具体指针来源必须包含在 P 中，具体对象保存的 pointer 来源必须包含在 M 中，调用效果必须覆盖被调函数对可达内存的影响，所有对外保存/返回必须经过 sink 检查。反例测试支持这些不变量，但不等于形式化证明。

先达到“任意现有合法 IR 都有保守规则、已知漏检全部关闭、固定点终止且诊断稳定”。若该论证发现某条无法建模的路径，必须补规则或明确拒绝，不能靠 sema 不生成来排除。

首版已知精度代价：字段/数组元素合并、不同外部参数合并、忽略覆盖清零、调用写效果 havoc、return 视作 capture。代码分别用 `ponytail:` 注释写明局限及升级条件。

后续按实际被拒绝的安全程序选择升级，顺序可以是：区分被调用函数的写目标/来源；区分字段与数组 summary element；为确定单一对象实现 CFG 敏感更新；最后允许借入指针返回并传播显式 outlives 约束。任何升级都要保留旧反例，并重新检查别名、递归、重叠复制和调用写回的不变量。
