# `gane_sema` 总览

一句话概括：`gane_sema` 接收同一个 package 的若干已解析 AST 文件，建立名字、对象、类型和词法作用域，检查当前 MVP 支持的声明、表达式和语句，并产出可供 HIR lowering、IDE 或诊断使用的语义事实。

```text
source text
  │
  ├─ gane_parser: scanner + parser
  │       └─ ast::File (one per source file)
  │
  └─ caller/package loader
          └─ PackageInput { path, files }
                   │
                   ▼
             analyze_package
                   │
                   ├─ AnalysisResult: objects/types/scopes + SemanticInfo
                   └─ Vec<Diagnostic>
```

`sema` 不读取目录、不筛选 build tag、不解析 import path、不读取文件内容，也不重新做词法或语法分析。这些是 parser 和未来 package loader 的职责。

## 1. 从哪里开始读

建议按这个顺序阅读：

1. `crates/sema/src/interface.rs`：公开 API 与外部可查询的结果。
2. `crates/sema/src/checker.rs`：`analyze_package` 的阶段编排和绝大部分语义规则。
3. `crates/sema/src/types.rs`：ID、对象、类型、scope、语义事实的结构定义。
4. `crates/sema/src/scope.rs`：作用域创建、声明插入、沿 parent 查找。
5. `crates/sema/src/symbol_table.rs`：名字 interning 与对象 arena。

若想用具体输入跟踪，可以从 `checker.rs` 中的单元测试开始；测试会直接构造 parser AST，再调用 `analyze_package`。

## 2. 最小调用方式

单文件 package：

```rust
use gane_parser::{
    parser::{parse_file, Mode},
    token::FileSet,
};
use gane_sema::{analyze_package, FileId, PackageInput};

let mut files = FileSet::new();
let (ast, parse_errors) = parse_file(
    &mut files,
    "main.go",
    b"package main\nfunc main() {}\n",
    Mode::default(),
);
assert!(parse_errors.is_none());

let analysis = analyze_package(PackageInput::single(
    "example/main",
    FileId::from_raw(1),
    &ast,
));
assert!(!analysis.has_errors());
```

多文件 package 必须使用同一个 `FileSet` 解析所有文件；这使所有 `Span` 位于同一坐标系，诊断才能定位到正确文件。

```rust
use gane_sema::{FileId, PackageFile, PackageInput, PackagePath};

let input = PackageInput {
    path: PackagePath("example/main".to_owned()),
    files: vec![
        PackageFile { id: FileId::from_raw(1), ast: &types_ast },
        PackageFile { id: FileId::from_raw(2), ast: &main_ast },
    ],
};
let analysis = analyze_package(input);
```

`FileId` 是 package 输入内的稳定身份，必须唯一。重复 ID 会产生 `E2006`；第一个文件的 file scope 会保留，不会被后一个覆盖。

## 3. `analyze_package` 的阶段

入口在 `checker.rs`：

```rust
pub fn analyze_package(input: PackageInput<'_>) -> AnalysisResult
```

当前执行顺序如下：

```text
check_package_clause
  → create_universe
  → create_package_scope
  → create_file_scopes
  → collect_top_level
  → resolve_type_headers
  → check_global_values
  → check_function_bodies
  → validate_entry_point
  → finish
```

各阶段的职责：

| 阶段 | 做什么 | 为什么此时做 |
| --- | --- | --- |
| `check_package_clause` | 检查输入非空、所有文件 package 名一致、MVP 的 `main` 限制 | 后续阶段需要一个 package 身份 |
| `create_universe` | 建立预声明类型和值：`bool`、`int`、`byte`、`true`、`false`、`nil` | 所有文件都可沿 scope parent 链找到它们 |
| `create_package_scope` | 建立 package 顶层声明所在的 scope | package 成员可跨文件相互引用 |
| `create_file_scopes` | 每个 `PackageFile` 建立一个 file scope | 将来 import alias 只应在声明它的文件可见 |
| `collect_top_level` | 为所有顶层 type/const/var/func 创建占位 object | 允许函数、类型及全局名字前向引用 |
| `resolve_type_headers` | 解析 named type underlying type、struct fields、函数参数和结果 | 函数体前先得到完整签名 |
| `check_global_values` | 检查 const/var 初始化和初始化环 | 全局值可能互相依赖 |
| `check_function_bodies` | 检查函数体中的局部声明、表达式、赋值、控制流和 return | 已有完整顶层名字与签名 |
| `validate_entry_point` | 检查 `func main()` 的 MVP 入口约束 | 属于 package 级最终验证 |
| `finish` | 冻结 arena、整理 diagnostics、构造结果 | checker 的可变状态不再暴露 |

