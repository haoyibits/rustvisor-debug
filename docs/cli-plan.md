# rvdbg 交互界面和 fdx 的优化计划

状态：三批都已实现（2026-11-25，未提交，版本号仍是 0.1.35）。
`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
`cargo test --all` 都通过。2026-11-25 上板（TRACE32，lab 固件
`latency/20261124T141143.392Z-45243/00-inject-five`，用 `--images` 烧录，
`verify` 6 个镜像一致）：

- `reset` 关掉 6 条链路；`go` 两次，第二次 `already running`；
- `source` 一个文件跑完 lab 流程（`reset`、`go`、`lab five inject trace`、
  五个 Guest `run timer`、`cores`），提示符显示 halted/running；
- 伪终端里的 `fdx --all`：带链路名的包，`@`/`:to`，EL2 等 `lab` 时 3 秒后
  的诊断（"el2 is open and its last packet was "ready""），`--guest 2` 不发送，
  对 EL2 发 `help` 1 秒后提示未读，`:last`，再进 `--guest 1` 时显示之前的包；
- `cores` 读出真实的停止记录（见下）；
- SDK 回归 `cargo board run smoke ipc`（`RVDBG=` 本地构建）7 个用例全过，
  证据 `run/20261125T101408.384Z-24579`。

上板发现：

- 板子刚上电后，Guest 还没初始化过的 RAM 在复位时写不进（guest1 的链路
  0x60420100 报 bus error），读出来也可能是任意值。`reset` 因此改成逐条关，
  写不进的列在 "not closed"；`cores` 把读不成链路的显示为 "not a link yet"。
- 同一次上电后的第一次 lab 运行，`cores` 显示 C0C0、C0C1、C1C0 都停在
  `Fault: Guest Other fault, pc 0x2801x020 (_start), syndrome 0x92000251,
  StopGuest`，和 2026-11-24 手动测试时 Guest 0 的 SError 一样；C1C1、C2C0
  正常，复位后再跑一次也正常。原因还没查（SDK 侧）。

和计划不同的地方：

- 3.2：`reset` 关掉的链路写在 stderr（`[rvdbg] closed 6 FDX link(s): el2
  guest0 …`），`--json` 的输出不加字段，仍是一个 JSON 值。runner 只看
  exit code，不受影响。
- 3.5：会话也记住 `fdx --send/--expect` 收到的包；`reset`、`flash`、
  `images <dir>` 和 `fdx --close` 清掉相应的记录。
- 4.2：`fdx` 的历史存在 `.rvdbg/fdx-history`，所有链路共用一个文件。
- 4.3：`--guest` 接受 `el2`、`guest<n>` 和 `<n>`；没有另加 `--image`。
  `swtrace --guest` 仍只接受编号。
- 4.4：TRACE32 提示符的 running/halted 只在会话已有 PowerView 连接时显示
  （不为提示符去连）；用 tracebridge 新加的 `Session::running`。
- 5.2：`cores` 只读停止记录和 FDX 链路。每个核是否已启动、跑哪个 VM 要按
  DWARF 读 `PLAN`/`PER_CORE`，没有做。
- 另外：`go` 在 TRACE32 上由 tracebridge 的 `debug go` 处理（同样适用于
  tracebridge 自己的 CLI）；停止记录的解码在 `rustvisor_debug::stop`，
  测试用的是 SDK `stop_decode.py --check` 的同一组例子。

## 1. 起因

2026-11-24 在板上手动跑 rustvisor-sdk 的 `lab` 固件（`hypervisor
--features lab`，五个 command Guest），只用 rvdbg 的交互界面：
`reset`，关掉 6 条 FDX 链路，`go`，给 EL2 发
`lab five inject trace`，再给每个 Guest 发 `run <场景>`，最后跑 `swtrace`。
过程中遇到的问题：

| 现象 | 原因 |
|---|---|
| 一次粘贴 6 行，报 `unexpected argument 'fdx'` | rustyline 默认开着 bracketed paste，粘贴的换行留在同一行里；`split_line` 把换行当空白，6 行成了一条命令的参数 |
| 单独输入 `--close`，打印 `Usage: rvdbg [OPTIONS] [COMMAND]` 和 `try '--help'` | clap 的原始错误，没有按交互界面的情况改写 |
| 关链路要输入 6 条 `fdx --guest N --close`；在 fish 里照着写的 bash `for` 循环解析失败 | 没有一次关掉全部链路的办法 |
| 在 EL2 的 `fdx` 里输入的 `--guest 0`、`help`、`exit` 都成了发给 EL2 的包 | `fdx` 交互模式没有自己的命令，也不拦截明显不是给目标的行 |
| 这些包发出后什么都没发生，也没有提示 | EL2 回 `ok lab` 后就不再读这条链路，包一直留在 ring 里 |
| 第二次进 `fdx` 看不到 `ready`，不知道 EL2 在等命令 | `ready` 上一次已经读走了，交互界面没有记下来 |
| 看到 `ready` 就按了 Ctrl-C，之后 `fdx --guest N` 一直显示 `waiting for the target to open the link` | EL2 还在等 `lab …`，别的核和 Guest 都没启动；rvdbg 只说在等，不说可能的原因 |
| 另一次 Guest 0 的链路一直没打开 | C0C0 停在 `__armv8r_park`：Guest 0 一到 `_start` 就收到 SError。要 `halt` + `fault` 才看得出来 |
| `fdx` 里按方向键输出 `^[[A^[[B` | `fdx` 交互模式按字节读 stdin，没有行编辑 |
| 对正在运行的目标 `go` 返回 exit 1 | `target running (command: Go)` 被当成错误 |
| 在 fish 里要 `set -x` 好几行 `ELF`/`EXTRA_ELF`；进入界面后看不出用的是哪套镜像 | 镜像只能来自 `rvdbg.toml` 或环境变量；第一屏只显示配置文件 |
| `swtrace --html` 要带 10 个 `--map` | SDK 的 `rvdbg.toml` 没有 `[swtrace] map` |
| TRACE32 的提示符一直是 `rvdbg [trace32]>` | OpenOCD 的提示符有 running/halted，TRACE32 没有 |

## 2. 约束

- runner（rustvisor-sdk `tools/board-tests`）用 `rvdbg --json` 管道会话：
  `ready` 行、每条命令后的 `end` 行、`reset`/`go`/`fdx --address … --close`/
  `fdx --send … --expect …` 和它们的 JSON。这些只能兼容地改：JSON 只加字段，
  管道模式仍然一行一条命令。
- 只有 `rustvisor-debug` crate 知道 rustvisor 的约定（`RUSTVISOR_FDX`、
  `RUSTVISOR_STOP_RECORDS`、镜像目录的样子）；`probecore`、`tracebridge`
  不加板子或 rustvisor 的知识。
- tracebridge 自己的 REPL（`tracebridge/src/debug/repl.rs`）不在这个计划里。

## 3. 第一批：今天直接卡住的地方

### 3.1 多行输入

- 交互界面（终端）收到含换行的输入时按行拆开，依次执行。每行执行前先连同提示符
  显示一遍，看起来和逐行输入一样。
- 某一行失败（exit code 非 0 或用法错误）就停下，打印"第 3 行失败，后面 3 行没有
  执行"。
- 以 `\` 结尾的行和下一行合成一条命令（rustyline `Validator`，提示符换成 `… `）。
- 不支持 `;` 分隔：`;` 在 `cmd`/`eval` 的 PRACTICE 文本里是注释。
- 新增会话命令 `source <文件>`：按同样的规则执行文件里的行，`#` 开头的行是注释，
  空行跳过，路径相对当前目录。手动跑的步骤可以写成文件反复用（例子见第 5 节）。
- 管道模式不变：本来就是一行一条命令。

测试：`handle_input("a\nb\nc")` 的执行顺序和失败即停；`\` 续行；`source` 文件的
注释、空行和失败行号。

### 3.2 `reset` 关掉所有 FDX 链路

- `reset` 之后芯片停在复位向量，正是 `fdx --close` 要的时机。`reset` 在成功后
  对 ELF 和每个 extra_elf 找 `[fdx] symbol`（默认 `RUSTVISOR_FDX`），找到的都
  关掉，打印 `closed 6 FDX links`；没有这个符号的镜像跳过，不算错。
- `reset --keep-links` 不关。
- 另加 `fdx --close --all`：不复位，只关全部链路。
- `--json`：`reset` 的输出加 `fdx_closed: [{image, symbol, address}]`。runner 自己
  按地址关链路，重复关没有影响。

### 3.3 `fdx` 里的误发保护和本地命令

- `:` 开头的行是 `fdx` 自己的命令，不发给目标：
  - `:q` 离开 `fdx`；
  - `:last` 列出这条链路最近收到的包（3.5）；
  - `:help` 列出这些命令。
- `exit`、`quit` 离开 `fdx`，并说明"离开 fdx，回到 rvdbg"。rustvisor 镜像的命令集
  里没有这两个词。
- 以 `--` 开头的行不发送，提示"这一行会发给目标，看起来是 rvdbg 的选项；Ctrl-C
  或 `:q` 回到 rvdbg"。
- 真要发一个以 `:` 开头的包，写成 `::…`，发出去的是 `:…`。

### 3.4 目标不读、不回复时提示

- 一个包发出（写进 ring）后 1 秒还没被目标读走：
  `[fdx] the target has not read "lab …" for 1 s: it may not serve this link now`。
  之后每 10 秒提醒一次，被读走时说一声。
- 包被读走后 2 秒没有收到任何包：`[fdx] no reply yet`，只说一次。
- 实现：`probecore::fdx` 已经能读 `to_target` 的读写位置；这里只是在交互循环里
  记下每个包的发送时间并比较读位置。

### 3.5 记住每条链路收到过的包

- 交互界面按链路地址保存最近 20 个收到的包和收到的时间（只在会话内，不写盘）。
- 再进入同一条链路的 `fdx` 时先打印：`[fdx] earlier on this link: < ready (35 s ago)`。
  最后一个包是 `ready` 或以 `error ` 开头时加一句"目标在等命令"。
- 目标复位后（链路关闭再打开）清空这条链路的记录。
- 单独运行的 `rvdbg fdx`（不在会话里）没有这份记录。

### 3.6 对运行中的目标 `go` 不再报错

- 已在运行时打印 `already running`，exit 0。runner 只在 `reset` 之后
  （芯片停着）调用 `go`，不受影响；实现前再查一遍 runner 的调用。

### 3.7 交互界面里的用法错误

- 第一个词以 `-` 开头：`options go after a command, e.g. 'fdx --close'; 'help'
  lists the commands`。
- clap 的其他用法错误：保留第一行错误，把 `Usage: rvdbg …` 换成这条命令自己的
  usage，把 `try '--help'` 换成 `'help <command>'`。

## 4. 第二批：手动测试体验

### 4.1 一个 `fdx` 看所有链路

- `fdx --all`：轮询 ELF 和每个 extra_elf 的链路。
  - 收到的包带前缀：`[el2] < ready`、`[guest0] < ok timer`；
  - 链路打开、关闭也显示：`[guest1] link opened`；
  - 输入发给提示符里的当前链路（`el2> `）；`@guest0 run timer` 只这一行发给
    guest0；`:to guest1` 切换当前链路。
- 链路名：ELF 是 `el2`，extra_elf 是 `guest0`…（和 `swtrace.map` 的键一致）。
- 3.3～3.5 对每条链路都适用。
- 今天的流程（选部署、给 5 个 Guest 发场景）在一个窗口里就能做完；哪个 Guest 一直
  没打开链路也一眼就能看到。

### 4.2 `fdx` 的行编辑

- 交互输入改用 rustyline：输入在一个线程里读，目标的包用
  `Editor::create_external_printer`（rustyline 18 有）打印在输入行上方，不再冲掉
  正在输入的内容（替换现在 `Console` 的 `\r\x1b[K` 做法）。
- ↑/↓ 调出历史，按链路分开，存在 `.tracebridge/fdx-history`。
- Tab 补全：补全词来自这条链路上目标对 `help` 的回复（`label: a b c` 的词和
  usage 的第一个词），所以先发一次 `help` 才有补全。
- 管道输入（stdin 不是终端）照旧按字节读，不进 rustyline。

### 4.3 指定镜像

- 全局选项 `--images <目录>` 和会话命令 `images <目录>`：目录里的 `hypervisor`
  （或唯一一个不叫 `guest*.elf` 的 ELF）是 ELF，`guest0.elf`、`guest1.elf`… 按编号
  是 extra_elf，`PROGRAM_NAME` 取 ELF 的文件名。这就是 board-tests 用例目录的样子。
  优先级：`--images` > `ELF`/`EXTRA_ELF` > `rvdbg.toml`。
- 不带参数的 `images`：列出每个镜像、它的链路符号地址，以及链路现在是否打开
  （`AD:` 读，不停核）。
- 启动时的第一屏除了配置文件，再显示镜像来自哪里：`images  ELF/EXTRA_ELF
  (…/00-inject-five)`。
- `fdx --guest` 也接受名字：`--guest guest1` 或 `--image el2`。

### 4.4 TRACE32 提示符显示运行状态

- 用会话共享的 PowerView 连接读一次 `STATE.RUN()`，提示符成为
  `rvdbg [trace32 running]>`/`[trace32 halted]>`；没有连接时不为提示符去连。

### 4.5 swtrace 的 map 写进 SDK 的配置（SDK 的改动）

- rustvisor-sdk 的 `rvdbg.toml` 加 `[swtrace] map`，按五 VM 的 product 表：
  `C0C0 = "VM1"` … `C2C0 = "VM5"`，`guest0 = "VM1:C0C0"` … `guest4 = "VM5:C2C0"`。
  之后 `swtrace --html x.html` 不用再带 `--map`；部署不同时仍可用 `--map` 覆盖。

## 5. 第三批：诊断

### 5.1 等链路时说明可能的原因

`fdx` 等一条链路打开超过 3 秒时，不停核地读两样东西，各打印一行：

- 其他镜像的链路：打开了、最后一个包是 `ready`、`ok …`、`error …` 中的哪个
  （3.5 的记录，或直接读 ring）。例如 `[fdx] el2 is open and its last packet was
  "ready": it may be waiting for a command`。
- hypervisor 的停止记录 `RUSTVISOR_STOP_RECORDS`（rustvisor-sdk
  docs/17-debug-abi.md；编码见 `hypervisor_hal::diagnostic`）：有已写完的记录就
  打印核、原因和地址，例如 `C0C0 stopped: Guest 0 data abort (SError) at _start`。
  解码按 debug ABI 的版本号（`RUSTVISOR_DEBUG_ABI`）；不认识的版本只打印原始字。

### 5.2 `cores` 命令

- 不停核，列出每个核：是否已启动、跑的是哪个 VM、有没有停止记录及其原因。相当于
  awareness 的 RVCORE/RVSTOP 窗口的命令行版，只读 RAM（`AD:`），不读需要停核的
  寄存器。
- `--json` 给 runner 或脚本用。

## 6. 验证

每批：单元测试按各节写的；板上按下面的清单手动跑，结果写进 CLAUDE.md 的版本记录，
没跑的项写明没跑。

第一批的板上清单（lab 固件，rustvisor-sdk `cases/latency.toml` 的
`inject-five` 镜像）：

1. 一次粘贴 `reset`、`go`、`fdx --send "lab five inject trace" --expect "ok lab"`
   三行：依次执行，显示三次提示符。
2. 第 1 步里的 `reset` 打印关掉了 6 条链路。
3. `fdx`（EL2）：输入 `--guest 0` 不发送；`help` 1 秒后提示目标没有读；`exit`
   离开。再进入时打印之前收到的 `ok lab`。
4. 再 `go` 一次：`already running`，exit 0。
5. `source` 下面的文件跑通，`swtrace` 有 EL2 和 Guest 的记录：

   ```text
   # lab-timer.rvdbg: five Guests run timer, EL2 traced
   reset
   go
   fdx --send "lab five inject trace" --expect ready --expect "ok lab"
   fdx --guest 0 --send "run timer" --expect ready --expect "ok timer"
   fdx --guest 1 --send "run timer" --expect ready --expect "ok timer"
   fdx --guest 2 --send "run timer" --expect ready --expect "ok timer"
   fdx --guest 3 --send "run timer" --expect ready --expect "ok timer"
   fdx --guest 4 --send "run timer" --expect ready --expect "ok timer"
   ```

6. runner 回归：rustvisor-sdk `cargo board run smoke ipc` 全过（管道会话没变）。

## 7. 待定

- `fdx` 里 `exit`/`quit` 离开（本计划）还是照样发给目标、只认 `:q`。
- `reset` 默认关链路（本计划）还是要 `--close-links` 才关。
- 4.1 的链路名用 `el2`/`guest0`（和 `swtrace.map` 一致，本计划）还是更短的
  `g0`。
