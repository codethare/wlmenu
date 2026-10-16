# SPEC-font-chain-lazy — 主字体改 mono + 备用面惰性加载（P4）

## 目标 Objective

启动不再为了画英文菜单读 27MB 的 CJK 字体：

1. **链路顺序**：主字体从 Noto Sans CJK 改为 Noto Sans Mono（0.6MB），CJK 降为备用面（顺序不变：mono → CJK → nerd）。
2. **备用面惰性加载**：链路里的面**不在 `load()` 时读取**，而是第一次真的要用到它时才读（按需一次，失败即永久跳过）。
3. 结果：首帧前只读 0.6MB（+ 可选 `-f` 指定的面）。

基线（release、headless sway 空闲，量「进程启动 → 首帧 attach」）：

| 场景 | 中位 |
|---|---|
| P3 之后默认（P3 已把字体与握手重叠） | 23–32 ms |
| `-f FILE`（0.6MB 单面） | 18–28 ms |
| 冷页缓存下 27MB TTC 首读 | +90 ms |

预期：默认与 `-f FILE` 拉平（省 3–8 ms 暖；冷启动省 ~75 ms）；首次出现 CJK 字形时一次性
加 ~15 ms（暖）/ ~90 ms（冷）——这是惰性加载的必然代价，只在真的要画 CJK 时付。

## Scope

- `src/font.rs`：`system_chain()` 改为 mono 优先；`MenuFont` 持有
  `faces: RefCell<Vec<FaceState>>`（index = 链路下标，0 = 主字体，恒为已加载）+ `chain: Vec<(PathBuf, u32)>`
  （备用面的路径，接口与今天 `fallbacks` 一样是「按序尝试」）；新增 `face_count()` / `face(i)`；
  移除 `pub fallbacks: Vec<FontVec>` 与 `pub font: FontVec` 字段（渲染改走 `face(i)`）。
- `src/render.rs`：`draw_text` 与 `measure` 的「遍历链路」改为 `0..face_count()` + `face(i)`。
- `README.md`：自动选字体那行改为「主字体 mono，CJK/图标按需回退」。
- 不改：`row_h`/`line_h` 公式、渲染算法、行高与间距、`-f FILE`（仍恰好一个面、无链路）、
  `-f FAMILY`（仍是主字体 + 系统链路）、wmenu flag 语义、stdin/stdout 契约、P1/P2/P3 行为。

## Design

- **面下标必须稳定**：`GlyphKey` 含面下标，且已加载的面缓存着位图。链路的每个位置对应一个固定下标，
  惰性加载只把该位置的 `Unread → Loaded`，因此字形缓存永远有效。
- **失败不重试**：`FaceState = Loaded(FontVec) | Unread | Failed`；读/解析失败标 `Failed`，
  后续查询直接跳过（否则每个缺字字形都会重试一次失败的 I/O）。
- **顺序不变只换头**：今天解析顺序是 CJK → mono → nerd（主字体 CJK），改后是 mono → CJK → nerd。
  除「谁画拉丁字母」外，符号/图标/中文落到哪个面与今天一致（不能把 nerd 提到 CJK 前面，
  那会改变符号字形的来源）。
- **代价**：nerd 图标字形会先读 CJK（27MB，一次）才能确认 CJK 没有 PUA。见 Open Questions。

## Code Style

零新依赖；`FaceState` 是 `font.rs` 内的私有枚举；`font.rs` 保持 Wayland 无关、可单测；
渲染仍是 CPU/wl_shm，不引 cairo/pango。

## Test Strategy

- 新增 `the_cjk_face_is_not_read_before_a_cjk_glyph`：`MenuFont::load(None, 16)` 后
  `unread_faces() == face_count() - 1`（启动只读主字体）；`has_glyph('中')` 之后减少。
- 既有 `fallback_chain_covers_cjk_for_latin_primary` / `chain_renders_nerd_font_pua_icons` /
  `fallback` 渲染测试现在**顺带覆盖惰性加载路径**。
- `file_spec_loads_exactly_that_font_without_chain` 改为断言 `face_count() == 1`。
- `row_h_for` 不变量与 `spec_size` 断言不变（P3 已锁）。
- 端到端：headless sway 量首帧（预期与 `-f FILE` 齐平）；`-p 中文` 与 `printf '中文\n'` 各跑一次，
  断言不 panic、第一帧后 CJK 面被加载（首帧耗时会比纯 ASCII 多 ~15 ms）。

## Boundaries

- Always：`cargo test` 全绿；`-f FILE` 仍无 fallback；CJK 与 Nerd 图标仍能渲染；`cargo fmt/clippy` 无新增。
- Ask first：改链路顺序（本 SPEC 已获批，只允许 mono 提到最前）；改主字体的**样式**（如换 Noto Sans 非 mono）。
- Never：把 CJK 面重新变成启动必读；把失败的面反复重读；为惰性加载引入后台线程/异步重绘。

## Success Criteria

- [x] 默认路径首帧与 `-f FILE` 差距 ≤ 2 ms：默认 17–31 ms（中位 20）vs `-f FILE` 15–20（中位 19）；
      改前默认 21–32 vs FILE 18–28（中位差 ~3 ms）
- [x] `load(None, 16)` 只读 1 个面（`the_cjk_face_is_not_read_before_a_cjk_glyph` 断言 2 unread → 1）
- [x] `-p 中文`、CJK 列表项、Nerd 图标都不 panic（`renders_cjk_via_fallback_when_primary_is_latin`
      与 `chain_renders_nerd_font_pua_icons` 覆盖解析；端到端 `printf '中文一\n' | wlmenu -p 中文` 无 panic）。
      惰性代价：首帧就要画 CJK 时 +4–8 ms（21–36 → 25–46）
- [x] `-f FILE` 仍恰好 1 个面（`file_spec_loads_exactly_that_font_without_chain` 改断 `face_count()==1`）；
      `-f FAMILY` 仍是主字体 + 系统链路
- [x] 行高/间距/颜色/几何断言全绿（87 passed）
- [x] 全量 test/fmt/clippy 无新增；README 同步

## 实施中发现（重要）

缓存键只看字体目录 mtime，**看不到解析算法变化**：改动链路顺序后，旧缓存（CJK 在前）仍被判为
“fresh”，P4 会静默失效（实测：主字体仍是 27MB TTC，首帧 21–32 ms）。修法：缓存里加一行
`p\t{CACHE_POLICY}`（当前 2），策略号参与键校验。策略不匹配 → 走 P1 的 serve-stale：
本次启动仍用旧链（不额外扫描、不额外延迟），后台重扫写回新链，**下次启动生效**。
实测：把旧格式（v1、无 p 行、CJK 在前）缓存放回去 → 首帧 22–36 ms（无同步扫描），
一次长活实例后缓存自动变为新格式且 mono 在前。

## Open Questions

- nerd 图标会先读 27MB CJK 一次：把 nerd 提到 CJK 之前可免，但会改变符号字形来源（Ask first）。
- P3b（进程内字节缓存）：惰性之后，`apply_scale` 的第二次 `load` 不再读 27MB，收益进一步缩小。
