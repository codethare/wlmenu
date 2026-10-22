# CLAUDE.md — wlmenu 开发约束

约束 AI 在 wlmenu 上的改动，保持产品风格与代码惯例。

## 项目是什么

- wmenu/dmenu 风格 Wayland 动态菜单（wlr-layer-shell），Rust，edition 2024
- 纯软件渲染：wl_shm + ab_glyph，**禁止引入 cairo/pango 等渲染依赖**
- 契约：stdin 读列表 → 筛选/选择 → 选中项打印 stdout（`--run` 模式除外）

## 产品哲学

1. **wmenu 兼容子集**：`-b -P -i -l -W -p -f -v`、颜色 flag `-N -n -M -m -S -s` 语义不得改变；无匹配 Enter 回显输入、Tab 补全、Ctrl-* 快捷
键均为 dmenu/wmenu 契约，硬边界
2. **Spotlight 风格**：圆角、启动仅显示单行输入栏、输入驱动列表展开、清空收起、非 `-b` 时上中居中
3. **键盘优先**：所有操作可纯键盘完成；已定义的快捷键不增删
4. **零依赖**：能用现有代码/标准库解决就不加依赖（新增依赖见 Boundaries）
5. **人眼舒适**：行高 ≈1.5× 字号、内容内边距 ≥ 圆角半径、间距遵循 8px 网格（8/12/24/32）
6. **CJK**：字体回退保留 CJK 渲染能力

## 代码风格

- 单文件模块（desktop.rs / font.rs / items.rs / render.rs），纯函数 + 常量，BGRA 颜色常量
- CPU 渲染；只改被要求的部分，不顺手重构

## 开发流程

- **Spec-driven**：需求先落 `openspec/specs/SPEC-*.md`（目标/代码风格/测试策略/Boundaries/Success Criteria），再开工
- 计划与执行：`tasks/plan.md`（依赖顺序、风险、验证检查点）+ `tasks/todo.md` 勾选
- 每个非平凡改动：`cargo test` 全绿 + `cargo clippy` 无新告警；行为变更补断言式单测（不引测试框架）

## Boundaries

- Always：`cargo test` 通过；wmenu flag 语义兼容；键盘流程不变
- Ask first：新增依赖；修改默认颜色值；删除/改写现有测试；改变 stdin/stdout 契约
- Never：破坏 wmenu flag 语义；删现有测试；引入 cairo/pango 类渲染依赖

## 版本控制（jj）

用 **jj (Jujutsu)**，不用 git 命令：

- `jj status` — 工作副本变更；工作副本本身就是一个变更（@），无需 add/commit 两步
- `jj describe -m "msg"` — 写描述（英文短句，风格：`fix:` / `feat:` / `rm` 前缀，参考现有提交）
- `jj new` — 新开变更；`jj diff` / `jj show` 查看内容
- `jj git fetch` / `jj git push` — 同步远程（origin: github.com/codethare/wlmenu）

## 已知限制（不是要修的 bug）

- 无 IME：输入法协议客户端不做
- 列表可见上限 64 行（shm 缓冲有界），`-l N` 控制固定高度
- `--run` 的 xdg-activation token 只带 surface、无 input serial（sctk 的 KeyEvent 不含 serial），要求 serial 的合成器仍可能不聚焦

