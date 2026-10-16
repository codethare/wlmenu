# SPEC-startup-font-overlap — 字体加载与首帧重叠（P3）

## 目标 Objective

让字体获取不再串在 Wayland 握手之前：面板先用**精确的临时高度**提交 layer surface，
等合成器 configure 的这段时间里字体在后台线程读盘/解析，首帧前 join。

首帧 = `spawn + max(字体, 握手 + 等待)` 而不是 `spawn + 字体 + 握手`。

现状（release、headless sway 空闲、/usr/share/fonts = 655 文件 / 422MB，量「进程启动 → 首帧 attach」）：

| 场景 | 中位 | 尾部 | 分解 |
|---|---|---|---|
| `-h`（只 spawn + parse_opts） | 7.5 ms | 23 ms | spawn |
| 默认（27MB CJK TTC 主字体 + 0.6MB + 2.5MB fallback） | 26–32 ms | 41–82 ms | +字体 ≈15 ms（冷读 90 ms） |
| `-f FILE`（0.6MB 单面） | 17–23 ms | 30 ms | 字体 ≈2 ms |
| Wayland 握手（首个协议请求 → 首帧 attach） | 8 ms | 14.5 ms | 其中提交后等待 configure 为主 |
| 提交 → 首次 attach 的间隙 | 2.1–2.7 ms | — | 今天的「无 buffer 已提交」窗口 |

预期：空闲省 6–8 ms（26–32 → ~20–24）；合成器繁忙（configure 慢）时字体**全部**被隐藏。
**冷缓存只省 ~6 ms**：字体字节仍必须在首帧前读完，砍掉 27MB 冷读要靠 P4（改主字体为 mono），不在本 SPEC 内。

## Scope

- `src/font.rs`：新增纯函数 `row_h_for(size)`、`spec_size(spec, default)`；`MenuFont::build` 复用 `row_h_for`；新增 `FontLoad` 类型别名与 `spawn_load(spec, size)`；`write_cache` 的 tmp 名带 pid。
- `src/main.rs`：启动段起字体线程、用 `row_h_for` 提交；`App.font: Option<MenuFont>` + `App.pending: Option<(f32, FontLoad)>`；`draw()` 内 join（唯一 join 点）；`apply_scale` 改为异步重载。
- 不改：渲染、几何、颜色、行高公式、wmenu flag 语义、stdin/stdout 契约、P1/P2 的缓存策略。

## 设计不变量（为什么临时高度是精确的）

1. `row_h` 与字体面无关：ab_glyph 0.2.32 `ScaleFont::height()` 返回 `self.scale().y`（即请求的 `PxScale`），
   所以 `row_h = size.ceil() + 2*(0.25*size).round()` 只由字号决定（已核 `scale.rs:122`）。
2. 字号不碰磁盘即可算出：`parse_spec()` 已是纯函数（裸数字 = pt×96/72，`Npx` = 像素）；
   只有 `-f <可读文件>` 走文件分支、字号取默认值。
3. `spec_size()` 与 `load()` 必须同一个答案：`spec_size` 用 `Path::exists()` 复刻 `load()` 的
   `fs::read(s).is_ok()` 分支（**不读文件**，否则等于把加载搬回主线程）。
   唯一偏差：文件存在但不可读/不可解析 —— 此时 `load` 落到 family 分支，尺寸可能差一行；
   由 `draw()` 每帧的 `set_size` 自愈（最多一次动画起点偏差）。
4. 单一来源：`row_h_for` 同时被 `build()` 与启动段使用；不变量测试锁死
   `MenuFont::load(..).row_h == row_h_for(size)`。将来谁把 row_h 改成依赖 ascent/descent，测试立刻红。
5. join 点唯一：`draw()` 顶部。首帧前没有任何绕过它的取字体路径（`visible`/`streaming`/`tick` 都不需要字体）。

## 为什么必须先提交再 join

实测：首个协议请求到提交只有约 1–2 ms，而提交到首次 attach 是 6.5–14.5 ms（等 configure）。
所以「起线程 → 提交 → 等 configure → 在 draw() 里 join」才吃到重叠；
若 join 放在 `set_size` 之前（提交前），重叠窗口只剩 1–2 ms，收益归零。

## Code Style

零新依赖；纯函数 + 常量；线程只用 `std::thread`；Wayland 相关代码不进 `font.rs`（保持可单测）。

`apply_scale` 语义保持：重载失败时**保留旧字体**（旧代码 `if let Ok(font) = load(..)`）；
只有「首帧前还没有任何字体」时失败才 `eprintln! + exit(1)`（与旧的连接前加载错误契约一致）。
被弃用的 handle 直接 drop（分离，不 join），避免 HiDPI 上等一次注定要丢的 scale-1 加载。

## Test Strategy

- `row_h_for(size)` 纯函数断言；不变量测试：真实字体（`system_chain()` 首个 + family spec 带字号 token）下
  `MenuFont::load(..).row_h == row_h_for(size)`。
- `spec_size(spec, default)` 断言：`None` / family+`20px` / family+裸数字(12→16px) / 可读路径（取默认值）；
  并与 `MenuFont::load(..).size` 对照（同一 spec 两边必须相等）。
