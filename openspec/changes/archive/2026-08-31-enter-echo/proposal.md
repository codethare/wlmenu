# Proposal: enter-echo

## Why

`coffice-times` 类脚本用 `echo "" | wmenu/wlmenu -p "提示:"` 空条目表做纯输入框;dmenu/wmenu 契约是无匹配时 Enter 回显当前输入作为结果。wlmenu 目前无匹配 → Cancel(exit 1),脚本拿到空值直接退出,第二个输入框永远不出现。

## What Changes

- 查询非空但无匹配时,Enter 不再取消,而是把当前输入文本回显为选择结果
- 空查询 Enter 取消的既有行为(Spotlight 收起态)不变
- 有匹配时选中高亮项的行为不变

## Capabilities

- **New Capabilities**: 无
- **Modified Capabilities**: `spotlight-ui`(Enter 语义:无匹配回显输入)

## Impact

- src/main.rs:`MenuState::on_key` 的 Return 分支 `None => Done::Select(query)`
- 单测 `enter_with_no_match_*` 语义更新
- 无新依赖