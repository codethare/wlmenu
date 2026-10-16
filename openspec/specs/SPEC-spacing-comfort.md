# SPEC: spacing-comfort — 间距/位置按人眼视觉舒适度调整

## Objective

氛围不变（Spotlight 式面板），只调「间距与位置」——让面板的每个部分遵循人眼舒适的视觉规律：

1. 行距舒适：行高 ≈1.5× 字号，文字上下留白、不拥挤，选中条是可轻松瞄准的目标
2. 内边距舒适：内容内边距 ≥ 圆角半径，文字不贴近圆角弧线；输入栏与列表文字左对齐当轴线
3. 屏边位置：面板与屏幕顶边留足呼吸空间，不与屏边"贴死"
4. 间距节奏统一：按 8px 网格（4/8/12/16/24/32）取值，视觉韵律一致

用户：Wayland 键盘启动/筛选用户。成功 = 观感更松弛、易读、易定位，键盘流程与 CLI 契约不变。

## Commands

- Build: `cargo build`
- Test: `cargo test`
- Lint: `cargo clippy -- -D warnings`

## Project Structure

```
src/font.rs    行距（改：row_h = line_h + 上下内距）
src/main.rs    布局常量（改：PAD 4→12、顶边距 24→32）
src/render.rs  渲染 + 几何测试（改：新增 spacing 舒适规则断言）
README.md      行为说明（不需要改——无外观语义变化）
```

## Code Style

沿用现状：纯常量 + 纯函数、注释一行讲清"为什么"（心理学依据），不引依赖。

## Testing Strategy

render.rs 内联单测：新增 `spacing_follows_eye_comfort_rules`——断言行高 ≥ 1.4×行高、PAD ≥ 圆角半径，防回归。其余既有几何/渲染测试全绿验证无破坏。

## Boundaries

- Always: `cargo test` 通过；不改变颜色、字号、字号 flag 语义
- Ask first: 改默认颜色；加依赖
- Never: 破坏 wmenu flag 语义；删既有测试

## Success Criteria

- [ ] 行高从 16px → 24px（16px 字号），文字上下各 4px 留白，选中条更高
- [ ] 面板内容内边距 4 → 12px，文字不再蹭 10px 圆角
- [ ] 非 `-b` 模式顶边距 24 → 32px；`-b` 底边距 8px 不变
- [ ] 输入栏高度随行高同步（仍是单行输入条，Spotlight 式）
- [ ] 既有 + 新增测试全绿

## Open Questions

假设：不改字号(16)、不改颜色、不引入比例式屏幕定位（顶边距定值 32px 已足够舒适；如需"面板位于屏高 1/3 处"再另行提出）。上述假设有误请指出。