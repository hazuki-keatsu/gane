# Go 语法与 `gane_sema` 覆盖矩阵

本文件是 `gane_sema` 的功能 backlog，也是阅读代码时的语法覆盖索引。目录以 [Go Programming Language Specification](https://go.dev/ref/spec) 的语法章节为准；本文撰写时官方规范标为 Go 1.27（2026-05-26）。

它刻意区分三件事：

- **Go 语法**：官方规范是否定义这个产生式或语法变体；本文件尽量逐项列出。
- **parser**：仓库的 `gane_parser` 是否有对应 token/AST 并能解析该类别。`[AST]` 表示有明确 AST 形状；这不表示每种合法/非法边界都已有回归测试。
- **sema checkbox**：唯一可勾选的状态。`[x]` 表示当前 sema 接受并检查该项的当前子集；`[ ]` 表示它不支持、仅错误恢复遍历，或实现的语义尚不符合 Go 规则。

因此，**不要因为 parser 能解析就勾选 sema**。实现新项时，应同步更新本项的限制、测试、`docs/sema-overview.md`，以及必要的 HIR 契约。

> 本项目是 Go-like 语言和 HIR V0 前端，不承诺完整 Go 兼容性。表中的 `[x]` 也只表示当前 Gane 语义，不自动等价于完整 Go 语义。

## 0. 源码表示与词法元素

这些大部分是 scanner/parser 的职责，通常不会成为 `gane_sema` 的独立功能；仍列在这里，防止“完整 Go 语法”遗漏词法层。

### 源码、字符、空白与注释

- [ ] UTF-8 源码及 Go 的 Unicode code point 限制 — parser：[AST/scanner]；sema 不参与。
- [ ] Unicode letter / decimal digit identifier 分类 — parser：[AST/scanner]；sema 只接收已解析 `Ident`。
- [ ] BOM、NUL 等实现限制 — parser：[scanner]；sema 不参与。
- [ ] line comment `//...` — parser：[AST/scanner]；sema 不解释普通 comment。
- [ ] block comment `/*...*/` — parser：[AST/scanner]；sema 不解释普通 comment。
- [ ] comment 对分号插入的影响 — parser：[scanner]；sema 不参与。
- [ ] compiler directive / `//go:` comment 的语义 — parser：[AST command metadata]；当前 sema 未实现。
- [ ] 自动分号插入 — parser：[scanner]；sema 不参与。

### Identifier、keyword、operator 与 punctuation

- [ ] identifier 的词法合法性 — parser：[AST/scanner]；sema 假定 parser 已验证。
- [ ] blank identifier `_` 的完整 Go 规则 — parser：[AST]；sema 只支持部分位置，见后文。
- [ ] 25 个 Go keyword 的词法保留性 — parser：[scanner]；sema 只实现部分对应语法。
- [ ] operator/punctuation 的词法识别 — parser：[scanner]；每个运算符的 sema 状态见“表达式”和“语句”。

### Literal 词法和常量值

- [x] decimal integer literal，例如 `0`、`42`、`4_2` — parser：[AST]；sema 赋予 `int` 类型并可折叠。
- [x] hexadecimal integer literal，例如 `0x2a`、`0X2A`、`0x_2a` — parser：[AST]；sema 的当前整数解析支持 `0x`/`0X` 与 `_`。
- [ ] binary integer literal，例如 `0b1010` — parser：[AST]；当前 sema 会把它当 `Token::Int`，但不能正确产生常量值。
- [ ] explicit octal integer literal，例如 `0o600`、`0O600` — parser：[AST]；当前 sema 不正确解析其基数。
- [ ] legacy octal integer literal，例如 `0600` — parser：[AST]；当前 sema 会按十进制值处理，语义不符合 Go。
- [ ] full arbitrary-precision integer constant evaluation — parser：[AST]；`IntegerValue` 可保存大整数，但多项运算折叠仅覆盖可转为 `i128` 的情形。
- [ ] decimal floating-point literal — parser：[AST]；sema 拒绝非整数 literal。
- [ ] hexadecimal floating-point literal — parser：[AST]；sema 拒绝。
- [ ] imaginary literal — parser：[AST]；sema 拒绝。
- [ ] rune literal，包括转义与 Unicode scalar value 验证 — parser：[AST]；sema 拒绝。
- [ ] interpreted string literal — parser：[AST]；sema 拒绝。
- [ ] raw string literal — parser：[AST]；sema 拒绝。

## 1. 常量、变量、类型、函数与 package block

### 预声明标识符和常量系统

- [x] `true`、`false` bool 常量 — parser：[AST Ident]。
- [x] `nil` — parser：[AST Ident]；当前仅可赋给 pointer。
- [x] `bool`、`int`、`byte` 预声明类型 — parser：[AST Ident]。
- [ ] Go 全部预声明基本类型：`string`、`uint*`、`int*`、`uintptr`、`rune`、`float*`、`complex*` — parser：[AST Ident]；sema 尚无对应类型。
- [ ] `byte` 与 `rune` 作为 Go alias 的完整 identity 规则 — 当前 `byte` 是 Gane 独立 basic type，不是 Go 的 `uint8` alias。
- [ ] `error` interface 预声明类型 — parser：[AST Ident]；sema 未实现 interface。
- [ ] `iota` — parser：[AST Ident]；sema 未声明它。
- [ ] `any`、`comparable` — parser：[AST Ident]；sema 未实现泛型约束。
- [ ] untyped constant、默认类型、representability、constant conversion 的 Go 规则。
- [ ] 完整 constant expression 系统：rune/float/complex/string、builtin 常量结果等。

### Declaration、scope 与 visibility

- [x] universe → package → file → function → block 的名字查找链。
- [x] block shadowing。
- [x] 同一 scope duplicate declaration 诊断；保留首个绑定以继续分析。
- [x] 同 package 多文件顶层名字可见性与跨文件重名检查。
- [x] 文件级 `FileScope` 建立；当前为空，等待 import binding。
- [ ] Go package block、file block、implicit block 的所有精确定义。
- [ ] label scope。
- [ ] type parameter scope。
- [ ] exported identifier 与跨 package 可见性规则。
- [ ] 完整 declaration order、scope start/end、init dependency 的 Go 规则。

### Constant declaration

- [x] 顶层单个或分组 `const` declaration — parser：[AST GenDecl/ValueSpec]。
- [x] 显式 bool/int/byte 常量 initializer 的基本检查。
- [ ] local `const` declaration。
- [ ] const declaration 中省略 type / expression 后继承前一个 `ConstSpec`。
- [ ] 多 name、多 expression 的 Go 展开规则。
- [ ] `iota` 与常量组内计数规则。
- [ ] 常量转换、溢出、精度和 representability 检查。

### Variable declaration

- [x] 顶层 `var` declaration — parser：[AST GenDecl/ValueSpec]。
- [x] local `var` declaration — parser：[AST DeclStmt/ValueSpec]。
- [x] 具有显式类型的 variable declaration。
- [x] 从单个非 `nil` initializer 推导 variable type。
- [x] `nil` 初始化 pointer variable（有显式 pointer type）。
- [x] 初始化环检测（当前顶层 global initializer）。
- [ ] `var` 的完整多 name、多 expression、多返回值展开规则。
- [ ] 从 untyped constant 推导默认类型。
- [ ] `var` 的完整零值、可表示性、conversion 规则。
- [ ] short variable declaration `:=`，包括“至少一个新变量”和 redeclaration 规则。

### Type declaration

- [x] 定义新 named type：`type T U` — parser：[AST TypeSpec]。
- [x] named type 的 underlying type 延迟解析与跨声明前向引用。
- [x] pointer 间接递归类型，例如 `type Node struct { next *Node }`。
- [ ] type alias：`type T = U` 的 Go identity 规则。
  - parser 记录 `=`；当前 sema 仍创建新的 `Named` type。
- [ ] type parameter declaration：`type T[P constraint] ...`。
- [ ] type parameter constraint 和 type set。
- [ ] instantiated type identity / assignability。

### Function declaration

- [x] package-level function declaration — parser：[AST FuncDecl]。
- [x] parameter list；命名和未命名 parameter。
- [x] result list；命名和未命名 result。
- [x] 无结果或单一非 aggregate 结果。
- [x] declaration 与 call 的前向引用。
- [x] 无 body declaration 的临时 extern 处理。
  - 这是由 `FuncDecl.body == None` 推导的 V0 过渡机制，不是稳定 FFI 语法。
- [ ] receiver / method declaration。
- [ ] variadic parameter：`...T`。
- [ ] 多结果函数与多值表达式传播。
- [ ] array/struct result。
- [ ] type parameterized function declaration。
- [ ] generic method declaration / method receiver type parameter 规则。
- [ ] linkage name、ABI、library、platform、ownership/escape 等稳定 extern binding 语义。

### Package、import 与 program initialization

- [x] `package main` clause — parser：[AST File]；sema 当前仅接受这个 package 名。
- [x] package 顶层 declaration 跨文件收集。
- [ ] 非 `main` package。
- [ ] import declaration — parser：[AST ImportSpec]；sema 报不支持。
- [ ] default import name。
- [ ] explicit import alias。
- [ ] dot import `.`。
- [ ] blank import `_`。
- [ ] grouped imports。
- [ ] import path、module/package loader、build constraints、import cycle 检查。
- [ ] imported package export data、qualified identifier、跨 package type/object identity。
- [ ] Go 的 package initialization order、`init` function、multiple `init` 函数。
- [ ] Go program execution model（main package 初始化后调用 `main`）。

## 2. 类型语法

### Type name 与 type literal

- [x] type name identifier，例如 `int`、`T` — parser：[AST Ident]。
- [x] parenthesized type：`(T)` — parser：[AST ParenExpr]。
- [x] pointer type：`*T` — parser：[AST StarExpr]。
- [x] array type：`[N]T` — parser：[AST ArrayType]。
  - `N` 当前只能是非零 decimal 或 hexadecimal integer literal；不能是常量名或表达式。
- [x] struct type：`struct { name T }` — parser：[AST StructType]。
  - struct 不能为空，field 必须具名，field 名不可重复。
- [ ] qualified type name：`pkg.T` — parser：[AST SelectorExpr]；等待 import/package resolution。
- [ ] slice type：`[]T` — parser：[AST ArrayType]；sema 拒绝没有长度的 array type。
- [ ] constant expression array length。
- [ ] zero-length array。
- [ ] struct tag，例如 ``struct { F int `json:"f"` }``。
- [ ] embedded struct field：`struct { T }`、`struct { *T }`。
- [ ] function type：`func(P) R`。
- [ ] interface type：`interface { ... }`。
- [ ] interface method spec。
- [ ] embedded interface/type element。
- [ ] interface union：`A | B`。
- [ ] underlying-type term：`~T`。
- [ ] map type：`map[K]V`。
- [ ] channel type：`chan T`、`chan<- T`、`<-chan T`。
- [ ] generic type instance：`T[A, B]`。

### Type identity、underlying type 与 properties

- [x] Gane named type identity 与 underlying type 查询。
- [x] pointer、array、struct、signature 的内部 type representation。
- [x] named recursive type resolving state，区分经过 pointer 与按值循环。
- [x] 当前 comparable：bool、int、byte、pointer。
- [ ] Go 完整 type identity（alias、defined type、instantiation、interface identity）。
- [ ] Go 完整 underlying type 规则。
- [ ] Go complete comparability rules（array、struct、interface、type parameter）。
- [ ] Go assignability、convertibility、implements、satisfies、method set 规则。
- [ ] size、alignment、field offset、unsafe layout 规则。

## 3. 表达式

### Operand 与 primary expression

- [x] 已绑定 value identifier — parser：[AST Ident]。
- [x] `true`、`false`、`nil` identifier operand。
- [x] integer literal operand（限制见第 0 节）。
- [x] parenthesized expression：`(x)`。
- [x] ordinary function call：`f(args)`，参数数量和可赋值性检查。
- [x] struct field selector：`x.f`。
- [x] pointer-to-struct 的单层自动解引用 selector：`p.f`。
- [x] array index：`a[i]`。
- [ ] qualified identifier：`pkg.Name`。
- [ ] method value / method expression。
- [ ] promoted field / promoted method selector。
- [ ] 多层 pointer selector 自动解引用。
- [ ] slice/string/map index。
- [ ] slice expression：`a[low:high]`、`a[low:high:max]`。
- [ ] type assertion：`x.(T)`。
- [ ] generic instantiation expression：`F[T]`、`F[A, B]`。
- [ ] conversion：`T(x)`。
- [ ] function literal：`func(...) { ... }`。
- [ ] composite literal：`T{...}`、`[]T{...}`、`map[K]V{...}`。
- [ ] literal element、keyed element：`key: value`。
- [ ] ellipsis `...` 在 call/composite literal 中的语义。

### Unary expression

- [x] integer unary plus：`+x`。
- [x] integer unary minus：`-x`。
- [x] bool logical not：`!x`。
- [x] address-of：`&x`，operand 必须是 variable。
- [x] pointer dereference：`*p`。
- [ ] bitwise complement：`^x`。
- [ ] channel receive：`<-ch`。

### Binary expression

- [x] integer arithmetic：`+`、`-`、`*`、`/`、`%`。
- [x] integer ordered comparison：`<`、`<=`、`>`、`>=`。
- [x] equality comparison：`==`、`!=`，限当前 comparable types。
- [x] bool logical conjunction/disjunction：`&&`、`||`。
- [x] 当前已支持运算的部分 bool/int 常量折叠。
- [ ] bitwise AND：`&`。
- [ ] bitwise OR：`|`。
- [ ] bitwise XOR：`^`。
- [ ] bit clear：`&^`。
- [ ] left shift：`<<`。
- [ ] right shift：`>>`。
- [ ] string concatenation / comparison。
- [ ] float、complex、rune 的运算规则。
- [ ] full Go operator precedence 与 untyped operand conversion 规则。

### Built-in functions

以下都是 Go 的预声明函数或 special built-in；当前 sema 没有 `ObjectKind::Builtin` 的可调用实现。

- [ ] `append`。
- [ ] `cap`。
- [ ] `clear`。
- [ ] `close`。
- [ ] `complex`。
- [ ] `copy`。
- [ ] `delete`。
- [ ] `imag`。
- [ ] `len`。
- [ ] `make`。
- [ ] `max`。
- [ ] `min`。
- [ ] `new`。
- [ ] `panic`。
- [ ] `print` / `println`。
- [ ] `real`。
- [ ] `recover`。
- [ ] `unsafe` package operations，例如 `unsafe.Sizeof`、`unsafe.Alignof`、`unsafe.Offsetof`、pointer conversion。

## 4. 语句

### Simple statement

- [x] empty statement。
- [x] expression statement，仅限 function call。
- [x] assignment：`lhs = rhs`。
  - 支持等长 lhs/rhs 的逐项检查，以及 blank assignment `_ = value`。
- [x] increment：`x++`。
- [x] decrement：`x--`。
- [ ] 任意 expression statement，例如单独写 `x + y`。
- [ ] short variable declaration：`:=`。
- [ ] compound assignment：`+=`、`-=`、`*=`、`/=`、`%=`、`&=`、`|=`、`^=`、`&^=`、`<<=`、`>>=`。
- [ ] Go multiple assignment 与 multi-value call/map lookup/type assertion/range 展开规则。

### Return、branch 与 block

- [x] block statement：`{ ... }`；每个 block 建立 lexical scope。
- [x] `return` 的结果数量与可赋值性检查。
- [x] 无标签 `break`，仅在 for 内。
- [x] 无标签 `continue`，仅在 for 内。
- [ ] 多值 return。
- [ ] named result 的裸 return。
- [ ] label declaration：`Label:`。
- [ ] labeled `break` / `continue`。
- [ ] `goto`。
- [ ] `fallthrough`。

### If 与 for

- [x] `if condition { ... }`。
- [x] `if condition { ... } else { ... }`。
- [x] else-if AST 形式。
- [x] condition for：`for condition { ... }`。
- [x] infinite for：`for { ... }`。
- [ ] if init statement：`if init; condition { ... }`。
- [ ] three-clause for：`for init; condition; post { ... }`。
  - 若 init 或 post 存在，当前报告不支持；`for ; ; {}` 会作为无限 for 通过。
- [ ] range over array、pointer-to-array、slice、string、map、channel、integer、function。
- [ ] range 的 `=` / `:=`、key/value/blank identifier 绑定规则。

### Switch、select 与并发

- [ ] expression switch。
- [ ] expression switch init statement。
- [ ] switch case expression list、default、implicit break、fallthrough 规则。
- [ ] type switch。
- [ ] type switch guard、case type list、nil case、隐式变量绑定。
- [ ] select statement。
- [ ] communication clause。
- [ ] send statement：`ch <- value`。
- [ ] receive expression 在 assignment/select 中的特殊多值形式。
- [ ] `go` statement。
- [ ] `defer` statement。
- [ ] defer/panic/recover 的运行时语义。

## 5. 语义边界与后端契约

这些不是独立的 parser 产生式，却是把 checkbox 从 `[ ]` 改为 `[x]` 时必须同时完成的语义工作。

- [x] `SemanticInfo` 记录 declaration、use、expression type/value mode、block/function scope、field selection。
- [x] `AnalysisResult` 提供 object/type/tuple/package member/file scope/node 查询。
- [x] V0 global initializer 限制：bool/int/byte 常量或 nil pointer。
- [x] V0 extern ABI 限制：int/byte 与满足 pointee 限制的 pointer。
- [ ] 新语法的完整 source span、definition/use/type/selection facts。
- [ ] 任何新可执行语法的 HIR lowering 支持。
- [ ] 任何新可执行语法的 HIR verifier、interpreter、LLVM backend 定义与测试。
- [ ] escape/alias/ownership 规则与 extern ABI metadata。
- [ ] Go runtime、GC、goroutine、channel、panic/recover、reflection 的完整运行时模型。

## 6. 勾选流程

勾选一个项之前，至少完成：

- [ ] parser 已为该语法形成无歧义 AST，或已新增对应 AST 与 parser 测试。
- [ ] sema 已实现合法输入的名称解析、类型检查和必要的 scope 行为。
- [ ] sema 已为非法输入给出稳定 diagnostic，并有负向测试。
- [ ] `SemanticInfo` 已补齐 lowering/工具需要的 facts。
- [ ] 若功能跨文件或涉及 import，已覆盖至少两个 `PackageFile`。
- [ ] 若功能进入可执行路径，HIR/codegen 契约与测试已同步。
- [ ] `cargo test --workspace` 通过。

## 7. 阅读实现的入口

- `crates/sema/src/checker.rs`：本表中绝大多数 `[x]` / `[ ]` 的实际判断位置。
- `crates/sema/src/types.rs`：`TypeKind` 中出现 future variant 不等于对应语法已支持。
- `crates/sema/src/scope.rs`：作用域创建、同 scope 重名、parent lookup。
- `crates/sema/src/interface.rs`：外部可依赖的查询 API。
- `crates/parser/src/ast/ast.rs`：parser 能产出的 expression、statement、declaration AST 分类。
- `docs/sema-overview.md`：数据流与源码阅读路线图。

