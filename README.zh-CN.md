# rustvisor-debug

`rvdbg` 经 Lauterbach TRACE32 或 ST 的 StellarLINK 烧录、查看和调试 rustvisor 板子。它读一个 `rvdbg.toml`，每条命令在当前插着的调试器上执行：TRACE32 走内置的 tracebridge 代码，StellarLINK 走打过补丁的 OpenOCD（[`openocd-stellar`](https://github.com/haoyibits/OpenOCD/tree/openocd-automotive-mcu-r3)，作为独立程序运行）。单个静态二进制文件，不依赖 Python，项目里只需要放这一个配置文件。

*English: [README.md](README.md)*

支持的主机：Apple 芯片的 macOS 和 Linux（x86_64、aarch64）。[agent skill](skills/rvdbg/SKILL.md) 告诉 AI agent 哪个命令做什么。

## 安装

用 Homebrew（macOS 或 Linux）：

```sh
brew install haoyibits/tap/rvdbg     # 会一起装上 openocd-stellar
```

或者用安装脚本：

```sh
curl -fsSL https://github.com/haoyibits/rustvisor-debug/releases/latest/download/install.sh | sh
```

安装脚本把 `rvdbg` 装到 `~/.local/bin`：用 `RVDBG_INSTALL_DIR` 改目录，用 `RVDBG_VERSION=v0.1.21` 固定版本。这个目录需要在 `PATH` 里。它不装 StellarLINK 后端需要的 openocd-stellar；只用 TRACE32 时不需要。

手动安装：从 releases 页面下载 `rvdbg-<target>.tar.gz`，用 `.sha256` 文件核对，再把二进制文件放到 `PATH` 里的某个目录。

**macOS 用浏览器下载时**：下载的文件会被加上隔离属性，Gatekeeper 会拒绝运行（"无法验证开发者"）。执行一次：

```sh
xattr -d com.apple.quarantine /path/to/rvdbg
```

用 `install.sh`（curl）安装不会带这个属性；如果有，脚本会顺手去掉。

从源码安装：`cargo install --path crates/rustvisor-debug`。

## 前提

**TRACE32**

- TRACE32 PowerView，带 `t32debugadapter`（装在默认的 `~/t32` 下会被自动找到）。
- TCP 上的 Remote API。可以在 `config.t32` 里打开：

  ```text
  RCL=NETTCP
  PORT=20000
  REMOTEHOSTDENY
  ```

  也可以不写：`config.t32` 里没有 `RCL=` 时，rvdbg 启动 PowerView 会加上 `--t32-api-rcl-remotehost=DENY --t32-api-rcl=TCP:<rcl_port>`。`REMOTEHOSTDENY` 让端口只对本机开放：Remote API 没有认证，能连上它的人可以执行任何命令，包括在你电脑上执行 `OS.*`。

**StellarLINK**

- `openocd-stellar`（Homebrew formula 会一起装上）。
- 用 `rvdbg debug` 时：`arm-none-eabi-gdb`，或者装了 [Cortex-Debug](https://marketplace.visualstudio.com/items?itemName=marus25.cortex-debug) 扩展的 VS Code。

## 快速上手

```sh
cd ~/work/rustvisor-sdk      # 项目根目录
rvdbg init                   # 生成 rvdbg.toml（目录里有 trace32.toml 时沿用其中的设置）
$EDITOR rvdbg.toml           # program、elf、extra_elf……
rvdbg config                 # 镜像、找到的调试器、会用的后端
rvdbg flash                  # 烧录镜像并运行
rvdbg vscode                 # 或：rvdbg rustrover（TRACE32）
```

命令在项目里的任何目录都能运行：rvdbg 像 git 一样，从当前目录往上找最近的 `rvdbg.toml`。`--config <路径>` 可以直接指定文件。

日常流程：

1. 用你自己的构建系统编译镜像。
2. `rvdbg flash`（用 TRACE32 时，只有符号变了可以用 `rvdbg load`）。
3. 在 IDE（见下文）、GDB 或 PowerView 里调试。
4. `rvdbg rtt` 看目标的 RTT 输出。
5. `rvdbg status`、`fault`、`verify`、`check` 查看板子的状态，确认它跑的是你刚编译的镜像。只有 `flash` 和 `reset` 会复位目标。

## 命令

| 命令 | 作用 | TRACE32 | StellarLINK（OpenOCD） |
|---|---|---|---|
| `init` | 生成带注释的 `rvdbg.toml` | | |
| `config` | 配置、找到的调试器、选中的后端 | | |
| `flash` | 烧录 ELF 和其他镜像，然后运行 | 烧录脚本（见下文） | 擦除、写入、逐段回读比对；一次性写入区内容不同时拒绝写入（`--write-otp` 才允许） |
| `load` | 不烧录、不复位，只加载符号 | 支持 | —（GDB 自己读 ELF） |
| `debug` | 前台运行给 IDE 用的调试服务 | DAP 代理 | `gdb_target` 的 GDB server |
| `vscode` | 把两个后端的调试配置写进 `.vscode/` | `TRACE32: Attach` | `StellarLINK: Attach`（Cortex-Debug） |
| `rustrover` | 写 `.run/TRACE32 Attach.run.xml`（LSP4IJ） | 支持 | — |
| `status`、`halt`、`go` | 运行状态（停着时还有 PC 和 CPSR）、停核、继续运行 | 支持 | 支持 |
| `fault` | AArch32 Hyp 异常报告 | 还有 `--trace [N]` | 同样的报告；核必须停在 Hyp 模式 |
| `mem <地址\|符号> [n]` | 读 32 位字 | 支持 | 经 MEM-AP，符号来自 ELF |
| `verify [ELF]` | 目标内存和镜像比对 | 还有 `--t32` | 经 MEM-AP |
| `check 文件` | 按检查文件比对期望值 | 所有读取方式 | `addr`、`core`、`clock` 读取 |
| `clock` | 带频率的时钟树 | 还有 `--html` | 支持 |
| `rtt` | 通道 0 上的 SEGGER RTT 终端 | 支持 | 经 MEM-AP，核照常运行 |
| `reset` | 复位并停在复位向量 | 支持 | `reset halt` |
| `attach` | 连上目标但不复位 | `SYStem.Mode Attach` | 支持 |
| `run-until <符号\|地址>` | 让停着的核运行到某个符号或地址 | 片上断点 | 硬件断点 |
| `open`、`chips`、`reg`、`eval`、`cmd`、`watch`、`trace`、`do`、`down` | PowerView、烧录脚本、PER 文件、PRACTICE、trace | 支持 | — |
| `skill` | 安装 agent skill（`~/.claude/skills/rvdbg`） | | |

在 StellarLINK 上运行只有 TRACE32 才有的命令时，会报错 "… is available with TRACE32 only"。

后端依次取 `--backend trace32|openocd`、`rvdbg.toml` 的 `project.backend`、当前插着的调试器：StellarLINK（USB 0483:0499 或 263d:4001）→ OpenOCD，Lauterbach 调试器或正在运行的 PowerView → TRACE32，两个都有时用 TRACE32。用 StellarLINK 时，查看类命令优先连 `rvdbg debug` 或交互会话正在运行的 OpenOCD（Tcl 端口），否则自己启动一个，不复位目标。

`--json`（写在命令参数后面）让每条查看类命令输出一个 JSON 文档。退出码：0 正常，1 出错，2 用法错误，3 检查没通过、`verify` 发现差异，或 `run-until` 停在了别处。

在终端里输出带颜色；有没有颜色，文字和列都一样。管道输出和 `--json` 不带颜色。`NO_COLOR=1` 关掉颜色，`CLICOLOR_FORCE=1` 在管道里也开颜色（`rvdbg fault | less -R`）。

### 交互会话：只输入 `rvdbg`

不带命令运行 `rvdbg` 时，它找到 `rvdbg.toml`、检测调试器，然后分组列出命令，当前调试器用不了的变暗（只有 TRACE32 能用的命令；没插调试器时是所有要用调试器的命令）。之后直接输入命令，不用加 `rvdbg`（加了也可以）：

```text
$ rvdbg
rvdbg 0.1.20  /work/rustvisor-sdk/rvdbg.toml
backend  openocd (StellarLINK on USB 0483:0499)
  inspect   status  mem  fault  verify  check  clock  reg  trace  eval
  control   halt  go  reset  attach  run-until  down  do  cmd
  ...
rvdbg [openocd running]> halt
rvdbg [openocd halted]> mem _SEGGER_RTT 4
rvdbg [openocd halted]> help run-until
rvdbg [openocd halted]> quit
```

- 提示符显示后端，用 OpenOCD 时还显示核在运行还是停着（`rvdbg [trace32]>`、`rvdbg [trace32 no-PowerView]>`、`rvdbg [no probe]>`）。
- 用 StellarLINK 时，会话期间一直开着一个 OpenOCD，GDB server 在 `gdb_port` 上。GDB 或 VS Code 的 `StellarLINK: Attach` 可以同时连上，会话里的命令也用这个 OpenOCD。会话里的 `debug` 只提示 GDB server 在哪里。
- 用 TRACE32 时，每条命令和命令行上一样连 PowerView。
- 会话自己的命令：`help [命令]`、`backend trace32|openocd`、`reconnect`（重新检测调试器，比如刚插上之后）、`quit` / `exit` / Ctrl-D。Ctrl-C 只停止正在等待的命令（`rtt`、`run-until`），不退出会话。命令出错也不会结束会话。
- Tab 补全命令名，历史记录在 `.rvdbg/history`。
- 从管道读入时不显示提示符，每行执行一条命令。

## 配置

`rvdbg.toml` 所在的目录就是项目根目录，文件里的相对路径都相对于这个目录。`rvdbg init` 生成带注释的文件，默认值是 SR6P6 的；同一目录里有 `trace32.toml` 时沿用其中的值。

| 段 | 键 |
|---|---|
| `[project]` | `backend`（可选）、`program`（TRACE32 的程序名）、`elf`、`extra_elf`（和 `elf` 一起烧录、加载、比对的其他镜像） |
| `[clock]` | 只有板子知道的频率，给 `clock` 和检查文件用：`XOSC = "40MHz"` |
| `[rtt]` | `symbol`、`control_block_address`、`poll_interval` |
| `[trace32]` | `sys`、`host`、`executable`、`binary`、`config`、`debug_adapter`、`rcl_port`、`dap_port`、`dap_backend_port`、`dap_backend_timeout`、`operation_timeout` |
| `[trace32.target]` | `cpu`、`cores`、`mem_access`、`jtag_clock`、`dual_port` |
| `[trace32.flash]` | `chip` 或 `script`（见下文）、`args` |
| `[trace32.rtos]` | `config`、`menu`、`show_tasks`（为空表示不启用 RTOS 感知） |
| `[openocd]` | `executable`、`config`（板级脚本）、`search`、`commands`、`tcl_port`、`gdb_port`、`gdb_target`（GDB 调试的核）、`core`（查看类命令用的核，默认同 `gdb_target`）、`gdb`、`memory_ap`（读物理内存用）、`timeout`、`otp`（一次性写入区的地址范围） |

环境变量可以临时覆盖 TRACE32 部分的设置，只对这一次运行有效。优先级：环境变量 > `rvdbg.toml` > 默认值。

| 变量 | 覆盖 | 空值 |
|---|---|---|
| `T32_SYS`，其次 `T32SYS` | `trace32.sys` | 忽略 |
| `T32_HOST`、`T32_EXE`、`T32_BIN`、`T32_CONFIG`、`T32_DEBUG_ADAPTER` | `trace32.*` 的各个路径 | 忽略 |
| `PROJECT_ROOT`、`ELF` | 项目目录、`project.elf` | 忽略 |
| `RTT_SYMBOL` | `rtt.symbol` | 忽略 |
| `T32_FLASH_CHIP` | `trace32.flash.chip` | 忽略 |
| `PROGRAM_NAME`、`T32_CPU`、`T32_CORES`、`T32_MEMACCESS`、`T32_JTAG_CLOCK`、`T32_DUALPORT`、`T32_FLASH_SCRIPT` | 对应的键 | **生效**（清空该值） |
| `T32_FLASH_ARGS` | `trace32.flash.args`，按 shell 命令行的规则拆分 | **生效**（没有参数） |
| `EXTRA_ELF` | `project.extra_elf`，按 shell 命令行的规则拆分 | **生效**（没有其他镜像） |
| `T32_RCL_PORT`、`T32_DAP_PORT`、`T32_DAP_BACKEND_PORT`、`T32_DAP_BACKEND_TIMEOUT`、`T32_TIMEOUT` | 端口和超时 | 忽略 |
| `T32_DAP_DEBUG=1` | 打开 `t32debugadapter` 的调试日志 | |

运行时文件写在 `<项目>/.rvdbg/`（会话历史、`openocd.log`），用 TRACE32 时还有 `<项目>/.tracebridge/`（PowerView 日志、工具栏脚本、`clock.html`、`trace.ad`）。两个目录里都有自己的 `.gitignore`。

不该放进公开仓库的板级资料放在 `~/.config/rvdbg/`（或 `$XDG_CONFIG_HOME/rvdbg`）：烧录脚本在 `flash/`，时钟描述在 `clock/`。

## 烧录

### StellarLINK

`rvdbg flash` 先执行 `reset init`，再对每个镜像执行 `flash write_image erase`，然后经 `memory_ap` 回读每个可加载段并和 ELF 比对（`--no-verify` 跳过比对），最后 `reset run`（`--no-run` 让核停着）。

**一次性写入区保护。** 写入任何东西之前，先把落在 `[openocd] otp` 范围里的段和板子上的内容比对。有差异时停止烧录，除非加了 `--write-otp`；把某一位从 0 改成 1 的写入总是被拒绝，因为 OTP 擦不掉。`rvdbg init` 默认写上 SR6P6 存放启动记录的 UTEST 区：

```toml
otp = [{ start = 0x29F87000, size = 0x1000 }]
```

### TRACE32：选择烧录脚本

`rvdbg flash` 需要一个支持 Lauterbach `PREPAREONLY` 约定的烧录脚本：脚本负责初始化目标、声明 flash，然后直接返回，不自己烧录。如果系统已经 up，rvdbg 会先执行 `SYStem.Down`：烧录脚本只在系统 down 的状态下复位并初始化芯片，否则会在应用程序留下的状态里跑烧录算法（比如 MPU 已经把算法所在的 RAM 设成了不可执行）。脚本返回后，rvdbg 执行：

```text
FLASH.ReProgram ALL /Erase
Data.LOAD.Elf <elf>
Data.LOAD.Elf <extra_elf> /NoClear     （每个其他镜像一次，没有就跳过）
FLASH.ReProgram OFF
SYStem.Down
SYStem.Up
```

可以只写芯片名，不写文件路径，由 rvdbg 来选脚本：

```toml
[trace32.flash]
chip = ""        # 为空时用 trace32.target.cpu
script = ""      # 这里写了路径就优先用它，不看 chip
```

查找顺序：

1. **你的脚本库** `~/.config/rvdbg/flash/*.cmm`。放多个项目共用的脚本，比如厂商单独给的、TRACE32 发布里没有的脚本，或者改过的官方脚本。
2. **TRACE32 安装目录** `<trace32.sys>/demo/*/flash/*.cmm`，大约一千个芯片的脚本。

脚本头部要有匹配的 `; @Chip:` 行（Lauterbach 的格式，可以用通配符，比如 `; @Chip: SR6P6*`），并且支持 `PREPAREONLY`，才会被选中。精确匹配优先于通配符，较长的通配符优先于较短的；片内 flash 的脚本优先于各种存储器变体（`-qspi`、`-spi`、`-emmc`、`-optionbyte` 等）。仍然分不出先后时会报错，这时请设置 `trace32.flash.script`。

TRACE32 的脚本覆盖整个芯片系列，具体型号通过 `CPU=<名字>` 传入。选中的脚本接受 `CPU=`、而 `trace32.flash.args` 没有设置它时，rvdbg 会自动传 `CPU=<chip>`。其他参数（如 `DUALPORT=`）保持脚本的默认值，所以 `args` 通常为空。`trace32.target.jtag_clock` 由 rvdbg 在脚本运行完后设置。

```sh
rvdbg chips SR6P6                    # 用哪个脚本，以及相关的脚本
rvdbg flash --chip SR6P6             # 临时换芯片
rvdbg flash --script ~~/demo/arm/flash/my_chip.cmm
```

往脚本库里加脚本：复制进去，确认头部有 `; @Chip: <名字>` 行。改官方脚本时，参数（`PREPAREONLY`、`DUALPORT=` 等）保持原样。带 "TRACE32 only" 许可的脚本不要放进公开仓库，放在脚本库里。

rvdbg 自己启动 PowerView 时，还会在 PowerView 工具栏上加 **Flash** 和 **Load ELF** 两个按钮。

### 多个镜像：`extra_elf`

rustvisor 有多个分别链接的程序，hypervisor 和它的 guest，各自占一块 flash。把其他镜像列在 `elf` 旁边：

```toml
[project]
elf = "target/thumbv8r-none-eabihf/release/boot_diagnostic"
extra_elf = ["target/thumbv8r-none-eabihf/release/scenario-guest"]
```

用 TRACE32 时，`FLASH.ReProgram ALL /Erase` 会擦掉烧录脚本声明过、但这次没有写入的所有扇区，所以一个接一个地烧，后烧的会把先烧的擦掉。因此每个命令都把所有镜像放在一起处理：

| 命令 | 其他镜像怎么处理 |
|---|---|
| `flash` | 和 `elf` 一起烧录（TRACE32：在同一个 `FLASH.ReProgram` 会话里，`Data.LOAD.Elf ... /NoClear`） |
| `load` | 在 `elf` 之后加载它们的符号（`/NoCODE /NoClear`） |
| `verify`（不带路径） | 依次比对每个镜像，全部一致才算一致 |
| `vscode` | `StellarLINK: Attach` 用 `add-symbol-file` 加上它们的符号 |
| `config` | 列出来，并显示文件是否存在 |

`EXTRA_ELF` 可以临时覆盖这个列表。它按 shell 命令行的规则拆分，设置了但为空表示没有其他镜像：

```sh
EXTRA_ELF="build/guest.elf build/other.elf" rvdbg flash
EXTRA_ELF= rvdbg flash      # 只烧 elf；用 TRACE32 时其他镜像所在的扇区会被擦掉
```

## 在 IDE 或 GDB 里调试

### VS Code

```sh
rvdbg vscode
```

把两个配置合并进 `.vscode/launch.json` 和 `.vscode/tasks.json`（已有的条目保留，写之前先备份成 `*.bak.<时间戳>`）。打开 **Run and Debug**，选一个按 F5：

- **TRACE32: Attach**。一个隐藏任务会启动 `rvdbg adapter`，它是 `t32debugadapter` 前面的 DAP 代理。PowerView 必须已经在运行（`rvdbg flash`、`load` 或 `open`）。代理会处理 **Restart**（通过 Remote API 复位，然后继续运行），并对 Locals 请求返回空列表，因为有些版本的 `t32debugadapter` 遇到 FreeRTOS 中断帧会崩溃；监视表达式、寄存器、调用栈、断点和单步都正常。
- **StellarLINK: Attach**（需要 Cortex-Debug 扩展）。一个隐藏任务会运行 `rvdbg debug`。SR6P6 的板级脚本在 GDB 连上时复位核，所以调试从复位向量开始；请先在终端里 `rvdbg flash`。

### RustRover（以及其他 JetBrains IDE），TRACE32

```sh
rvdbg rustrover
```

1. 安装 **LSP4IJ** 插件（Settings → Plugins → Marketplace），它给 JetBrains IDE 加上 Debug Adapter Protocol 支持。
2. 这条命令写出 `.run/TRACE32 Attach.run.xml`，IDE 里会出现名为 **TRACE32: Attach** 的运行配置。
3. 选中它，点 **Debug**。LSP4IJ 会启动 adapter，等它就绪后连接。

可以在 `*.c *.h *.cpp *.hpp *.cc *.s *.S *.rs` 文件里设断点（在配置的 **Mappings** 页里）。

生成的文件里用的是绝对路径（rvdbg 和 `rvdbg.toml`）。项目搬了位置或者 rvdbg 装到了别处，要重新运行 `vscode` / `rustrover`。

### GDB（StellarLINK）

`rvdbg debug` 在前台运行 OpenOCD，在 `gdb_port` 上为 `gdb_target` 开 GDB server，并设置 `gdb_breakpoint_override hard`。GDB 断开时它就结束（`--keep` 让它一直运行到 Ctrl-C）。如果交互会话已经占着这些端口，它只打印端口位置并复用。

```sh
rvdbg debug &
arm-none-eabi-gdb target/thumbv8r-none-eabihf/release/boot_diagnostic -ex 'target extended-remote :3333'
```

用 TRACE32 时，`rvdbg debug` 改为在前台运行 DAP 代理。

## RTT

```sh
rvdbg rtt                 # 从符号 _SEGGER_RTT 找控制块
rvdbg rtt --cb 0x60000000 # 直接给控制块地址
rvdbg rtt --output-only   # 不转发键盘输入
rvdbg rtt --replay        # 先显示缓冲区里已有的内容
```

核运行时双向使用通道 0：TRACE32 通过运行时内存访问（`dual_port = "ON"`），StellarLINK 通过 MEM-AP。终端会切换成逐字符输入，退出时恢复原来的设置。

## 查看目标

查看类命令连接已经在运行的程序：通过 Remote API 连 PowerView，或通过 Tcl 端口连 OpenOCD。**只有 `reset`（以及 `flash`）会复位目标**：连接本身和其他命令都不复位；用 TRACE32 时也不会启动 PowerView（请先 `rvdbg open`、`flash` 或 `load`）。

一次典型的会话：

```text
rvdbg flash                    # 烧录板子
rvdbg
rvdbg [...]> verify            # 板子上跑的是刚编译的镜像吗？
rvdbg [...]> status            # 运行状态
rvdbg [...]> halt              # 协处理器寄存器和核心寄存器要先停核
rvdbg [...]> status            # 这时还会显示带符号+偏移的 PC，以及解码后的 CPSR
rvdbg [...]> fault             # 崩溃之后：向量、解码后的 HSR、出错地址
rvdbg [...]> mem my_buffer 4   # 某个符号或地址处的内存
rvdbg [...]> check checks/boot.toml
rvdbg [...]> go
```

| 命令 | 类型 | 作用 |
|---|---|---|
| `status` | R | 运行状态；停着时还有带符号+偏移的 PC 和解码后的 CPSR（TRACE32 还显示调试器模式、电源、CPU） |
| `mem <地址\|符号> [个数]` | R | 以十六进制显示 `个数` 个 32 位字（默认 1）；`C15:0x4025` 读协处理器寄存器 |
| `fault [--trace [N]]` | R | AArch32 Hyp 异常报告：向量槽（PC 落在 HVBAR 以外的向量表里也能认出来）、解码后的 HSR、HDFAR 或 HIFAR、ELR_hyp 及其符号、SPSR_hyp。`--trace`（TRACE32）再附上最新的 N 条 trace 记录（默认 32） |
| `verify [elf] [--t32]` | R | 目标内存里是不是 ELF 的可加载内容？ |
| `check <文件> [--variant V] [--dry-run]` | R | 由数据驱动的验收检查（见下文） |
| `check <文件> --halt` | **S** | 同上，但检查要读协处理器或核心寄存器时先停核 |
| `clock [时钟=频率…] [--tree 文件] [--html]` | R | 根据时钟寄存器算出的时钟树和频率（见下文） |
| `reg <名字\|地址>…` | R | TRACE32：按 PER 文件里的名字（`HSR`）、字段（`HSCTLR.C`，同时打印 BITFLD 的选项文字）、完整 PER 路径或原始地址读寄存器 |
| `trace [--last N]` | R | TRACE32：PowerView 录下的最新 N 条 trace 记录（默认 100），包括记录号、地址、距上一条的周期数、符号 |
| `eval <表达式>` | R | TRACE32：任意 PRACTICE 表达式的值 |
| `watch <文件\|名字…>` | UI | TRACE32：打开只含这些寄存器的 PER.Watch 窗口（需要 PowerView build 176763（2025/09）或更新） |
| `halt`、`go` | S | 停核或继续运行 |
| `attach` | S | 不复位地连上；核原来在跑就继续跑，停着就继续停着 |
| `reset` | S | 复位芯片并停在复位向量。TRACE32：`SYStem.Down`、`SYStem.RESet`、`SYStem.CPU`、`SYStem.MemAccess`、`CORE.ASSIGN`、`SYStem.Mode Up`、`SYStem.JtagClock`，有 PRACTICE 脚本在运行时拒绝执行。OpenOCD：`reset halt` |
| `run-until [--timeout S] <符号\|地址>` | S | 让停着的核运行到某个符号、`符号+偏移`或地址，用的是临时硬件断点（TRACE32：`Go.direct … /Onchip`，也可以是源码行 `\module\120`；OpenOCD：`bp … hw`，A32）。停在别处时退出码为 3 |
| `down` | S | TRACE32：`SYStem.Down` |
| `do [--timeout S] <脚本> [参数…]` | S | TRACE32：用 `DO` 运行 PRACTICE 脚本，等它结束。脚本因出错停下时会被结束并报告错误；停在对话框或 `STOP` 的脚本留给你处理 |
| `cmd <PRACTICE 命令>` | S | TRACE32：执行**任意**命令，包括复位和烧录 |

R 类命令只读，S 类命令会改变目标的状态。

### 值是怎么读出来的

**TRACE32。** 所有值都由 PowerView 以 PRACTICE 函数求值（`Data.Long(...)`、`Register(...)`、`PER.VALUE(...)`、`sYmbol.BEGIN(...)`），所以地址和访问类的含义和在 PowerView 命令行里完全一样。只有 `verify` 用原始内存接口，而且只读普通内存（`AD:`）。

- 寄存器名用 CPU 的 PER 文件，通过 `PER.ADDRESS()`/`PER.VALUE()` 查找：`HSR` 按 `.HSR` 找，`A.B` 先按 `.A.B` 再按完整路径找。名字区分大小写，路径里带空格的部分要加引号：`'"TMR (Timer Unit)".TMR_0.CTRL'`。在很多模块里都出现的名字（比如 `CR`）要写完整路径；报错信息会列出候选路径。
- 同一个名字定义了多次但地址相同时，照常读取并附上说明；地址不同时，`reg` 会列出各个完整路径。`PER.ADDRESS()` 读不了的只读 `rgroup` 条目改用 `PER.VALUE()` 读。
- 对于 BITFLD 字段，`reg` 还会打印值对应的选项文字，比如 `HSCTLR.C ... 0x00000001  "Enabled"`，通常取自 PER 文件（`--json` 里的 `choice_source`）。
- 第一次查 PER 之前，rvdbg 会执行 `PER.Set.CONDitions`（调试器状态变化后会再执行一次）；查找时如果报 "No default peripheral file"，每次连接最多执行一次 `PER.ReProgram`。这两步只改调试器的状态，不动目标。
- 对 `C15:` 和 `C14:` 寄存器，PER.ADDRESS() 返回的是命令行地址的 4 倍；`reg` 打印命令行形式，并通过回读来核对（`--json` 里的 `address_check`：`confirmed`、值为 0 或 0xFFFFFFFF 时为 `unconfirmed`、`failed`）。
- **guest 停着时读 Hyp 寄存器。** TRACE32 在核当前的异常级别执行 MRC，所以在 EL1 读 Hyp 寄存器（op1 = 4，比如 HCR `C15:0x4011`）会报总线错误。rvdbg 这时改用 `HC15:` 再读一次，这在 Cortex-R52 上可用。

PER 文件还会被当成纯文本扫描，用于上面这些备用方案、选项文字和报错信息；值本身总是来自 TRACE32。

**StellarLINK。** 内存经 `memory_ap` 读，核运行时也能读。`C15:`/`C14:` 地址在停着的核上用 `aarch64 mrc` 读，用的是核当前的模式：Hyp 寄存器要求核停在 EL2，比如停在 hypervisor 里的断点上（没有 `HC15:` 这样的替代）。核心寄存器来自 `get_reg -force`，符号来自 ELF 的 `.symtab`。

协处理器寄存器和核心寄存器只能在核停着时读。只读命令从不自己停核，读不了时会直接说明。

### Trace（TRACE32）

rvdbg 不负责配置 trace：trace 输出端、ETM 以及相关的板级设置写在你自己的 PRACTICE 脚本里，用 `rvdbg do` 运行。`trace` 和 `fault --trace` 只导出 `Trace.METHOD` 已经录下的内容，用 `Trace.EXPORT.Ascii … /ShowRecord` 导出到 `.tracebridge/trace.ad`，并重新给每个地址查一遍符号，因为导出时会截断长符号。核必须停着。周期数这一列需要周期精确的 trace（`ETM.TImeMode CycleAccurate`）。

```sh
rvdbg halt
rvdbg do scripts/trace_setup.cmm
rvdbg run-until batch_done --timeout 30
rvdbg trace --last 3000 --json > trace.json
```

### `verify` 和 `check`

**`verify`** 把每个 `PT_LOAD` 段和它加载地址（LMA，`p_paddr`）处的内容比对，而不是运行地址，所以启动代码会复制到 RAM 的初始化数据是在 NVM 里比对的。结果是 `match`，或者第一个不同的地址和不同的字节数。`--t32` 还会运行 TRACE32 自己的比对（`Data.LOAD.Elf <elf> /DIFF /PHYSLOAD /NoRegister /NosYmbol /NoClear`，不改内存、PC 和已加载的符号），并报告两者结论是否一致。

**`check`** 运行放在你项目里的 TOML 文件；rvdbg 本身不包含任何板子的数据。所有写法见 [`docs/check-example.toml`](docs/check-example.toml)：

```toml
description = "Boot acceptance"

[[check]]
name = "system control"
read = { reg = "SCTRL" }                 # 或 addr = "AD:0x..."、core = "PC"、expr = "..."
expect = { eq = 0x00C50078 }             # eq/ne（可加 mask）、nonzero、range、in_symbol、one_of
variants = ["debug"]                     # 可选：只在 --variant debug 时运行

[[check]]
name = "core clock"
read = { clock = "CORE" }                # `clock` 时钟树里的一个时钟
expect = { hz = 400_000_000, source = "PLL", warnings = [] }  # state、hz、source、warnings 任选
```

`reg` 和 `expr` 读取需要 TRACE32；`addr`、`core`、`clock` 两个后端都能用。`read.clock` 检查判断的是 `clock` 算出的时钟树（同样的描述文件和 `[clock]` 频率）：`state` 是 `"on"` 或 `"off"`；`hz` 误差在半赫兹以内算一致（并隐含 "on"）；`source` 是它当前用的时钟源；`warnings` 是完整的警告列表（比如 `"not locked"`）。

`eq`、`ne`、`one_of` 里可以用 `"sym:<名字>[+偏移]"` 代替数字。输出每个检查一行，最后一行汇总。`--dry-run` 只解析所有名字和符号，什么都不读，所以核在运行时也能用。检查要读协处理器或核心寄存器而核在运行时，`check` 会报错退出，除非加了 `--halt`；加了 `--halt` 会先停核、提示一下，结束后核保持停着（用 `rvdbg go` 继续）。

### `clock`

`rvdbg clock` 打印当前配置下的整棵时钟树：每个时钟挂在它当前的时钟源下面，带频率，以及这个频率是怎么来的。

```text
$ rvdbg clock XOSC=8MHz
/home/me/.config/rvdbg/clock/example.toml (Example MCU (made up))
IRC             16 MHz   nominal  ; internal RC oscillator
XOSC            8 MHz    given  ; crystal: the board decides
└─ PLL          200 MHz  CLKSEL[24]=1 x50 /2
   └─ SYSCLK    200 MHz  CLKSEL[1:0]=2  ; core clock
      ├─ BUS    50 MHz   /4  ; peripheral bus
      └─ TIMER  off      (TIMDIV[31] = 0)
```

- `off` 表示时钟被关掉了，或者分频字段为 0；`?` 表示频率算不出来（没给频率，或者选择器选了一个描述文件里没列出的源）；`! …` 是描述文件里定义的警告，比如 PLL 没锁定。
- 你只需要提供寄存器里没有的信息，通常是晶振频率：可以作为参数（`XOSC=40MHz`），也可以在 `rvdbg.toml` 的 `[clock]` 里写一次。
- 最后一列说明频率是怎么来的：源时钟是 `nominal` 或 `given`，选择器是 `字段=N`，`xN` `/N` 是在上一级时钟基础上的倍频和分频。
- 它只读内存映射的寄存器，每个只读一次，所以核在运行时也能用。读不了的寄存器会让相关时钟显示为 `error`（退出码 1），其余部分照常打印。
- **`--html`**（TRACE32）另外生成一张图，写到 `<项目>/.tracebridge/clock.html`，并打印它的 `file://` 地址。它是单个文件，不从网络加载任何东西，明暗跟随浏览器的设置。图里有两种视图：**按模块**（描述文件里每个 `group` 一个面板，每个选择器列出它能选的所有源）和**按时钟源**（从源时钟出发的一棵树，正在使用的路径加粗，没用到的选择器输入也标出名字）。

时钟树来自**描述文件**：`--tree <文件>` 直接指定；否则在 `~/.config/rvdbg/clock/*.toml` 里找 `chips` 能匹配 `trace32.flash.chip` 的文件，`trace32.flash.chip` 为空时用 `trace32.target.cpu`。所有写法见 [`docs/clock-example.toml`](docs/clock-example.toml)：

```toml
chips = ["MYCHIP*"]

[reg]                                    # 32 位内存映射寄存器
PLLDV = "AD:0x40001008"

[[clock]]
name = "XOSC"                            # 不写 hz：频率由板子决定

[[clock]]
name = "PLL"
select = "CLKSEL[24]"                    # 或：from = "XOSC"
sources = { "0" = "IRC", "1" = "XOSC" }
mul = "PLLDV[6:0]"                       # 频率 = 源频率 * mul / div
div = "PLLDV[14:12]"
enable = "CTL[2]"                        # 为 0 时时钟关闭
warn = [{ when = "PLLSR[2] == 0", text = "not locked" }]
group = "PLL"                            # 可选：它在图里属于哪个面板
```

表达式支持数字、寄存器字段（`REG[high:low]`、`REG[bit]`、`REG`）、`+ - * / ^`、`== !=` 和括号。描述文件照着芯片的参考手册写；厂商文档往往是保密的，所以这种文件应该放在你的库里，不要放进公开仓库。

## AI agent

`rvdbg skill` 写出 `~/.claude/skills/rvdbg/SKILL.md`（设置了 `$CLAUDE_CONFIG_DIR` 时写到 `$CLAUDE_CONFIG_DIR/skills`；其他 agent 用 `--dir <skills 目录>`）。它告诉 agent 哪个命令做什么、哪些命令会改变目标（要先问；永远不要自己加 `--write-otp`），以及用户希望 agent 直接运行 rvdbg。升级 rvdbg 后要重新运行一次。

用 TRACE32 时，Lauterbach 的 [t32mcp](https://gitlab.com/lauterbach/t32mcp) 和它的 agent skill（`lauterbach-*`，从 Lauterbach 的 t32chat 安装）负责通用调试。两者通过同一个 Remote API 端口连同一个 PowerView：

| 工作 | 工具 |
|---|---|
| 单步、断点、源码、调用栈、局部变量、任意 PRACTICE 命令 | t32mcp 配合 `lauterbach-trace32-debug-basic` |
| 烧录、加载、复位到复位向量、RTT、运行到某个符号 | `rvdbg flash`、`load`、`reset`、`rtt`、`run-until` |
| 按 PER 名字读寄存器、Hyp 异常、目标内存和 ELF 比对、检查文件、时钟树 | `rvdbg reg`、`fault`、`verify`、`check`、`clock` |

注册 t32mcp 时用 `rvdbg.toml` 里的端口（`trace32.rcl_port`，默认 20000）。Claude Code 的写法：

```sh
claude mcp add-json -s user t32mcp '{"type":"stdio","command":"/path/to/t32mcp","args":["--execution-policy","unrestricted","--port","20000"]}'
```

两者并用时：PowerView 同一时间只能运行一个 PRACTICE 脚本。有脚本在运行时（t32mcp 正在执行，或者有脚本停在对话框或错误处），`flash`、`load`、`reset`、`do` 会报 `PowerView is busy` 并停止，不会在它下面硬做。rvdbg 从不选择、创建或关闭 AREA 窗口，所以 t32mcp 捕获输出不受影响。

用 StellarLINK 时没有 MCP server；agent 用 rvdbg，`rvdbg debug` 提供 GDB。

### 放行只读命令（Claude Code）

这些命令没有副作用，可以让 agent 不经询问直接运行。写在项目的 `.claude/settings.json` 里：

```json
{
  "permissions": {
    "allow": [
      "Bash(rvdbg config)",
      "Bash(rvdbg chips:*)",
      "Bash(rvdbg status:*)",
      "Bash(rvdbg mem:*)",
      "Bash(rvdbg reg:*)",
      "Bash(rvdbg fault:*)",
      "Bash(rvdbg verify:*)",
      "Bash(rvdbg clock:*)",
      "Bash(rvdbg trace:*)",
      "Bash(rvdbg check checks/boot.toml)",
      "Bash(rvdbg check checks/boot.toml --dry-run)",
      "Bash(rvdbg check checks/boot.toml --variant release)"
    ]
  }
}
```

`check` 用的是**完整**命令行，而不是 `:*` 前缀：前缀规则也会放行 `check … --halt`，而它会停核。其余命令（`flash`、`load`、`open`、`rtt`、`attach`、`reset`、`down`、`halt`、`go`、`run-until`、`do`、`cmd`、`check --halt`）仍然要询问。要让这些规则匹配上，命令要在项目里运行（命令前面不要加 `--config` 或 `--backend`），`--json` 放在命令参数后面。

## 从 tracebridge 迁移

1. 在 `trace32.toml` 所在的目录运行 `rvdbg init`：`rvdbg.toml` 会沿用它的值（`[target]`、`[flash]`、`[rtos]` 挪到 `[trace32]` 下面）。然后删掉 `trace32.toml`。
2. 运行 `rvdbg vscode`（以及 `rvdbg rustrover`）。`TRACE32: Attach` 配置改为启动 `rvdbg adapter`，旁边再加上 `StellarLINK: Attach`。
3. 把脚本和 agent 权限里的 `tracebridge`、`tracebridge debug` 换成 `rvdbg`（`debug break` 对应 `rvdbg halt`）。
4. 运行 `rvdbg skill`，删掉 `~/.claude/skills/tracebridge`。

把烧录脚本和时钟描述从 `~/.config/tracebridge/` 挪到 `~/.config/rvdbg/`（0.1.21 起）。

## 排障

- **`no probe found`**：插上 StellarLINK 或 Lauterbach 调试器、启动 PowerView，或者加 `--backend trace32|openocd`。`rvdbg config` 会显示找到了什么。
- **`… is available with TRACE32 only`**：这个命令需要 PowerView（`reg`、`eval`、`cmd`、`do`、`trace` 等）。两个调试器都插着时加 `--backend trace32`。
- **`… can only be read while the core is halted`**（`fault`、`status`、`check`）：协处理器寄存器和核心寄存器要求核停着。先 `rvdbg halt`，或者用 `check --halt`。用 StellarLINK 时，Hyp 寄存器还要求核停在 EL2。
- **`port 20000 is open but is not a usable TRACE32 RCL endpoint`**：别的程序占了这个端口，或者 `config.t32` 里的 `RCL=` 不是 `NETTCP`。
- **PowerView 一直没就绪**：检查 `config.t32` 里的 `RCL=`/`PORT=` 是否和 `trace32.rcl_port` 一致，并查看 `.tracebridge/powerview.log`。
- **OpenOCD 起不来**：查看 `.rvdbg/openocd.log`；`rvdbg config` 会显示它用的程序和端口。
- **`no PowerView on RCL port …`**：在 `rtt`、查看类命令或 IDE 调试之前，先用 `rvdbg open`、`flash` 或 `load` 启动 PowerView。
- **`reg <名字>` 每个名字都失败**：看看有没有关于 `PER.ReProgram` 和 `PER.Set.CONDitions` 的警告。`rvdbg eval 'PER.FILENAME()'` 显示 PowerView 用的是哪个 PER 文件。
- **`PowerView is busy: a PRACTICE script is active`**（`flash`、`load`、`reset`、`do`）：有脚本在运行、停在对话框里或停在了错误处，比如 agent 通过 t32mcp 启动的脚本。等它结束，或者在 PowerView 里结束它（`END`），再重试。
- **烧录时超时**：调大 `trace32.operation_timeout` 或 `openocd.timeout`；用 TRACE32 时看一下 PowerView 的 AREA 窗口。
- **List 窗口里只有汇编**：ELF 里没有 DWARF 行号表。编译时要带调试信息。Rust 的 release 构建默认不带，需要在 profile 里写 `[profile.release] debug = true`；它只影响 ELF 文件，不改变烧进去的代码。
- **RTT 一直在等**：固件还没初始化 RTT，或者（用 TRACE32 时）`project.program` 和加载的程序名不一致；用 `--cb` 可以绕过符号查找。

## 开发

```sh
cargo test --all           # 单元测试、模拟服务器测试、端到端测试
cargo clippy --all-targets -- -D warnings
```

这是一个虚拟 workspace：

| 路径 | 内容 |
|---|---|
| `crates/rustvisor-debug` | `rvdbg`：rustvisor 的前端、后端选择、交互会话 |
| `crates/tracebridge` | TRACE32 后端；rustvisor-sdk 的板级 runner 用它的库（`tracebridge::backend`，见 [docs/backend.md](docs/backend.md)） |
| `crates/t32rcl` | Lauterbach Python 库 `lauterbach-trace32-rcl` 中 tracebridge 用到的部分的纯 Rust 移植（只支持 TCP） |
| `crates/stellarbridge` | OpenOCD（StellarLINK）后端；OpenOCD 作为独立程序运行 |
| `crates/oocd` | OpenOCD Tcl 端口的客户端 |
| `crates/probecore` | 两个后端共用的部分：检查文件、时钟树、ELF 比对、RTT、异常报告 |
| `skills/` | 二进制文件会安装的 agent skill |

依赖只朝一个方向（`rvdbg → tracebridge, stellarbridge, probecore`；`tracebridge → t32rcl, probecore`；`stellarbridge → oocd, probecore`），由 `crates/rustvisor-debug/tests/workspace_deps.rs` 检查。`t32rcl` 用从 Python 库录下的通信数据做逐字节测试；对着真实 PowerView 录制的方法见 `crates/t32rcl/tools/`，以及示例 `capture_proxy` 和 `smoke`。

## 许可证

MIT。`crates/t32rcl` 包含从 lauterbach-trace32-rcl 移植的代码（Copyright (c) 2020 Lauterbach GmbH，MIT），见 [NOTICE](NOTICE)。
