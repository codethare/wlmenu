# Proposal: tab-complete

## Why

wmenu/dmenu 用户习惯按 Tab 把当前高亮的条目直接补进输入框，减少逐字输入。当前 wlmenu 没有 Tab 绑定，只能靠回车直接选中或手动抄写条目名。

## What Changes

- Tab 键：把当前列表中选中条目的文本填入输入框（query），并重新过滤、保持选中该条目
- 补全后 Enter 依然选中该条目（无多余二次选择）
- 不改变其它按键语义与 CLI 契约

## Capabilities

- **New Capabilities**: 无
- **Modified Capabilities**: `spotlight-ui`（列表与输入栏交互：新增 Tab 补全行为）

## Impact

- src/main.rs：`MenuState::on_key` 新增 `Keysym::Tab` 分支 + 单测
- README.md：键盘行为说明补一行
- 无新依赖、无 API/协议变更