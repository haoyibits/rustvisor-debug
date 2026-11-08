# 配置项目与选择后端

[English](../en/configuration.md)

配置的目标是确定三件事：操作哪些镜像、通过哪个后端、连接哪个核。改配置后先运行 `rvdbg config`，再执行会改变目标状态的操作。

## 配置文件在哪里，路径相对谁？

rvdbg 从当前目录向上找最近的 `rvdbg.toml`。它所在的目录是项目根目录；也可以显式指定：

```sh
rvdbg --config /work/firmware/rvdbg.toml config
```

镜像等项目相对路径以配置目录为基准。OpenOCD 的 `config` 条目是交给 OpenOCD 查找的脚本名，例如 `board/sr6p6_evb.cfg`；额外搜索目录通过 `search` 添加。只写程序名的 `executable`/`gdb` 由 `PATH` 查找，不能把它们误当作项目内文件。

`rvdbg init` 是获取完整带注释模板的入口。解析器会拒绝未知段和未知键；不要在文件里保留已经改名的字段。

## 后端怎样选择？

按以下优先级取第一个明确选择：

1. 命令行 `--backend trace32` 或 `--backend openocd`。
2. `[project] backend = "trace32"` 或 `"openocd"`。
3. 当前探针：Lauterbach 或正在运行的 PowerView 对应 TRACE32；StellarLINK 对应 OpenOCD；同时发现两者时选择 TRACE32。

```sh
rvdbg --backend openocd status
rvdbg --backend trace32 status
```

手动指定只决定调用哪个后端，不会让未连接的探针或未启动的服务自动变得可用。TRACE32 查看命令要求 PowerView 已运行；OpenOCD 查看命令会复用已有 Tcl 服务，否则启动一个不复位目标的实例。交互会话的连接生命周期见[调试](debugging.md)。

## 哪些字段值得先核对？

| 段 | 关键字段 | 决定什么 |
| --- | --- | --- |
| `[project]` | `elf`、`extra_elf`、`program`、`backend` | 镜像集合、TRACE32 符号程序名和后端 |
| `[trace32]` | `sys`、`config`、`host`、`rcl_port` | PowerView 安装、启动配置和 Remote API |
| `[trace32.target]` | `cpu`、`cores`、`mem_access`、`jtag_clock`、`dual_port` | 目标及运行中内存访问 |
| `[trace32.flash]` | `chip`、`script`、`args` | 烧录脚本选择及参数 |
| `[trace32.rtos]` | `config`、`menu`、`show_tasks` | 可选的 RTOS 感知 |
| `[openocd]` | `executable`、`config`、`search`、`commands` | OpenOCD 程序和板级初始化脚本 |
| `[openocd]` | `tcl_port`、`gdb_port`、`gdb_target`、`core` | 控制连接、GDB 服务核、查看类命令用的核 |
| `[openocd]` | `memory_ap`、`timeout`、`otp` | 物理内存访问、操作超时及 OTP 保护范围 |
| `[rtt]` | `symbol`、`control_block_address`、`poll_interval` | RTT 控制块及轮询间隔 |
| `[fdx]` | `symbol` | FDX 链接符号，默认 `RUSTVISOR_FDX` |
| `[clock]` | 例如 `XOSC = "40MHz"` | 板子决定、寄存器无法告知的输入频率 |

`program` 未填或为空时从主 ELF 文件名推导。它在 TRACE32 中标识符号程序，不是待烧录镜像路径；两个字段需要各自正确。

`trace32.host` 是 TRACE32 安装树中的主机平台目录名，用于定位二进制和 adapter，不是 Remote API 的网络主机地址。通常留空，由工具根据主机平台推导。

## StellarLINK 的“调试核”和“内存端口”不同

`init` 生成的 SR6P6 相关默认值包括：

```toml
[openocd]
executable = "openocd-stellar"
config = ["board/sr6p6_evb.cfg"]
search = []
commands = []
tcl_port = 6666
gdb_port = 3333
gdb_target = "sr6p6.k0c0.core"
gdb = "arm-none-eabi-gdb"
memory_ap = "sr6p6.axi"
timeout = 600
otp = [{ start = 0x29F87000, size = 0x1000 }]
```

`gdb_target` 选 GDB 调试的核；`core` 未指定时，`halt`、`go`、`fault`、`check` 等也使用它。设置另一个 `core` 不会同时把 IDE 的 GDB 连接切到那个核。`memory_ap` 是访问物理内存的 MEM-AP，不能用核名称替代。具体核名由板级 OpenOCD 脚本定义。

`tcl_port` 和 `gdb_port` 必须不同。多项目同时调试时，为各项目使用不冲突的端口，避免命令复用到另一个项目的服务。

## 环境变量怎样覆盖配置？

镜像覆盖对两种后端生效；TRACE32 设置的覆盖遵循其后端配置规则。优先级为环境变量、TOML、默认值。临时更换镜像时：

```sh
ELF=build/el2.elf EXTRA_ELF="build/guest.elf" rvdbg config
```

同样的前缀必须用于后续实际操作，或写入当前 shell 环境；只对 `config` 设置一次不会影响下一个命令。

| 变量 | 作用及空值行为 |
| --- | --- |
| `ELF`、`PROJECT_ROOT` | 覆盖主镜像或镜像解析用的项目目录；空值忽略；后者不替代 `--config` 选择配置文件 |
| `EXTRA_ELF` | 按 shell 命令行规则解析镜像列表；空值明确清空列表 |
| `PROGRAM_NAME` | TRACE32 符号程序名；空值也生效 |
| `T32_SYS`，其次 `T32SYS` | TRACE32 安装目录；空值忽略 |
| `T32_HOST`、`T32_EXE`、`T32_BIN`、`T32_CONFIG`、`T32_DEBUG_ADAPTER` | TRACE32 主机平台目录及程序路径；空值忽略 |
| `T32_CPU`、`T32_CORES`、`T32_MEMACCESS`、`T32_JTAG_CLOCK`、`T32_DUALPORT` | 目标设置；空值也生效 |
| `T32_FLASH_CHIP` | 脚本匹配的芯片；空值忽略 |
| `T32_FLASH_SCRIPT`、`T32_FLASH_ARGS` | 显式脚本及 shell 风格参数列表；空值清空 |
| `T32_RCL_PORT`、`T32_DAP_PORT`、`T32_DAP_BACKEND_PORT` | Remote API、DAP 对外及后端端口 |
| `T32_DAP_BACKEND_TIMEOUT`、`T32_TIMEOUT` | adapter 和 TRACE32 操作超时 |
| `RTT_SYMBOL` | RTT 符号；空值忽略 |

清空 `EXTRA_ELF` 会改变[烧录与验证](flashing.md)的镜像集合。尤其 TRACE32 的擦除范围由烧录脚本声明，不会因为列表缩短就自动保留其他镜像。

## 配置和日志放在哪里？

| 路径 | 内容 |
| --- | --- |
| `<项目>/.rvdbg/` | `history`、`openocd.log` |
| `<项目>/.tracebridge/` | PowerView 日志、工具栏脚本、时钟 HTML、trace 导出 |
| `~/.config/rvdbg/flash/` | 本机烧录脚本库 |
| `~/.config/rvdbg/clock/` | 本机时钟描述库 |

设置 `XDG_CONFIG_HOME` 时，本机库位于 `$XDG_CONFIG_HOME/rvdbg/`。受许可或保密条款限制的板级资料放本机库；仓库公开示例使用虚构寄存器，不能直接作为真实板子的配置。
