# 从安装到第一次烧录

[English](../en/quickstart.md)

本页跟踪一次 SR6P6 项目操作：主机上已有构建成功的 ELF，接好板子和调试器，生成配置，烧录，然后查看运行状态。rvdbg 不替项目编译固件。

## 安装 rvdbg 和所需后端

macOS Apple 芯片或 Linux 上可用 Homebrew：

```sh
brew install haoyibits/tap/rvdbg
rvdbg --version
```

formula 会一起安装 `openocd-stellar`。使用 TRACE32 仍需自行安装 PowerView 和 `t32debugadapter`；它们不包含在 rvdbg 里。

不使用 Homebrew 时，可以安装发布二进制：

```sh
curl -fsSL https://github.com/haoyibits/rustvisor-debug/releases/latest/download/install.sh | sh
```

安装脚本默认写入 `~/.local/bin`。该目录需在 `PATH` 中；`RVDBG_INSTALL_DIR` 可改变目录，`RVDBG_VERSION` 可指定发布版本。脚本不会安装 `openocd-stellar`。从源码安装则在本仓库执行：

```sh
cargo install --path crates/rustvisor-debug
```

浏览器下载的 macOS 二进制若被 Gatekeeper 拦截，确认文件来自预期发布后，对实际二进制路径执行：

```sh
xattr -d com.apple.quarantine /path/to/rvdbg
```

## 准备调试器

**TRACE32：** PowerView 默认从 `~/t32` 查找。若安装在别处，在配置中设置 `trace32.sys`。Remote API 必须使用 TCP。已有 `config.t32` 可写：

```text
RCL=NETTCP
PORT=20000
REMOTEHOSTDENY
```

若文件未定义 `RCL=`，rvdbg 启动 PowerView 时会传入 TCP Remote API 参数，并限制为本机访问。已有 `RCL=` 时，要确保协议和端口与 `trace32.rcl_port` 一致。Remote API 没有认证，能连接它就能执行 PRACTICE 和主机命令，因此应保留本机访问限制。

**StellarLINK：** 使用 `openocd-stellar`，不要用缺少 Stellar 支持的普通 OpenOCD 替代。SR6P6 EVB 默认脚本是 `board/sr6p6_evb.cfg`。如果要用 VS Code，还需要 Cortex-Debug；直接用 GDB 则需要 `arm-none-eabi-gdb`。

## 在固件项目中生成配置

以下操作在待调试的固件项目根目录执行，不是在 rvdbg 自己的源码目录执行：

```sh
cd ~/work/rustvisor-sdk
rvdbg init
$EDITOR rvdbg.toml
```

`init` 不会覆盖已有文件。它生成 SR6P6 默认配置，请先核对 `[project]`：

```toml
[project]
program = "boot_diagnostic"
elf = "target/thumbv8r-none-eabihf/release/boot_diagnostic"
extra_elf = ["target/thumbv8r-none-eabihf/release/scenario-guest"]
```

把路径换成这次构建的实际产物；没有额外镜像时使用 `extra_elf = []`。若镜像会同时部署 hypervisor 和 Guest，应将它们一起列出，原因见[多镜像烧录](flashing.md)。

## 先检查工具将要操作什么

```sh
rvdbg config
```

检查配置路径、主镜像与额外镜像是否存在、发现了哪个探针，以及实际选择了哪个后端。两个探针都插着时默认选 TRACE32；本次想用 StellarLINK 可执行：

```sh
rvdbg --backend openocd config
```

`config` 能发现配置错误和缺失文件，但它的成功不代表 JTAG、目标供电或 Flash 操作已经通过。更改后端或核的规则见[配置](configuration.md)。

## 烧录并确认结果

确认镜像和目标板正确后执行：

```sh
rvdbg flash
rvdbg verify
rvdbg status
```

`flash` 会擦写并复位目标，正常完成后运行程序。`verify` 不带路径时检查全部已配置镜像；退出码 0 表示可加载内容与目标一致。`status` 显示目标在运行还是停着，停着时还会显示 PC/CPSR。

`verify` 一致说明镜像内容一致，不能单独证明启动成功或业务逻辑正确。需要观察日志或项目验收条件时，继续看[RTT](channels.md)和[检查文件](inspection.md)。烧录失败时先看[排障](troubleshooting.md)，不要用跳过验证或写 OTP 来替代定位问题。

接下来可用 `rvdbg vscode` 生成 IDE 配置，或直接运行 `rvdbg` 进入[交互会话](debugging.md)。
