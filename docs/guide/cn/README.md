# rvdbg 使用手册

[English](../en/README.md)

`rvdbg` 用一份 `rvdbg.toml` 管理 rustvisor 项目的烧录、调试和目标检查。连接 Lauterbach 时使用 TRACE32 PowerView；连接 StellarLINK 时使用独立运行的 `openocd-stellar`。镜像由项目自己的构建系统生成，rvdbg 负责把已经生成的镜像送到板子上，并帮助你确认目标的状态。

本手册面向在 macOS Apple 芯片或 Linux x86_64/aarch64 主机上使用 rvdbg 的人。`rvdbg init` 的板级默认值针对 SR6P6；换板前需要核对脚本、核名称、内存访问端口和一次性写入区，不能把这些默认值当作通用配置。

## 从你要完成的事情进入

| 任务 | 阅读入口 | 读完应能完成什么 |
| --- | --- | --- |
| 第一次使用 | [快速开始](quickstart.md) | 安装依赖，生成配置，烧录并查看状态 |
| 换镜像、调试器或核 | [项目配置](configuration.md) | 确认实际生效的镜像、后端和端口 |
| 更新程序或符号 | [烧录与验证](flashing.md) | 选择 flash/load，并正确处理多镜像 |
| 断点、单步、查看调用栈 | [调试与交互会话](debugging.md) | 接入 IDE/GDB，理解停核和复位行为 |
| 诊断当前程序 | [查看目标](inspection.md) | 检查 PC、异常、内存、时钟和验收条件 |
| 查看日志、收发数据 | [RTT 与 FDX](channels.md) | 找到目标缓冲区，区分日志与包通信 |
| 接入脚本或 agent | [自动化](automation.md) | 解析 JSON、退出码和会话命令边界 |
| 操作失败 | [排障](troubleshooting.md) | 从现象找到下一步检查 |

第一次阅读可按目录顺序；日常使用直接打开对应任务页。页面里的标准 Markdown 相对链接也能在 GitHub 和 Obsidian 中使用。mdBook 网站在页尾自动列出“引用此页”，可以反查哪些流程依赖当前页面。

## 两种后端有哪些差异？

| 能力 | TRACE32 | StellarLINK / OpenOCD |
| --- | --- | --- |
| 烧录、验证、状态、停核与继续 | 支持 | 支持 |
| IDE | VS Code、RustRover/LSP4IJ，通过 DAP | VS Code/Cortex-Debug、GDB |
| 普通 RAM、RTT、FDX | 运行中通过 `AD:` 访问 | 运行中通过 MEM-AP 访问 |
| 加载调试符号 | `rvdbg load` | 由 GDB 读取 ELF |
| 按 PER 名称读取寄存器 | `reg`、`watch` | 不支持；使用地址或检查文件 |
| PRACTICE | `eval`、`cmd`、`do` | 不支持 |
| Trace 导出 | `trace`、`fault --trace` | 不支持 |
| 时钟图 | `clock --html` | 仅文本/JSON 时钟树 |

两个后端共享命令名不代表所有选项都通用。后端选择规则见[配置](configuration.md)，每种操作的限制放在对应任务页。

## 操作前先确认它改变什么

`status`、`mem`、`fault`、`verify`、不带 `--halt` 的 `check` 等检查命令不会为了读取而自动复位或停核；某些寄存器读取因此会被拒绝。普通 RAM 访问和有读副作用的 MMIO 也需要区分，见[查看目标](inspection.md)。

`flash` 会写入 Flash 并复位；`reset` 复位并停核；`halt`、`go`、`run-until` 改变运行状态。`load` 不写程序，但会加载符号并在需要时继续运行；IDE 接入也可能执行板级复位钩子，详见[调试](debugging.md)。

`ram --write` 和 FDX 发送会改变目标 RAM 或让固件执行自己的协议。`cmd`、`do` 的影响取决于 PRACTICE 内容；不能仅按 rvdbg 命令名判断它们的实际作用。

查完整参数使用 `rvdbg --help` 和 `rvdbg <命令> --help`。手册解释操作过程和判断方法，参数默认值以当前安装版本的帮助和 `rvdbg init` 输出为准。

本地预览、链接规则和构建检查见[维护这本手册](writing.md)。