重要的是 `collect_top_level` 与后续解析分开。若在遇到一个声明时立刻解析它，后面才出现的类型或函数将无法被前向引用。

## 4. 核心身份：ID 而不是 Rust 引用

`types.rs` 使用 arena 加稳定整数 ID 表示所有会被交叉引用的实体：

```text
NameId     名字的 interned 拼写
ObjectId   一次声明或预声明实体
TypeId     一个语义类型
TupleId    函数参数/结果 tuple
ScopeId    一个词法作用域
FileId     PackageInput 中一个源文件
NodeId     一个 AST span 在本次 checker 中的节点身份
PackageId  package 身份（当前 MVP 内部使用 0）
```

这样设计有两个原因：

- Go 风格类型可以递归，例如 `type Node struct { next *Node }`；直接用 Rust 引用会很难表达环。
- 语义查询、diagnostic、后续 HIR lowering 都应保存稳定身份，而不应持有 checker 内部容器的借用。

有两个保留的无效 ID：

```text
TypeId(0)   = INVALID，表示 poison type
ObjectId(0) = INVALID，表示错误恢复中的占位 object
```

这使 checker 在一次错误后仍能继续遍历其余 AST，尽量报告更多独立错误，而不是被迫在每一处使用 `Option` 或 panic。

## 5. Object、Type、Scope 的关系

最重要的心智模型是：

```text
source identifier
      │
      ├─ declaration ── SemanticInfo.defs ──► ObjectId
      └─ use         ── SemanticInfo.uses ──► ObjectId
                                                 │
                                                 ├─ Object.kind
                                                 ├─ Object.span
                                                 ├─ Object.parent: ScopeId
                                                 └─ Object.typ: TypeId
                                                                    │
                                                                    └─ Type.kind
```

### 5.1 `Object`

`Object` 是一个可被名字查找到的实体。它保存：

- `kind`：`Const`、`Var`、`Func`、`TypeName`、`Field`、`Param` 等；
- `name`：`NameId`，通过 `AnalysisResult::name` 还原拼写；
- `package`：所属 package；
- `parent`：声明所在 scope；
- `span`：声明位置；
- `typ`：该实体的 `TypeId`。

顶层 type/const/var/func 的 `parent` 是 package scope。它们**不**属于 file scope：同一 package 的其他文件必须能看见这些声明。

`Func` object 的 `typ` 是 signature `TypeId`；`ObjectKind::Func` 也保存同一个 signature ID。V0 要求每个函数声明都有函数体；无函数体声明会在 sema 中以 unsupported feature 拒绝，不能借此隐式表示 FFI。

### 5.2 `Type`

`TypeArena` 中的 `TypeKind` 比当前语言子集更大。阅读时应区分“enum 有该分支”和“checker 接受源码构造它”。当前可由源码稳定构造的主要类型是：

- `bool`、`int`、`byte`；
- named type；
- pointer `*T`；
- 非零长度 array `[N]T`；
- 非空、具名字段的 struct；
- 函数 signature 与参数/结果 tuple。

named type 与 underlying type 必须区分：

```go
type UserID int
var id UserID
```

`id` 的 declared type 是 `UserID` 的 `TypeId`，而不是 `int` 的 `TypeId`。需要底层表示时，通过：

```rust
let underlying = analysis.underlying_type(analysis.object(id).typ);
```

`resolve_named` 使用 `UnderlyingState::{Unresolved, Resolving, Resolved, Invalid}` 处理递归。按值递归会报错：

```go
type Bad struct { next Bad }
```

经过 pointer 的递归则允许：

```go
type Node struct { next *Node }
```

### 5.3 `Scope`

`ScopeArena` 只负责三件事：创建 child scope、向一个 scope 插入名字、从当前 scope 沿 parent 链向外查找名字。它不拥有 `Object` 本身。

当前作用域树为：

```text
UniverseScope
└─ PackageScope
   ├─ FileScope(a.go)
   │  └─ FunctionScope
   │     └─ BlockScope ...
   └─ FileScope(b.go)
      └─ FunctionScope
         └─ BlockScope ...
```

