## 1. 实现

- [x] 1.1 main.rs `MenuState::on_key` 新增 `Keysym::Tab` 分支：把 `matches[sel]` 的 text 填入 query 并 `refilter`，选中锚定回该条目
- [x] 1.2 main.rs 新增 MenuState 单测：方向键移到非首项后 Tab 补全该条目、补全后 Enter 选中该项

## 2. 回归

- [x] 2.1 README 键盘行为补一行 Tab 补全说明
- [x] 2.2 `cargo test` 全绿 + `cargo clippy -- -D warnings` 干净