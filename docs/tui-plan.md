# rvdbg 终端界面（TUI）方案

2026-11-26 · 已完成重构并作为交互终端下裸 `rvdbg` 的默认界面（默认全屏 `Altscreen`，支持 `--inline` 与 `Alt+F` 无损互切），实机验收待连接 SR6P6 板卡后执行。

## 架构与体验

- **默认入口**：交互终端（`stdin`/`stdout` 均为 TTY 且 `TERM != dumb`，无 `--json` 与子命令）运行裸 `rvdbg` 直接进入全屏无闪烁 TUI；`rvdbg --inline`（或 `--tui=inline`）以动态高度内联模式启动；会话中按 `Alt+F` 或输入 `/inline`、`/fullscreen` 可在保留完整内存历史的情况下即时切换。管道、`rvdbg --json` 与 `TERM=dumb` 继续走 `session.rs` 的纯文本/机器流模式。
- **One Dark 语义配色与结构化排版**：`probecore::output::with_colored_sink` 透传颜色开关，`tui/ansi.rs` 解析 One Dark SGR 序列为 Ratatui `Span`（遵守 `NO_COLOR`）；次要文字与暗色提示使用默认前景色叠加 `Modifier::DIM`（避免 One Dark 下 `DarkGray` 对比度过低）；每轮命令按 `❯ <cmd>` + 2 列缩进输出块展示并自动软折行重排。
- **多行圆角 Composer 与全量补全**：支持 `Ctrl+J` / `Shift+Enter` 换行及多行粘贴（最多 8 行自适应增高）、按词移动/删除、`Ctrl+R` 历史模糊搜索、空行双击 `Ctrl+D` 安全退出，以及长命令运行期的待执行队列预输入；普通命令支持 Ghost text、`Tab` 补全命令名、Clap `--flag`、当前镜像的 ELF 符号及路径；`/` 菜单中无必填参数的命令按一次 `Enter` 直接执行，内置 `/images`、`/backend`、`/fdx` 交互选择器与 `/copy`（OSC 52）、`/clear`、`/mouse`。
- **运行活动条与 FDX 多镜像融合**：命令运行期间在 Composer 上方展示动态 Spinner、阶段计时与 OpenOCD 烧录进度条；按 `Shift+Tab`（或 `F6`）在 `rvdbg ›`（Cyan 框）与 `fdx:* ›`（Magenta 框）间切换输入模式，FDX 模式下支持 `!cmd` 逃逸执行单条调试命令、`@guest` / `:to` / 目标 `help` 词条补全；全屏宽屏（$\ge 100$ 列）下 FDX 以右侧独立分栏展示，窄屏与内联模式下显示为上方可 `Ctrl+O` 折叠/展开的预览抽屉，不再向主滚屏重复刷 `[FDX]`。

## 模块划分

- `probecore::output` / `probecore::style`：捕获输出时保留彩色开关。
- `crates/rustvisor-debug/src/tui/`：`mod.rs`（事件循环与工作线程桥接）、`surface.rs`（全屏 `Altscreen` 与无 CPR 竞态动态高度 `Inline` 同步差分渲染）、`ansi.rs`（SGR 解析与软折行）、`theme.rs`（One Dark 语义色令牌）、`transcript.rs`（结构化 Turn 历史与滚动）、`composer.rs`（多行编辑器与历史搜索）、`complete.rs`（Ghost text、ELF 符号缓存与 `/` 选择器）、`fdx_view.rs`（分栏/抽屉视图）。

## 验收状态

- **主机自动验收**：`cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all`（含模拟 TRACE32 / OpenOCD 的 PTY 端到端回归，覆盖默认全屏、One Dark ANSI 颜色与 `NO_COLOR`、`/` 单次回车执行、普通 `Tab` 补全、多行粘贴、`run-until` 中断/命中/超时、RTT 双向收发、`Shift+Tab` FDX 分栏与 `!cmd` 逃逸、`Ctrl+O` 折叠、`Alt+F` 全屏/内联互切、双击 `Ctrl+D`、`SIGTERM` 恢复、`TERM=dumb` 回退及 `--json` 管道兼容）。
- **上板手工验收清单（待连接 SR6P6 板卡）**：
  1. TRACE32 与 StellarLINK 分别运行裸 `rvdbg` 验证默认全屏启动、One Dark 色彩显示与缩放重排；
  2. 执行 `/images` 切到 `lab` 多镜像目录，`flash` 观察动态 Spinner、烧录进度条与运行期预输入队列；
  3. 执行 `fdx --all` 与 `Shift+Tab` / `Ctrl+O`，给 `el2` 和各 `guest` 发包，用 `!cores` 检查状态，验证 `reset` 关闭链路后 `go` 自动重连；
  4. 执行 `run-until` 后 `Ctrl+C` 中断，确认断点清理与 FDX 轮询恢复；测试 `Alt+F` 切内联模式与 `/copy` 剪贴板复制。
