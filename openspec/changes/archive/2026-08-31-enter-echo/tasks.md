## 1. 实现

- [x] 1.1 main.rs `MenuState::on_key` Return 分支:`matches.get(sel)` 为 `None` 时 `Done::Select(query.clone())`(回显输入),空查询仍 Cancel
- [x] 1.2 main.rs 更新单测:`enter_with_no_match_cancels` → 断言无匹配时 Enter 回显查询文本;补空查询仍取消的新断言(已有测试覆盖)

## 2. 回归

- [x] 2.1 `cargo test` 全绿 + 新增用例
- [x] 2.2 README 无改动;spec delta 同步进 SPEC-spotlight-ui.md