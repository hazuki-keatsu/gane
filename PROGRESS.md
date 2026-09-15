# 项目开发进度

## 说明

下文中提到的设计文档指的是[hir-design](./docs/hir-design.md)

## Milestone

- [x] 补齐 sema 到 HIR 的边界测试
先严格实现设计文档 2.1 的拒绝清单。目标是：只要 AnalysisResult 无 error，lowering 就不应遇到“AST 有、HIR 无法表达”的特性。同时确认 lowering 所需的语义事实都能从公开 API 获得：每个标识符的 def/use、表达式类型/常量、field selection、全局初始化结果等。缺哪个就先扩展gane_sema 的只读查询 API，绝不在 lowering 里重做名字解析或类型推导。

- [x] 完成纯 HIR 数据模型与构造 API
在 gane_hir 内先实现：
  - 强类型 ID（TypeId、ValueId、BlockId 等，0 为 invalid）
  - 私有构造的 TargetSpec
  - 类型 arena、UnverifiedHirPackage / VerifiedHirPackage
  - function / block / instruction / terminator / global / extern
  - builder：负责分配 ID、维护 value 定义位置、构造 block 参数
此阶段不依赖 AST、不依赖 LLVM。优先保证“无法轻易构造错 IR”，但 verifier 仍必须把外部输入当作不可信。

- [x] 先写 verifier 与稳定 HIR printer
这是最值得优先投入的一步。先覆盖设计文档第 14 节中的：
  - ID、类型、符号唯一性；
  - CFG 与 block 参数；
  - dominance 和同 block 的 use-before-def；
  - 所有指令的 operand/result 类型矩阵；
  - call/return、aggregate、extern ABI、非法递归类型等。
同时提供确定性的文本输出，用作 --emit-hir 和 golden test 基础。测试应以“手写 raw HIR → 验证成功/失败”为主；每一条 verifier 不变量至少有一个反向测试。

- [ ] 实现 lowering，但从最小垂直切片开始
不要一次性覆盖整份 V0。建议顺序：
  1. 常量/局部变量/标量表达式
  2. 函数、调用、return
  3. if 与 block parameter
  4. for、break、continue
  5. short-circuit && / ||
  6. 指针、field/index、显式检查
  7. array/struct、AggregateZero/Copy
  8. global 与 extern
第一批就可以支持 func main(){ var x int; x = 1 + 2 }，并把 golden HIR 固定下来。每增加一个语法结构，就同时增加：
  - source → HIR golden；
  - 验证器测试；
  - 关键语义测试（尤其副作用顺序与 trap 路径）。

- [ ] 单独实现 escape check
它不要和普通 verifier 混在一起。先做保守版本：
  - 标记 StackAddr 为 stack-derived；
  - 穿透 GEP、slot store/load 和内部函数参数；
  - 拒绝 return、写 global、传给未知 extern；
  - 对内部调用图做 noescape summary 的不动点求解。
verify_and_check_escape 只有两类检查都通过才返回 VerifiedHirPackage。

- [ ] 先做 interpreter，再做 LLVM backend
interpreter 是 HIR 设计的安全网：可先实现整数、CFG、stack object、trap、aggregate copy/zero，不需要真实物理布局或 extern。之后 LLVM codegen 只接受 VerifiedHirPackage，实现 hosted main wrapper，并逐步用“interpreter 与生成可执行文件的差分测试”验证除零、MIN / -1、越界、负/超宽 shift、null 等最危险的语义。

## 建议

建议的 crate 内部模块边界：

```plaintext
hir/
id.rs       target.rs    types.rs     ir.rs
builder.rs  verify.rs    printer.rs
lower.rs    escape.rs    interpreter.rs
```

最关键的工程纪律是：

- codegen/interpreter 的入口类型只接受 VerifiedHirPackage；
- lowering 只消费 AST + AnalysisResult，不自行推导语义；
- 先用 stack slot 完成源码局部变量，暂不做 HIR mem2reg；
- 所有危险操作在 HIR 层定义 total semantics，不能把正确性押在 LLVM 不产生 poison 上；
- 每个阶段都能独立验收并提交，避免形成一个无法调试的大分支。