各层含义：

- universe：预声明类型和值；
- package：package 顶层声明；
- file：文件局部可见的名字的预留位置；当前为空；
- function：参数、命名结果及函数体的外层环境；
- block：每一个 `{ ... }`，以及目前 if/for 的中间词法区域。

`ScopeArena::declare` 不覆盖已有绑定，因此同 scope 重名会保留第一个 object，并让 checker 在两个声明位置之间建立 duplicate diagnostic。`lookup` 返回的不只是 object，还返回实际提供名字的 scope，这对理解 shadowing 很有帮助。

## 6. FileScope：为何存在且现在如何流动

FileScope 是为了 package 内的“文件局部名字”准备的。最主要的未来使用者是 import：

```go
// a.go
import "fmt"

// b.go
func main() { fmt.Println() } // 未来应当是未定义，而不是可见
```

因此 import 名不能放入 package scope；它应插入 `FileScope(a.go)`。

当前尚未实现 import，但 FileScope 已经参与实际数据流：

```text
PackageInput.files
    │
    ├─ FileId ──► Checker.file_scopes: HashMap<FileId, ScopeId>
    │                        │
    │                        └─ ScopeKind::File，parent = PackageScope
    │
    └─ 顶层声明记录来源 FileId
             │
             ├─ type spec: 解析 underlying type 时从该 FileScope 查找
             ├─ func decl: FunctionScope 挂在该 FileScope 下
             └─ global initializer: 类型和表达式从该 FileScope 查找
```

现在 file scope 为空，所以从它查找会继续向 parent 的 package scope 和 universe scope 走；现有跨文件顶层引用行为不变。未来只需在“创建 file scope”与“收集顶层声明”之间插入已经由 loader 解析好的 import bindings，后面的名字查找路径不需要重写。

外部只能获得 file scope 的身份：

```rust
let scope = analysis.file_scope(FileId::from_raw(1));
```

外部不能枚举该 scope 的 names，因为 `ScopeArena` 仍是内部实现。对当前调用方而言，`ScopeId` 可用于记录、比较或关联语义事实；真正查询声明/类型应使用下节的 `AnalysisResult` 和 `SemanticInfo` API。

## 7. `SemanticInfo`：AST 与语义结果的连接

`SemanticInfo` 是 node 级事实表。它不拥有 arena，也不做名字查找。它的 key 是 checker 由 AST `Span` 分配的 `NodeId`：

| 字段 | key | value | 含义 |
| --- | --- | --- | --- |
| `defs` | 声明标识符 | `ObjectId` | 这个 identifier 定义了哪个 object |
| `uses` | 使用标识符 | `ObjectId` | 这个 identifier 引用了哪个 object |
| `types` | expression | `TypeAndValue` | 表达式的类型、值类别和可选常量 |
| `scopes` | 函数名或 block | `ScopeId` | 节点对应的词法 scope |
| `selections` | `x.f` | `Selection` | 选择到的字段、index path、是否间接访问 |

`FileId → ScopeId` 不在 `SemanticInfo` 中，而是 `AnalysisResult` 的 package 输入元数据。这避免把“一个 AST 节点的语义事实”和“一个输入文件的作用域身份”混为同一种映射。

典型查询模式：

```rust
use gane_diagnostics::Span;

// 1. 从 AST identifier 或 expression 得到其 span。
let span = Span::new(ident.pos(), ident.end());

// 2. 取得本次 analysis 中分配的 NodeId。
let node = analysis.node_at(span).expect("node was checked");

// 3a. 声明点：defs → ObjectId → TypeId → Type。
let object = analysis.info.defs[&node];
let typ = analysis.type_of(analysis.object(object).typ);

// 3b. 使用点：uses → ObjectId。
let resolved = analysis.info.uses[&node];

// 3c. 任意已检查 expression：直接查询 TypeAndValue。
let value = &analysis.info.types[&node];
let expression_type = analysis.type_of(value.typ);
```

package 顶层成员不需要 AST node 即可按拼写查询：

```rust
let object = analysis.package_member("globalValue");
```

局部变量没有“按名字全局查询”的 API；局部名字可能被 shadow，因此应始终使用其具体 AST declaration/use node 查询。

### `TypeAndValue` 的 `ValueMode`

表达式类型并不足以回答“它能否被赋值”或“它是不是编译期常量”。`TypeAndValue` 额外保存：

