# RTT 日志与 FDX 通信

[English](../en/channels.md)

RTT 和 FDX 都通过目标 RAM 与运行中的固件交换数据，不要求先停核。TRACE32 使用运行中内存访问，通常需要 `dual_port = "ON"`；StellarLINK 使用配置中的 MEM-AP。二者都要求固件实现并初始化相应缓冲区，主机命令不会替固件创建协议。

| 需求 | 使用什么 |
| --- | --- |
| 连续查看字符日志，或终端输入 | RTT 通道 0 |
| 向镜像发送一个包并等待约定回答 | FDX |
| 直接检查普通 RAM | [mem / ram](inspection.md) |

## RTT：先找到控制块

```sh
rvdbg rtt
rvdbg rtt --output-only
rvdbg rtt --replay
```

默认按 `_SEGGER_RTT` 符号找到控制块。`--output-only` 不转发键盘输入；`--replay` 先将上行通道读偏移设为零，以便显示缓冲区中已有内容，不能理解为回放全部历史日志。退出用 Ctrl-C，终端设置会恢复。

配置与临时覆盖示例：

```toml
[rtt]
symbol = "_SEGGER_RTT"
control_block_address = ""
poll_interval = 0.02
```

```sh
rvdbg rtt --symbol _SEGGER_RTT --poll 0.02
rvdbg rtt --cb 0x60000000 --output-only
```

最后一条的地址只是语法示例，必须替换成项目真实控制块地址。`--cb` 绕过符号查找，不能绕过固件尚未初始化、地址不可读或缓存一致性问题。TRACE32 还需正确的符号程序名 `project.program`。持续等待时按[排障](troubleshooting.md)逐项确认。

## FDX：链接属于一个镜像

rustvisor 使用 FDX BufferE 链接，默认符号是 `RUSTVISOR_FDX`。同一静态区域内相邻存放发往目标和发往主机的两个通道。主机通过 RAM 读写它们，因此两种后端都能使用。

```sh
rvdbg fdx
rvdbg fdx --guest 0
```

第一条默认选主 ELF；`--guest 0` 选 `project.extra_elf` 的第一个镜像，不是 VMID 0 或硬件核 0。交互终端把输入的每一行当作一个包，收到的包逐行显示。在终端里收到的包以 `< ` 开头，`> ` 提示符标出输入行；通过管道时每行就是包本身。目标复位后重新打开链接时终端会重新跟上。

`--symbol` 可选择其他符号；`--address` 直接给地址，跳过 ELF 符号查找。符号和镜像选择规则与[项目配置](configuration.md)一致，地址必须指向实际有效的 FDX 链接。

## 脚本收发与超时

下面的消息是协议示例，仅适用于实现了这些消息的镜像：

```sh
rvdbg fdx --send "boot single" --expect "ok boot" --timeout 10 --json
```

工具先按顺序发送全部 `--send` 包，再按顺序等待 `--expect` 包。重复选项可以发送或等待多个包。`--timeout` 分别约束等待链接打开、发送空间及每个期望包的等待，不能理解为整条命令的总时间上限。期望包没有按时到达时返回 3。

FDX 只提供传输，消息的含义由固件定义；一个字符串可能触发启动、测量或状态修改。`--json` 只改变输出形式，不改变这些作用。脚本怎样判断结果见[自动化](automation.md)。

## 谁拥有缓冲区？

同一 FDX 链接同一时刻只能由一个主机程序驱动。不要同时让板级 runner、`rvdbg fdx` 和 PowerView `FDX.*` 窗口使用它。RTT 也应避免多个读者竞争同一个通道的读偏移。

`rvdbg fdx --close` 将链接标成关闭；它用于板子保持复位、RAM 还残留上次打开状态的特定场景，会修改 RAM，不是通用的“断开主机连接”按钮。不要在固件或 runner 正在使用链接时执行。
