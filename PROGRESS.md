# 项目开发进度

## Milestone

- [x] 加强 IR 逃逸检测（2026-09-29）
  - 用有限对象来源、points-to/内存内容集合和 `Reach` 闭包替换旧 taint 算法。
  - 用 capture、返回来源、外部写入和 local/unknown violation 摘要求解跨函数及递归效果。
  - `verify_and_check_escape` 是唯一上线入口；driver 只把其产出的 `VerifiedIrPackage` 交给 interpreter/codegen。
  - 删除旧算法与预期漏检测试，保留 34 项现行方案的拒绝/接受回归。
  - `cargo test -p gane_ir`、workspace check/test 和 rustfmt 全部通过。

原简化版里程碑保留在 Git 历史中，不以本次完成状态覆盖。
