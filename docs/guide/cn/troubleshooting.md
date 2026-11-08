# 按现象排障

[English](../en/troubleshooting.md)

先保留报错和现场，再判断故障在配置、服务、探针还是目标。`rvdbg config` 显示工具准备操作什么；`rvdbg status` 查看当前状态。不要先复位再试图恢复已经消失的异常现场。

## 没发现探针，或者选错后端

**现象：** `no probe found`，或命令实际走了另一种调试器。

运行 `rvdbg config`，检查检测到的 USB 探针、PowerView 和配置中的固定后端。两种探针同时存在时默认 TRACE32；本次要用 StellarLINK，使用 `rvdbg --backend openocd ...`。交互会话插拔探针后执行 `reconnect`。

明确指定后端只跳过自动选择，不能证明物理连接可用。成功标准是实际后端命令能读取目标状态。规则见[配置](configuration.md)。

## TRACE32 端口有响应，但 rvdbg 连不上

**现象：** `no PowerView on RCL port ...`、`not a usable TRACE32 RCL endpoint`，或启动后一直未就绪。

- 尚未启动 PowerView：执行 `rvdbg open`，随后再运行查看命令。
- 已有 `config.t32` 定义 `RCL=`：确认是 `NETTCP`，`PORT=` 与 `trace32.rcl_port` 一致。
- 端口已占用但协议不对：确认监听者确实是预期 PowerView，避免把其他服务当作 Remote API。
- 启动失败：查看 `<项目>/.tracebridge/powerview.log`，核对安装目录和启动配置。

不要为了排障移除本机访问限制。连接设置见[快速开始](quickstart.md)，启动路径见[配置](configuration.md)。

## OpenOCD 起不来，或复用了别的会话

检查 `rvdbg config` 中的 executable、板级脚本、Tcl/GDB 端口，随后查看 `.rvdbg/openocd.log`。确认安装的是 `openocd-stellar`，脚本存在，目标已供电，探针连接正确。

rvdbg 会优先复用 Tcl 端口上已有的 OpenOCD。多个项目不能无意共享同一组端口；也不要仅因端口冲突就结束不明来源的调试服务。为项目设置独立端口，重新建立所需会话，见[配置](configuration.md)。

## 命令或选项只支持 TRACE32

**现象：** `... is available with TRACE32 only`，或选项被提示只适用于一个后端。

先确认这次是不是有意使用 OpenOCD。`reg` 的 PER 名称、PRACTICE、trace、时钟 HTML 都没有直接的 StellarLINK 对应。寄存器可改用项目确认过的地址或检查文件，不能把 PER 名称当作物理地址。功能对照见[手册入口](README.md)，读取方式见[查看目标](inspection.md)。

## 运行中读不到核心或 Hyp 寄存器

**现象：** `... can only be read while the core is halted`，或停在 Guest 时仍无法读 Hyp 寄存器。

读取内存和读取核心状态有不同条件。需要停核时显式执行 `rvdbg halt`，或对检查选择 `--halt`。StellarLINK 还需要核当前处于 Hyp/EL2；应在 hypervisor 的合适断点停下。不要把当前在 EL1 造成的不可读误判为异常寄存器全零。诊断后 `check --halt` 不会自动继续，见[查看目标](inspection.md)。

## PowerView is busy

`flash`、`load`、`reset`、`do` 会拒绝覆盖正在运行的 PRACTICE 脚本。脚本可能来自其他工具，也可能停在对话框、`STOP` 或错误位置。先到 PowerView 确认脚本用途，等它结束；确实需要放弃时由操作者结束脚本，再执行原命令。

调大超时不会解除脚本栈冲突。共享服务的规则见[调试](debugging.md)。

## 烧录超时、验证差异或 OTP 拒绝

| 现象 | 下一步 |
| --- | --- |
| 烧录确实还在进行 | 核对 `trace32.operation_timeout` / `openocd.timeout`；TRACE32 同时看 AREA 输出 |
| `verify` 返回 3 | 保存首个差异地址，核对本地镜像、加载地址和本次完整镜像集合 |
| 另一个镜像消失 | 核对 `extra_elf`，尤其 TRACE32 的全范围擦除行为 |
| OTP 内容不同 | 核对启动记录和保护范围；确定是否原本就不应更改该区域 |

不要将 `--no-verify` 或 `--write-otp` 作为通用重试选项。操作顺序和保护规则见[烧录](flashing.md)。

## IDE 只有汇编、没有源码，或断点不对

确认 IDE 加载的 ELF 与板子一致，再检查 ELF 是否有 DWARF 行号。Rust release profile 可启用 `debug = true` 并重新构建；这会改变构建产物，构建后按正常流程核对或重新部署，不能直接假定新 ELF 与旧目标一致。

TRACE32 需要加载正确符号；GDB 需要读取正确 ELF 和额外镜像符号。项目移动后重新生成 IDE 配置。TRACE32 Locals 为空是代理的既定行为，见[调试](debugging.md)。

## RTT 无输出，或者 FDX 等不到回答

RTT 先确认固件已经执行初始化，再核对符号/地址和 TRACE32 程序名。给出 `--cb` 只绕过符号定位，不能修复错误控制块。

FDX 先确认选择的是主镜像还是 `--guest N`，再确认固件确实实现了发送的消息。结束其他 runner/终端对同一链接的占用；检查期望包内容与超时。不要向不明链接反复发送会改变目标的命令。缓冲区和超时含义见[RTT 与 FDX](channels.md)。

## 脚本显示成功，但其中一个检查失败

如果使用 `rvdbg --json` 管道会话，检查每条命令的 end 记录。会话最终退出码 0 仅说明输入循环正常结束，不能覆盖某条命令返回的 3。若只按进程退出码判断，应改用一次性命令，或实现[会话协议](automation.md)的逐条结果判断。
