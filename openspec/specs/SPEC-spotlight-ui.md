# SPEC: spotlight-ui — macOS Spotlight 风格界面

## 目标 Objective

把 wlmenu 的默认观感从 wmenu 式"启动即显示列表"改为 macOS Spotlight 式：

1. 面板四角圆边（rounded corners）
2. 启动后只在屏幕中上方显示一行输入栏（不显示列表）
3. 输入关键字后，列表出现在输入栏正下方（过滤沿用现有 wmenu 规则）
4. 列表与输入栏背景色有细微差异——独立分割、又同属一个面板
5. Tab 补全：Tab 把当前高亮条目的文本补进输入框并保持选中（dmenu 习惯，列表隐藏时补首个匹配项）

用户：Wayland 下用键盘启动/筛选/选择的用户。成功 = 观感贴近 Spotlight，键盘操作流程与 CLI 契约不变（stdin 打印所选值）。

## Commands

- Build: `cargo build --release`
- Test: `cargo test`
- Lint: `cargo clippy -- -D warnings`（可选，不改 Cargo 配置）

## Project Structure

```
src/main.rs    Wayland/window/键盘/布局（改：收起/展开、水平居中、空查询 Enter 取消）
src/render.rs  软件渲染（改：圆角 span 填充、默认色微调）
README.md      行为说明补充一行（现有未提交改动不动）
tests/         无外部测试目录；单测内联各文件 #[cfg(test)]（headless 可跑）
```

## Code Style

沿用现状：零新依赖、纯函数 + 常量、BGRA 常量、单文件模块。渲染保持 CPU/wl_shm，不引 cairo/pango。

## Testing Strategy

- render.rs：`rounded_span` 纯函数单测；`draw` 集成测试（角像素 alpha=0、面板内部不透明、输入行与列表行背景色不同）
- main.rs：MenuState 单测（空查询 Enter → Cancel；空查询不渲染行）
- 全部 `cargo test` 通过方可继续

## Boundaries

- Always: `cargo test` 通过；`-b`/`-l`/`-W`/`-i`/`-P`/颜色 flag 行为兼容
- Ask first: 修改默认颜色值；增加依赖；删除/改写现有测试
- Never: 破坏 wmenu flag 语义；删现有测试

## Success Criteria

- [ ] 面板四角圆角：角像素 alpha=0，角内边界不透明（r = min(10, h/2)）
- [ ] 查询为空时表面高度 = 1 行输入栏；输入后列表在输入栏正下方展开；清空查询收起回单行
- [ ] 输入栏与列表默认背景色存在细微差异、无间隙
- [ ] 非 `-b` 模式面板水平居中于上中部（顶边距 24px）
- [ ] 空查询时按下 Enter → 取消（无可视选项）
- [ ] 无匹配时 Enter → 回显当前输入作为结果（dmenu/wmenu 契约，支持 `echo "" | wlmenu` 纯输入框脚本）
- [ ] Tab 补全：高亮条目文本填入输入框并保持选中；无匹配时 Tab 无副作用
- [ ] 现有 + 新增测试全绿

## Open Questions

见评审说明中的假设清单。默认取值：圆角半径 10、顶边距 24、输入栏默认背景 `#2e2e2e`（列表不变 `#222222`）、选中行默认色保留 wmenu 青绿 `#005577`。