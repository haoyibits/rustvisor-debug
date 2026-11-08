# 维护这本手册

[English](../en/writing.md)

中文正文按任务平铺在 `docs/guide/cn/`，对应英文页面放在 `docs/guide/en/`。根目录保留语言入口、`SUMMARY.md` 和 `book.toml`。一个短主题使用一个文件；只有出现多个需要独立持续维护的长主题时，才考虑增加目录。`README.md` 提供任务入口，`SUMMARY.md` 维护 mdBook 阅读顺序，二者都不应替代正文。

## 从一次操作往前写

每篇先交代适用项目、后端、镜像及起终点。主线依次说明前提、命令、目标影响、预期结果和失败后下一步。后端差异放在发生差异的位置，不要求用户拼接两套完整教程。

命令第一次出现时解释它改变什么。特别区分烧录、复位、停核、加载符号、运行中 RAM 访问和固件消息。普通 RAM 示例不能推广到 MMIO；示例地址、符号、协议消息和虚构寄存器要明确说明需要替换。

CLI 默认值、选项及行为先核对当前实现，再修改手册。主要依据是 [CLI 定义](https://github.com/haoyibits/rustvisor-debug/blob/main/crates/rustvisor-debug/src/main.rs)、[初始化模板](https://github.com/haoyibits/rustvisor-debug/blob/main/crates/rustvisor-debug/src/init.rs)、[配置解析](https://github.com/haoyibits/rustvisor-debug/blob/main/crates/rustvisor-debug/src/config.rs)、[会话协议](https://github.com/haoyibits/rustvisor-debug/blob/main/crates/rustvisor-debug/src/session.rs)及对应后端。网页链接的 `main` 跟随仓库主线；排查特定发布版本时，应切到对应 tag/commit 核对。

源码和模拟测试说明实现及预期，不说明某次板上操作已通过。声称上板验证时，应给日期、镜像、后端、判据和记录位置；本手册的命令与输出示例不作为实测记录。

## 正文链接与自动反向链接

使用标准 Markdown 相对文件链接，例如 `[配置](configuration.md)`，便于 GitHub、mdBook 和 Obsidian 共同阅读。每个行为保留一个主要解释位置，其他页面在需要处引用它。不要为增加关系数量而互相堆链接。

把 `docs/guide/` 作为 Obsidian vault 打开，使用 Markdown 链接格式即可查看编辑器反向链接。mdBook 构建时由仓库的 `tools/docs/links.py` 解析正文链接，在页面末尾添加“引用此页”；只修改构建内容，不回写 Markdown，也不把生成的反向链接再次算入引用关系。

新正文页必须加入 `SUMMARY.md`。工具会检查遗漏、重复收录、文件是否存在，以及链接目标是否属于手册。指向手册之外的源码、示例或旧文档时，使用仓库网页链接，避免生成网站后相对路径失效。参考式链接也可使用，代码块和行内代码中的示例链接不计入引用。

中英文保持相同文件名和内容覆盖，每页提供双向语言切换。两种语言使用同一配置构建，目录按语言分组；英文正文除语言切换外链接到英文页面。

## 本地构建与检查

在本仓库根目录执行，文档依赖只用于构建手册，不是 rvdbg 运行依赖。需要 Python 3.10+ 和 mdBook 0.5.4：

```sh
cargo install mdbook --version 0.5.4 --locked
python3 -m venv target/docs-venv
. target/docs-venv/bin/activate
python -m pip install -r tools/docs/requirements.txt
python -m unittest discover -s tools/docs -p 'test_*.py'
mdbook build docs/guide
python tools/docs/links.py --check-html target/guide
```

构建结果在 `target/guide/`，语言入口为 `index.html`，中文打开 `cn/index.html`，英文打开 `en/index.html`；需要自动重建预览时执行 `mdbook serve docs/guide --open`。启动 mdBook 时应保持虚拟环境激活，因为它通过 `python3` 调用预处理器。构建过程的依赖机制见 [mdBook 预处理器说明](https://rust-lang.github.io/mdBook/format/configuration/preprocessors.html)。

交稿前检查目录、页面间跳转、反向链接和长表格显示，并执行 `git diff --check`。HTML 检查确认生成文件和片段链接有效；它不验证外部网站可达性，也不代替实际操作命令的板级验证。
