# Delta: enter-echo → spotlight-ui

## ADDED Requirements

### Requirement: 无匹配时 Enter 回显输入

查询非空但列表无匹配项时,Enter SHALL 把当前输入文本作为选择结果输出(dmenu/wmenu 契约,支持 `echo "" | wlmenu` 式纯输入框脚本)。空查询 Enter 取消的既有行为不在此列。

#### Scenario: 纯输入框脚本回显

- **WHEN** 条目列表为空或输入不匹配任何条目
- **AND** 用户输入文本后按 Enter
- **THEN** wlmenu 输出输入的文本作为结果并退出(exit 0)

#### Scenario: 有匹配时不变

- **WHEN** 输入匹配若干条目且高亮其一
- **AND** 用户按 Enter
- **THEN** wlmenu 输出高亮条目的值