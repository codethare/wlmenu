# SPEC-output-select-fix — `-o NAME` 实际不可用

## 目标 Objective

修复 `wlmenu -o <name>` 恒失败：找不到屏幕名 → `wlmenu: no output named <name>` + exit 1。

现状（headless sway，2 输出，`WAYLAND_DEBUG` 取证）：

- 合成器**发来了** `<- wl_output@7.name (Some("HEADLESS-1"))`，但 `OutputState::info()` 仍是 `None`。
- 根因：`main.rs` 用 `Connection::roundtrip()`（wayland-client `conn.rs:148`）等输出信息，
  它只 dispatch 连接的**内建**队列；sctk 的 `OutputState`（`OutputHandler` + `Dispatch2`）住在
  `registry_queue_init` 返回的那个 `EventQueue` 里，那个队列此时还没被 dispatch 过。
- 所以 `.find(|o| info(o).name == name)` 必然空 → 必失败（不是偶发）。

## Scope

- `src/main.rs` 启动段重排：`App` 先构造（`layer: None`），`-o` 用
  `event_queue.roundtrip(&mut app)` dispatch，再查名；layer surface 在此之后创建并赋给 `app.layer`。
- `WaylandSource::new(conn, event_queue)` 与单实例 listener 的 `insert_source` 移到该 roundtrip 之后
  （`event_queue` 被 WaylandSource 消费，且此时循环尚未启动）。
- 不改：`-o` 的 CLI 语义、未知名 → exit 1 的行为、其他 flag、字体/P3、缓存（P1/P2）。

## Code Style

保持单文件、零依赖；新增注释只解释「为什么必须先 dispatch 再查名」。

## Test Strategy

- 无可合成器单测可写（需真实 wl_output）；`-o` 的解析单测保持不变。
- 端到端（headless sway，**两个输出** HEADLESS-1/HEADLESS-2）：
  `wlmenu -o HEADLESS-1` → 用 `swaymsg -t get_tree` 断言 layer surface 挂在 HEADLESS-1 下；
  `wlmenu -o nope` → exit 1 + `wlmenu: no output named nope`；
  不带 `-o` → 行为与改前一致（不额外 roundtrip）。

## Boundaries

- Always：`cargo test` 全绿；未知名退出码 1 与文案不变；不带 `-o` 时不引入额外 roundtrip。
- Never：为了 `-o` 拖慢默认路径（例如无条件 roundtrip）。

## Success Criteria

- [x] `wlmenu -o HEADLESS-1` 在两个输出的合成器上把面板挂到 HEADLESS-1
      （取证：`<- wl_surface@14.enter, (wl_output@8)`，而输出名事件里 @8 = HEADLESS-1；
      `-o HEADLESS-2` → `enter(wl_output@7)`）
- [x] `wlmenu -o nope` → exit 1 + `wlmenu: no output named nope`（且在创建 surface 之前退出）
- [x] 默认路径首帧时间与改前一致：默认 17–31 ms（中位 20，与 `-f FILE` 的 15–20 齐平）；
      `-o` 才多 1 次 `wl_display.sync`（实测 extra_roundtrips=1），不带 `-o` 为 0
- [x] 全量 test 87 passed / fmt clean / clippy 无新增

## Open Questions

- 无。修法（dispatch 真队列 vs 移到 App 之后）在本 SPEC 内选了后者，因为 `OutputState` 归 `App` 所有。
