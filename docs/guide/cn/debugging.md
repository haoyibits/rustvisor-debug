# 调试与交互会话

[English](../en/debugging.md)

本页从已经部署的程序进入调试。先确认[本地镜像与目标内容一致](flashing.md)，并确保 ELF 含调试信息；单纯拥有 `.elf` 后缀不代表其中有源码行号。

## 先选择是否改变目标状态

| 命令 | 行为 |
| --- | --- |
| `rvdbg status` | 查看状态，不自动停核 |
| `rvdbg attach` | 不复位地连接目标，保留原有运行/停止状态 |
| `rvdbg halt` | 停下所选核 |
| `rvdbg go` | 继续所选核 |
| `rvdbg reset` | 复位芯片并停在复位向量 |
| `rvdbg run-until <符号或地址>` | 从停核状态运行到临时硬件断点 |
| `rvdbg down` | TRACE32：执行 `SYStem.Down` |

使用 TRACE32 查看或控制目标前，PowerView 必须已经运行。`rvdbg open` 启动或复用 PowerView；需要加载符号时使用 `rvdbg load`，注意它会在目标停着时继续运行。

运行到检查点的示例：

```sh
rvdbg halt
rvdbg run-until rustvisor_boot_main --timeout 30
rvdbg status
```

符号需要在实际镜像中存在，且控制流之后确实能到达。目标原本已经运行时，`run-until` 不会先替你停核。停在其他位置时返回 3；超时不等于目标已经停止。StellarLINK 超时时目标继续运行，工具尽可能移除临时断点；下一步先检查状态。OpenOCD 路径使用 A32 硬件断点，不应直接套用于不同指令集的目标。

## VS Code

```sh
rvdbg vscode
```

命令向项目的 `.vscode/launch.json`、`tasks.json` 合并配置，保留已有条目，修改前备份为 `*.bak.<时间戳>`。在 Run and Debug 中选择相应后端：

| 配置 | 连接方式 | 启动前准备与行为 |
| --- | --- | --- |
| `TRACE32: Attach` | 隐藏任务启动 rvdbg DAP 代理 | PowerView 已运行，符号已加载 |
| `StellarLINK: Attach` | Cortex-Debug 接入 `rvdbg debug` 的 GDB server | 先烧录；SR6P6 板级脚本在 GDB 连接时会复位核 |

因此不能仅凭配置名中的 “Attach” 推断它保留现场。需要保存故障现场时，先用[查看目标](inspection.md)记录状态，再决定是否开启 IDE 会话。

TRACE32 的 Restart 由代理通过 Remote API 复位并继续运行。代理对 Locals 请求返回空列表，以避开部分 `t32debugadapter` 在 FreeRTOS 中断帧上的崩溃；寄存器、调用栈、监视表达式、断点和单步使用对应调试功能。

生成文件包含 rvdbg 和配置文件的绝对路径。项目搬家或工具安装位置改变后，重新执行 `rvdbg vscode`。

## RustRover / JetBrains（TRACE32）

安装 LSP4IJ 插件后执行：

```sh
rvdbg rustrover
```

命令生成 `.run/TRACE32 Attach.run.xml`。选择 `TRACE32: Attach` 并启动 Debug，LSP4IJ 启动 DAP 代理并连接。此流程不提供 StellarLINK 配置；StellarLINK 使用 VS Code/Cortex-Debug 或 GDB。

## 直接使用 GDB（StellarLINK）

第一个终端保持服务运行：

```sh
rvdbg --backend openocd debug
```

第二个终端加载实际主镜像并连接，下面的端口采用默认值：

```sh
arm-none-eabi-gdb build/el2.elf -ex 'target extended-remote :3333'
```

默认 GDB 断开后服务结束；`rvdbg debug --keep` 保持运行直到 Ctrl-C。如果复用已有交互会话的 OpenOCD，服务生命周期归原会话管理。额外镜像符号在手工 GDB 会话中需要自行加载；生成的 VS Code 配置会为 `extra_elf` 添加符号加载命令。

`gdb_target` 决定 GDB 调试哪个核；查看命令的 `core` 可以另设，见[配置](configuration.md)。外部命令停核、继续运行会影响同一个核上的 GDB 调试，不要把它们当作彼此隔离的会话。

## 多条命令：进入交互会话

不带子命令运行 `rvdbg`。下面是示意交互，提示符中的后端和状态以实际连接为准：

```text
$ rvdbg
rvdbg [...]> status
rvdbg [...]> halt
rvdbg [...]> fault
rvdbg [...]> mem my_buffer 4
rvdbg [...]> go
rvdbg [...]> quit
```

会话提供 `help [命令]`、Tab 补全、`backend trace32|openocd`、`reconnect` 和 `quit`/`exit`。换探针后用 `reconnect` 重新检测。历史写入 `.rvdbg/history`。Ctrl-C 结束等待中的 RTT、FDX 或 run-until，保留会话；命令失败也不会直接退出。

StellarLINK 会话保持一个 OpenOCD 供命令与 GDB 共用。TRACE32 的查看命令、`ram` 和 `fdx` 共用首次使用时建立的连接；失败后丢弃，下一条命令重新连接，失败命令不会自动重试。`flash`、`load`、`open` 等另行管理连接。

## TRACE32 自定义操作

```sh
rvdbg eval 'Register(PC)'
rvdbg cmd 'Break.Set my_function /Program /Onchip'
rvdbg do scripts/trace_setup.cmm
```

`eval` 求值，`cmd` 执行 PRACTICE 命令，`do` 运行脚本并等待结束。脚本出错停止时工具会收尾并报告；脚本停在对话框或 `STOP` 时留给操作者处理。命令或脚本能做什么取决于内容，包括改变寄存器、复位和烧录。

Trace 的配置和导出见[查看目标](inspection.md)；IDE 无源码、端口或脚本繁忙问题见[排障](troubleshooting.md)。