- `typ`：表达式类型；
- `mode`：`Value`、`Variable`、`TypeExpr`、`Nil`、`NoValue`、`Invalid` 等；
- `constant`：可选的 `ConstValue`。

例如：一个变量 identifier 通常是 `Variable`，`true` 是带 `ConstValue::Bool(true)` 的值，type name 在类型上下文可表现为 `TypeExpr`，错误恢复则使用 `Invalid`。

## 8. 名字解析与声明检查

### 顶层声明

`collect_top_level` 按输入文件逐一遍历，但把 type/const/var/func 的 object 都插入 package scope。这同时保证：

- 任意文件可引用同 package 的顶层成员；
- 重名检查跨文件生效；
- 后续 type 和 function header 可以前向引用；
- 记录的 `FileId` 保留“这个声明的类型/初始化应在哪个 file scope 解析”。

随后 `resolve_type_headers` 解析所有 named type 和 signature。函数参数和结果 object 会声明到函数 scope；函数体检查在所有 header 完成后才开始。

### 局部声明与普通使用

函数 body 由 `check_function_bodies` 进入；`check_block` 为每个 block 创建 child scope。表达式中的 identifier 通过 `bind_value_name` 调用 `ScopeArena::lookup`，结果同时写入 `SemanticInfo.uses`。

局部 `var` 的 initializer 在变量插入当前 scope **之前**检查，所以变量不会在自身 initializer 中可见。显式局部变量类型则在当前 scope 中解析。

### 类型名字解析

`resolve_type_expr` 递归处理 type AST；遇到 identifier 时，`resolve_type_name` 从调用者传入的 scope 查找。顶层 type 的调用者是所属 file scope，局部变量类型的调用者是当前局部 scope。

这也是 FileScope 的关键：未来某文件导入的包名会自然参与该文件的 type、函数签名和 global initializer 查找，而不会泄漏到别的文件。

## 9. 函数、语句与表达式检查

当前 checker 是以递归函数直接遍历 AST 的实现，而不是先构造第二套语义树。常用入口包括：

```text
check_function_bodies
  └─ check_block
      └─ check_stmt
          ├─ check_local_decl / check_local_var_spec
          ├─ check_expr
          │   ├─ check_call
          │   ├─ check_unary
          │   ├─ check_binary
          │   └─ check_selector
          ├─ check_assignment
          └─ check_return
```

函数检查还维护 `ControlContext`：

- `results`：当前函数的结果类型，用于验证 `return`；
- `loop_depth`：验证无标签 `break` / `continue` 是否处在循环内。

返回值函数会检查是否可能从末尾落出；MVP 只支持单一标量返回，不支持 aggregate 返回或多结果作为普通值。

字段选择 `x.f` 会产生 `SemanticInfo.selections`。`Selection` 记录最终 field object、字段 index path 和是否穿过 pointer；当前 MVP 的 path 只有一个 index，因为 embedded/promoted field 尚未实现。

## 10. 当前 MVP 接受的范围

parser 能产生的 AST 比 sema 当前支持的语言大得多。遇到可解析但尚未支持的构造，checker 应报告 `E2405` 或相应类型诊断，并尽量继续检查其他区域。

| 类别 | 当前主要支持 | 当前明确未支持或受限 |
| --- | --- | --- |
| package | 多文件、同 package 名 | 非 `main` package（MVP 限制） |
| import | AST 可解析 | 语义解析、loader、package binding |
| 基本类型 | bool/int/byte | string、float、complex 等 |
| 类型 | named、pointer、非零 array、非空具名 struct、signature | slice/map/interface/channel/generic/embedded field |
| 顶层声明 | const、var、type、func | method 等 |
| 局部声明 | `var` | local const/type、`:=` |
| 表达式 | integer、identifier、pointer、index、field、call、部分算术/比较 | composite literal、func literal、type assertion 等 |
| 语句 | block、var、赋值、inc/dec、call、return、if、条件/无限 for、break/continue | range、switch、select、go、defer、label、三子句 for |
| 全局初始化 | bool/int/byte 常量、nil pointer | aggregate 和运行时求值 initializer |

具体限制会受到 `docs/hir-design.md` 中 V0 HIR 可表达性的约束。sema 的职责是：不把它明知 HIR 无法正确表示的源码状态静默交给 lowering。

