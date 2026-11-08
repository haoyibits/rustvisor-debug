# 脚本、JSON 与 AI agent

[English](../en/automation.md)

自动化首先要区分两种调用：一次性命令靠进程退出码判断；长期管道会话靠每条命令的结束记录判断。二者都应固定[配置与镜像](configuration.md)，记录 rvdbg 版本，并为等待操作设置期限。

## 一次性命令：先检查退出码

```sh
rvdbg status --json
rvdbg verify --json
rvdbg check checks/boot.toml --variant debug --json
```

把 `--json` 放在命令参数后面，尤其 `cmd`、`eval`、`do` 这类接收剩余文本的命令。查看命令的典型结果封装如下，`result` 的具体字段随命令而变：

```json
{"command":"ram","ok":true,"exit_code":0,"result":{"symbol":null,"address":"0x60000000","words":[1],"hex":["0x00000001"]}}
```

这是格式示例，不是一次板上读取结果。`--json` 并不保证任意命令、任意失败阶段都只在 stdout 输出 JSON；配置加载失败等早期错误仍可能只有 stderr。不要把 stderr 合并进待解析的 JSON。

| 退出码 | 含义 | 脚本应如何处理 |
| --- | --- | --- |
| 0 | 命令完成、条件满足 | 再按需要解析结果 |
| 1 | 执行失败 | 保存错误，区分配置、连接和读取问题 |
| 2 | 用法错误 | 修正参数，不重试目标操作 |
| 3 | 已执行但结果不符合预期 | 保存差异或失败条件，不能当作连接失败 |

返回 3 的场景包括 `verify` 不一致、`check` 条件未满足、`run-until` 停在其他位置，以及 FDX 期望包超时。不要无条件重试 `flash`、`reset` 或协议消息；重试会再次改变目标。

一个只读取状态的 Python 示例：

```python
import json
import subprocess

completed = subprocess.run(
    ["rvdbg", "status", "--json"],
    capture_output=True, text=True, timeout=15,
)
if completed.returncode != 0:
    raise SystemExit(completed.stderr or completed.stdout)
report = json.loads(completed.stdout)
if not report["ok"]:
    raise SystemExit(report)
print(report["result"])
```

对于可能返回 3 的命令，先保存 stdout 中的结构化报告，再按退出码分类。烧录的超时预算应覆盖实际 Flash 耗时，不能照搬上面的 15 秒。

## 多条命令：NDJSON 管道会话

`rvdbg --json` 在 stdin 来自管道而非终端时启用机器会话：

```sh
printf '%s\n' 'status' 'quit' | rvdbg --json
```

首先输出 ready 记录，再逐行执行命令，并在每条命令自己的输出后追加 end 记录。以下省略了命令结果，值仅作示意：

```json
{"ready":true,"version":"0.1.25","config":"/work/project/rvdbg.toml","backend":"openocd","reason":"..."}
{"end":"status","ok":true,"exit_code":0,"error":null}
{"end":"quit","ok":true,"exit_code":0,"error":null}
```

`ready: true` 表示进入协议循环，不代表目标检查成功；还需要检查 `backend`、连接信息和实际命令结果。未能确定后端时 `backend` 可以为 null。

客户端按下面的顺序驱动：

1. 启动进程，读取 ready，确认版本、配置路径和后端符合预期。
2. 写一条命令并 flush；持续收集输出，直到对应 `end`。
3. 以该记录的 `ok`、`exit_code`、`error` 判断结果，再决定下一条操作。
4. 最后发送 `quit` 或关闭 stdin，等待进程退出并收集 stderr。

查看命令、`ram` 和 FDX 单次收发提供结构化输出；`flash`、`load` 等仍可能输出进度文本。机器客户端应把这些文本保存为日志，并识别已约定的 JSON 记录，不能假定每一行都是同一结构。持续交互的 `rtt`/裸 `fdx` 不适合作为普通请求插入该循环；FDX 使用带超时的单次收发模式，见[通信](channels.md)。

**会话读到 EOF 后可以返回 0，即使之前某条命令失败。** 所以 shell 管道最后成功不能证明所有检查成功，必须消费每个 end。会话还会在命令失败后继续接收输入；若失败后不应继续执行，客户端需要等 end 后再决定，而不是预先把整段破坏性流程全部写入。

客户端也应为启动、每条命令和进程退出设置超时，并持续处理 stderr，避免日志管道填满。连接丢失后先检查目标状态，不要直接重放先前命令。

## 与 rustvisor-sdk 板级 runner 配合

当前 SDK 集成使用已安装的 rvdbg（至少 0.1.24，或 runner 支持的 `RVDBG` 路径覆盖），通过 `rvdbg --json` 通信，每次烧录使用一个会话。镜像通过 `ELF`、`EXTRA_ELF`、`PROGRAM_NAME` 选择；具体测试计划仍属于 SDK 的 runner。

编写自己的 runner 时，把 ready/end 协议和命令结果作为接口，不解析带颜色的人类表格。记录镜像、配置、版本和检查报告，才能区分“内容写入一致”“检查满足”和“业务运行正确”。现有 Rust 库集成说明见[开发者后端文档](https://github.com/haoyibits/rustvisor-debug/blob/main/docs/backend.md)，它与 SDK 当前通过子进程调用的路径不同。

## AI agent 使用哪一层？

```sh
rvdbg skill
rvdbg skill --dir /path/to/agent/skills
```

默认安装到 `$CLAUDE_CONFIG_DIR/skills` 或 `~/.claude/skills`，升级 rvdbg 后重新安装以更新说明。该 skill 说明命令选择、后端限制，以及目标状态变更的确认要求；不要让 agent 自动添加 `--write-otp`。

TRACE32 通用断点、单步、调用栈和交互调试可配合 Lauterbach t32mcp；烧录、专用诊断、验证、时钟和 RAM/FDX 流程由 rvdbg 提供。它们共享同一个 PowerView，PRACTICE 脚本同一时间只能运行一个。StellarLINK 使用 rvdbg 与 GDB 路径，详见[调试](debugging.md)。

如果为 agent 配置免确认规则，依据[操作影响](README.md)和实际参数限制范围。`check` 可带 `--halt`，`ram` 可带 `--write`，不能仅用宽泛命令前缀把所有调用当作只读。
