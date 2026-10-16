# Delta: tab-complete → spotlight-ui

## ADDED Requirements

### Requirement: Tab 补全选中条目

输入过程中，Tab 键 SHALL 把当前选中条目的显示文本填入输入框（query），并重新过滤；补全后该条目 MUST 保持为当前选中项。列表为空（无匹配）时 Tab SHALL 不修改输入。

#### Scenario: Tab 补全当前选中项

- **WHEN** 用户输入关键字、列表出现且高亮某条目
- **AND** 用户按下 Tab
- **THEN** 输入框文本变为该条目文本
- **AND** 列表重新过滤后该条目仍是选中项

#### Scenario: 方向键移动后 Tab 补全

- **WHEN** 用户用方向键把高亮移到列表中第 N 个条目（非首项）
- **AND** 用户按下 Tab
- **THEN** 输入框文本变为第 N 个条目的文本
- **AND** 补全后该条目仍是选中项，Enter 选中它

#### Scenario: 无匹配时 Tab 无副作用

- **WHEN** 查询无任何匹配（列表为空）
- **AND** 用户按下 Tab
- **THEN** 输入框文本不变