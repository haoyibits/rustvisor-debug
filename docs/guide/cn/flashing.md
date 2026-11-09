# 烧录、加载符号与验证

[English](../en/flashing.md)

先用[配置检查](configuration.md)确认镜像集合和后端。这里的三个命令解决不同问题：

| 命令 | 使用时机 | 目标影响 |
| --- | --- | --- |
| `rvdbg flash` | 程序内容需要更新 | 写入 Flash、复位，正常完成后运行 |
| `rvdbg load` | TRACE32 需要加载当前 ELF 的符号 | 不写程序、不复位；会在需要时继续运行 |
| `rvdbg verify` | 确认目标内容与本地 ELF 相符 | 读取并比对，不自动停核或复位 |

如果重新编译改变了程序内容，`load` 不会把新程序写进板子。符号与目标代码不一致时，断点、PC 符号和栈解释都会失真。

## 多个镜像要一起声明

```toml
[project]
elf = "build/el2.elf"
extra_elf = ["build/guest.elf", "build/other-guest.elf"]
```

`flash` 一次处理主镜像和所有额外镜像；TRACE32 的 `load` 也加载它们的符号；不带 ELF 参数的 `verify` 依次检查整个集合。

TRACE32 使用 `FLASH.ReProgram ALL /Erase`：烧录脚本声明的扇区即使这次没有镜像写入，也可能被擦除。因此不要把 hypervisor 和 Guest 分成两次烧录来替代 `extra_elf`，也不要为了临时烧一个镜像而随意清空列表。

## TRACE32：从脚本到运行

`rvdbg flash` 会在需要时启动 PowerView。烧录脚本必须支持 `PREPAREONLY`：它只初始化目标并声明 Flash 区域，随后由 rvdbg 执行写入。目标已处于 up 状态时，先 down，避免沿用应用留下的 MPU 等状态运行烧录算法。

主要烧录顺序是：

```text
脚本 PREPAREONLY
FLASH.ReProgram ALL /Erase
Data.LOAD.Elf <主镜像>
Data.LOAD.Elf <额外镜像> /NoClear    # 每个额外镜像一次
FLASH.ReProgram OFF
SYStem.Down
SYStem.Up
```

后续设置调试环境、加载符号并在需要时运行。该路径不会自动执行本工具 `verify` 的逐段回读；需要确认内容时再执行 `rvdbg verify`。

脚本选择先看显式 `script`，否则用 `chip`，为空时用 `trace32.target.cpu` 匹配。候选先查本机 `rvdbg/flash` 库，再查 TRACE32 安装目录的 `demo/*/flash`：

```sh
rvdbg chips SR6P6
rvdbg flash --chip SR6P6
rvdbg flash --script '~~/demo/arm/flash/my_chip.cmm'
```

自动匹配要求脚本有合适的 `; @Chip:` 和 `PREPAREONLY` 支持。精确匹配优先于通配符，更具体的通配符优先；仍然歧义时应明确指定脚本。若脚本接受 `CPU=` 且配置没有提供，rvdbg 会补充芯片参数。其他参数通过 `[trace32.flash] args` 指定。

`flash`、`load` 不会抢占正在运行的 PRACTICE 脚本；遇到 `PowerView is busy` 时按[排障](troubleshooting.md)处理。

## StellarLINK：写入、回读和运行

正常过程是 `reset init`，读取 Flash 扇区布局，擦除每个镜像涉及的扇区，分块写入，通过 `memory_ap` 回读可加载段并比对，最后 `reset run`。同一扇区内的多个段先合并，空隙和扇区尾部按 `0xFF` 填充。

写入和回读比对分别显示按已完成字节数计算的百分比，写入总量包含填充字节。终端显示原地刷新的进度条；重定向输出时使用普通进度行。擦除和初始化单独显示步骤，不显示百分比。

```sh
rvdbg --backend openocd flash
rvdbg --backend openocd flash --no-run
```

`--no-run` 使烧录后目标保持停着；`--no-verify` 跳过烧录后的比对。两者以及 `--write-otp` 都只适用于 OpenOCD；TRACE32 的 `--chip`、`--script` 则不能用于 OpenOCD。

一般保留默认验证。发生差异时退出码为 3，先核对报告中的镜像、地址和读回内容，不应把跳过验证当作修复。

### OTP 为什么会阻止烧录？

写入之前，工具先检查镜像中落入 `openocd.otp` 范围的内容。和目标相同则无需更改；不同则拒绝写入，除非显式使用 `--write-otp`。即便允许写入，需要将某一位从 0 改为 1 的请求仍会被拒绝，因为 OTP 不能擦除。

SR6P6 默认范围是存放启动记录的 UTEST：`start = 0x29F87000`、`size = 0x1000`。保护范围来自项目配置，并不自动覆盖所有芯片的一次性区域。`--no-verify` 不关闭 OTP 检查。

收到 OTP 差异后，先判断是不是选错镜像、启动记录变更或保护范围设置错误。只有确实计划永久改变该区域时，才由操作者决定使用 `--write-otp`。

## 单独核对板子上是什么

```sh
rvdbg verify
rvdbg verify build/el2.elf
rvdbg --backend trace32 verify --t32
```

比对的是 ELF `PT_LOAD` 段中的文件内容及其加载地址 LMA（`p_paddr`），不是初始化后 RAM 的全部运行状态。启动时复制到 RAM 的初始化数据因此在其加载位置比对；BSS 和业务状态应使用[检查文件](inspection.md)。

`verify` 无路径时检查配置中的全部镜像，显式给路径时只检查该 ELF。发现不一致时报告差异并返回 3；读取或解析失败返回 1。

TRACE32 的 `--t32` 额外使用 `Data.LOAD.Elf /DIFF /PHYSLOAD /NoRegister /NosYmbol /NoClear` 交叉比对，不改目标内存、PC 和已加载符号。

内容匹配后，继续用[状态与项目检查](inspection.md)确认程序运行到了预期位置，再接入[IDE 调试](debugging.md)。