## 11. Diagnostics 与错误恢复

diagnostic code 在 `checker.rs` 顶部集中定义，例如：

```text
E2002  duplicate declaration
E2003  invalid package
E2004  mixed package names
E2005  empty package input
E2006  duplicate FileId
E2101  unknown type
E2201  undefined name
E2301  type mismatch
E2401  invalid return
E2405  unsupported MVP feature
```

checker 不会因单个错误停止：

- 无效类型传播 `TypeId::INVALID`；
- 无效 object 使用 `ObjectId::INVALID`；
- statement/expression 尽可能继续遍历子节点；
- `finish` 会排序、去重 diagnostics。

因此调用方应同时查看 `AnalysisResult::has_errors()` 和完整 `diagnostics`。即使有错误，`SemanticInfo` 仍可能包含对 IDE、高亮或后续错误恢复有用的局部事实；但 HIR lowering 应只接受无 error 的 analysis。

## 12. 对外查询 API 的边界

`AnalysisResult` 有意不公开 `TypeArena`、`ObjectArena`、`ScopeArena` 或 name interner 的存储。外部应保存 ID，再通过方法查询：

```rust
analysis.object(object_id)
analysis.name(name_id)
analysis.type_of(type_id)
analysis.tuple(tuple_id)
analysis.package_member("name")
analysis.file_scope(file_id)
analysis.node_at(span)
analysis.underlying_type(type_id)
analysis.identical_types(left, right)
analysis.is_assignable(source, target)
analysis.is_basic_type(type_id, basic)
analysis.deref_type(type_id)
analysis.array_element_type(type_id)
analysis.is_comparable_type(type_id)
analysis.has_errors()
```

这种边界允许未来更换 arena 的内部布局、缓存策略或 interner 实现，而不会要求 driver、lowering 或 IDE 客户端同步改写。

## 13. 修改 sema 时的检查清单

增加一种语法或语义能力时，通常需要沿以下路径逐项确认：

1. parser 是否已经提供所需 AST；若没有，应先修改 parser。
2. 新构造是否需要新的 `TypeKind`、`ObjectKind`、`ScopeKind` 或 `SemanticInfo` 事实。
3. 顶层名字是否必须预收集，以支持前向引用或跨文件引用。
4. 它的类型/表达式解析从哪个 scope 开始；若是文件局部能力，应使用声明所属 file scope。
5. HIR 是否可表达其运行时和 ABI 语义；不能时应在 sema 诊断，而不是留给 backend 猜测。
6. 是否需要对错误恢复、重名、shadowing、递归或初始化环建立负向测试。
7. 是否需要在 `public_interface.rs` 保护新增的公开查询 API。

## 14. Import 与 FFI 的后续落点

FileScope 已经把 import 的正确插入点准备好，但 import 本身仍不属于当前 sema 输入能力。推荐的未来职责分界是：

```text
package loader
  ├─ 根据路径、build constraints 找到文件
  ├─ 解析 import path，构建 package graph
  ├─ 检查循环 import
  └─ 向 sema 提供已解析的 imported package identity / export interface

sema
  ├─ 在每个 FileScope 声明该文件的 import alias / PkgName
  ├─ 检查 selector 的 package 成员选择
  └─ 产出名称、类型、selection 事实
```

FFI 不应仅靠“缺少函数体”这一语法形状得到完整语义。稳定设计至少需要单独明确：链接 symbol、调用约定、平台目标、可传递类型、ownership/escape 规则以及可信 ABI metadata 的来源。V0 不接受 extern；未来必须通过独立的 binding 设计引入。

## 15. 相关文件

| 文件 | 阅读重点 |
| --- | --- |
| `crates/sema/src/checker.rs` | 语义阶段、AST 遍历、诊断和规则 |
| `crates/sema/src/types.rs` | 所有稳定 ID、对象、类型、SemanticInfo |
| `crates/sema/src/scope.rs` | scope tree、declare、lookup |
| `crates/sema/src/symbol_table.rs` | NameId 与 ObjectId 的 arena 管理 |
| `crates/sema/src/interface.rs` | 对外稳定查询面 |
| `crates/sema/tests/public_interface.rs` | 外部调用者可依赖的最小接口 |
| `docs/sema-syntax-checklist.md` | 按当前实现维护的语法支持 checklist |
| `docs/hir-design.md` | sema 到 HIR 的 V0 契约与限制 |