- 线程与 join 属事件循环代码，只能用 headless sway 计量（见 Success Criteria）；不引测试框架、不加测试用 env 开关。
- `cargo test` 全绿 + `cargo fmt --check` + `cargo clippy` 无**新增**告警（render.rs 存量 19 条与父提交一致）。

## Boundaries

- Always：`cargo test` 全绿；wmenu flag 语义不变；键盘流程不变；stdin/stdout 契约不变；`-f` 错误路径的
  退出码 1 与 `wlmenu: <原因>` 文案不变。
- Ask first：改 `row_h` 公式；让首帧渲染依赖线程时序（不允许）；给 `font.rs` 加新依赖。
- Never：把字体字节重新搬回提交前；在 `draw()` 之外 join；用 `unwrap()` 吞掉加载失败继续渲染。

## Risks

| 风险 | 后果 | 缓解 |
|---|---|---|
| 错误路径时序变化 | `-f` 无效时今天在连接前 exit 1（合成器看不到窗口）；改后 surface 已提交才报错 | 无 buffer 的 layer surface 不可见（今天本就有 ~2.4 ms 这种窗口）；退出码/文案不变 |
| 键盘独占窗口变长 | 提交到首次 attach 从 ~2.4 ms 变为 ~2.4 ms + 字体剩余时间（最坏 ~10 ms） | 既有语义，只是变长；`KeyboardInteractivity::Exclusive` 不动 |
| 加载线程 panic | 旧代码 panic 直接杀进程；分离的 handle 会吞掉 panic | `draw()` 的 `join()` 必须 `Err` 当致命（首帧前）或保留旧字体（重载）；不得静默继续用空字体 |
| 并发写缓存（P1 重扫线程 + HiDPI 重载） | 两个 writer 写同一 `*.tmp` 可产生撕裂文件 | tmp 名带 pid（本 SPEC 内，1 行） |
| 临时高度漂移 | 若 row_h 改为依赖 face 度量，提交高度错 | `row_h_for` 单一来源 + 不变量测试；`draw()` 每帧 `set_size` 自愈 |
| `MenuFont` 是 `Send` 但 `!Sync`（内含 `RefCell<HashMap>`） | 不能共享，只能 move 回来 | 只用 JoinHandle 传所有权 |
| 顺序约束 | `control::claim()` 必须在起线程之前，否则重复启动退化成「先载字体再被 dismiss」 | 代码顺序：claim → spawn_load → connect |

## Success Criteria

- [ ] 默认路径首帧中位 ≤ 24 ms（改前 26–32 ms），且 `-f FILE` 与默认的差值缩小（字体被握手遮住）
- [ ] `WAYLAND_DEBUG` 的「提交 → 首次 attach」间隙 ≈ 2.4 ms + 字体剩余时间（证明提交先于 join）
- [ ] `-f "No Such Font"` → `wlmenu: font family not found: No Such Font` + exit 1，无窗口可见
- [ ] `-f "Noto Sans Mono 20px"` 首帧高度 = `row_h_for(20)`（不出现 16px 的一帧）
- [ ] HiDPI（scale 2）：`set_buffer_scale(2)` 生效、无 panic；scale-1 的加载不再被等待（handle 直接丢弃）；
      旧字体重载失败时仍保留旧字体（与改前一致）
- [ ] 不变量 + `spec_size` 断言全绿；`cargo test` 84 → 86；fmt clean；clippy 无新增
- [ ] P1/P2 行为不变（陈旧缓存仍 serve-stale；`-f FAMILY` 仍命中缓存）

## 附带发现（不属本 SPEC 范围，未修）

验收时发现 **`-o NAME` 目前完全不可用**：`wlmenu -o HEADLESS-1` 总是 `wlmenu: no output named HEADLESS-1` + exit 1，
即使合成器已把 `wl_output.name` 发来（`WAYLAND_DEBUG` 可见 `<- wl_output@7.name (Some("HEADLESS-1"))`）。
根因不在本 SPEC：`main.rs` 用 `Connection::roundtrip()`（wayland-client `conn.rs`）等待输出信息，
而它只 dispatch 连接内建队列，不 dispatch `registry_queue_init` 返回的那个 `EventQueue`
（sctk 的 `OutputState` handler 在该队列里），所以 `OutputState::info()` 仍是 `None` → 名字查不到。
修法方向：把输出解析改为 `event_queue.blocking_dispatch(...)`（或把 `-o` 解析移到 `App` 构造之后），
但需要 dispatch 目标，属独立改动。

## Open Questions

- P3b：进程内字体字节缓存（`Arc<Vec<u8>>` per path+index）让 HiDPI 第二次加载变成 memcpy 而非再次读盘
  （~20 行）——本 SPEC 不做，视 scale 2 实测再定。
- P3c：`-o NAME` 时可在 roundtrip 后、提交前就知道输出 scale，从而一开始就按该密度加载（省掉 scale-1 的废载）。
  多显示器默认路径不敢猜（猜错就是一次多余加载）——本 SPEC 不做。
