# 查看目标与执行检查

[English](../en/inspection.md)

先明确要观察的是运行状态、故障现场、程序内容还是业务条件。查看类命令不会为了满足读取条件而自动停核；`check --halt` 是需要明确选择的例外。

TRACE32 查看命令要求已有 PowerView；StellarLINK 优先复用 Tcl 端口上的 OpenOCD，否则启动不复位目标的实例。连接失败先看[排障](troubleshooting.md)，不要直接复位仍有诊断价值的现场。

## 从状态到异常现场

```sh
rvdbg status
rvdbg halt
rvdbg status
rvdbg fault
```

第一条观察当前运行状态；`halt` 是显式改变目标状态的步骤。核停着时，`status` 显示 PC、符号偏移与解码后的 CPSR。`fault` 面向 AArch32 Hyp 异常，报告向量、HSR、HDFAR/HIFAR、ELR_hyp 及 SPSR_hyp；它不把任何停核都视为一次有效 Hyp 故障。

StellarLINK 的核心及协处理器寄存器读取要求核停着，Hyp 寄存器还要求核停在 EL2。停在 Guest EL1 后报错，并不证明 Hyp 寄存器没有值；需要在 hypervisor 内的合适断点取得现场。TRACE32 对部分 EL1 下的 Hyp 协处理器读取可以改用 `HC15:`，StellarLINK 没有同等替代。

记录结果后是否 `go`、`reset` 或重新部署，由当前诊断任务决定；这些操作见[调试](debugging.md)。

## 读一个值还是读一片 RAM？

| 命令 | 适用对象 | 单位与限制 |
| --- | --- | --- |
| `mem <地址或符号> [个数]` | 少量 32 位内存值；TRACE32 地址访问类；受支持的协处理器地址 | 默认读 1 个 32 位字 |
| `ram <地址或符号> [个数]` | 已知有效的普通 RAM 缓冲区 | 批量 32 位字，地址需对齐；不能用于 MMIO/协处理器 |
| `ram <地址或符号> --write <值…>` | 向普通 RAM 写入 32 位字 | 改变目标内存，操作前确认所有权和地址 |
| `reg <名字…>` | TRACE32 PER 寄存器或字段 | 名称区分大小写，可能需要完整路径 |

```sh
rvdbg mem my_buffer 4
rvdbg ram my_buffer 64 --json
rvdbg --backend trace32 reg HSR HSCTLR.C
```

`ram` 在运行中通过 TRACE32 `AD:` 或 StellarLINK MEM-AP 访问，不经过逐个寄存器表达式求值。地址和长度检查不能证明这片区域确实是普通 RAM；应由项目内存布局确认。目标正在更新的多字缓冲区也不保证是同一时刻的一致快照，需要固件协议或显式停核配合。

写入示例只用于自己控制的普通 RAM：

```sh
rvdbg ram my_buffer --write 0x12345678 0
```

MMIO 应用 `mem` 或项目检查文件，并先确认该寄存器的读取语义。工具没有执行写命令，不代表“读清除”类寄存器不会被硬件改变。

TRACE32 的 `reg` 通过 PER 定义解析名字和字段，再由 PowerView 读取值。重名且地址不同会列出候选路径；用完整路径消歧。`watch` 可打开 PER.Watch 窗口，需要支持该功能的 PowerView 版本，窗口本身不代替记录验收结果。

## 用 check 把人工判断写下来

把检查文件放在被测项目中，例如 `checks/boot.toml`。以下假定项目确实有 `idle_loop` 符号：

```toml
description = "Stopped at the idle loop"

[[check]]
name = "PC is in the idle loop"
read = { core = "PC" }
expect = { in_symbol = "idle_loop" }
```

```sh
rvdbg check checks/boot.toml --dry-run
rvdbg check checks/boot.toml --halt
```

`--dry-run` 解析名字和符号，不读取目标值；它检查文件能否解析，不证明目标满足条件。`--halt` 在需要读取核心/协处理器寄存器时先停核，结束后保持停着；如果要继续，显式执行 `rvdbg go`。

| 读取方式 | 后端 |
| --- | --- |
| `read = { addr = "AD:0x..." }` | 两者；地址和访问类必须适用于对应后端 |
| `read = { core = "PC" }` | 两者，需要停核 |
| `read = { clock = "CORE" }` | 两者，需要可用的时钟描述 |
| `read = { reg = "SCTRL" }` | TRACE32 PER |
| `read = { expr = "..." }` | TRACE32 PRACTICE |

数值条件包括 `eq`、`ne`、`nonzero`、`range`、`in_symbol`、`one_of`；适用的比较可带 `mask`。`eq`、`ne`、`one_of` 可用 `"sym:名称+偏移"` 表示符号地址。`variants = ["debug"]` 限制某项只在指定变体运行：

```sh
rvdbg check checks/boot.toml --variant debug --json
```

全部条件满足返回 0，已执行但条件不满足返回 3，无法执行检查返回 1。公开的[检查文件示例](https://github.com/haoyibits/rustvisor-debug/blob/main/docs/check-example.toml)展示更多语法，其中寄存器和值是虚构的，开头仍使用 tracebridge 命令示例；在 rvdbg 中使用 `rvdbg check`，并替换为项目自己的条件。

`verify` 回答目标的加载内容与 ELF 是否一致；`check` 回答当前观察值是否满足条件。二者配合使用，具体镜像比对规则见[烧录与验证](flashing.md)。

## 时钟树：先提供板子知道的输入

```sh
rvdbg clock
rvdbg clock XOSC=40MHz
rvdbg --backend trace32 clock --html
```

时钟描述通过 `--tree 文件` 指定，或从本机 `rvdbg/clock` 库按配置芯片匹配。`[clock]` 提供寄存器中不存在的信息，例如晶振频率；命令参数可临时提供输入。

输出同时显示当前源、频率和计算依据。`off` 表示关闭；`?` 表示输入或源未知；警告如 `not locked` 来自描述文件规则。相关寄存器读取失败时对应时钟显示错误并返回 1。工具读取到的是该次查询的寄存器值，结果是否正确也取决于描述公式和板级输入是否正确。

`--html` 仅 TRACE32 支持，生成 `.tracebridge/clock.html`，可在浏览器查看模块关系和当前有效路径。新增芯片描述可参考[虚构时钟示例](https://github.com/haoyibits/rustvisor-debug/blob/main/docs/clock-example.toml)，真实芯片公式依据其手册编写，并遵守资料许可。

## Trace：导出已经录下的内容（TRACE32）

rvdbg 不配置 ETM 和板级 trace 硬件。先用项目自己的脚本配置，再运行到目标位置，最后在核停着时导出：

```sh
rvdbg halt
rvdbg do scripts/trace_setup.cmm
rvdbg run-until batch_done --timeout 30
rvdbg trace --last 3000 --json > trace.json
```

脚本和符号需要来自实际项目。`fault --trace` 可把最新记录附在异常报告后。工具读取 PowerView 已记录的数据并重新查询符号，原始导出在 `.tracebridge/trace.ad`；周期列需要周期精确的 trace 配置，不能由普通地址记录推断周期数。
