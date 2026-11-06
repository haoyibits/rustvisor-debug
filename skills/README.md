# Agent skills

The skills that tell an AI agent (Claude Code's layout) which command does
what. They are the only source: the binaries compile them in with
`include_str!` and install them with the version filled in.

| Skill | Install | Installed to |
|---|---|---|
| [`rvdbg`](rvdbg/SKILL.md) | `rvdbg skill [--dir <skills>]` | `~/.claude/skills/rvdbg/SKILL.md` |
| [`tracebridge`](tracebridge/SKILL.md) | `tracebridge skill [--dir <skills>]` | `~/.claude/skills/tracebridge/SKILL.md` |

The default directory is `$CLAUDE_CONFIG_DIR/skills`, else
`~/.claude/skills`. A changed installed file is kept as `SKILL.md.bak`.

`{version}` in the frontmatter is replaced by the program's version when
it installs the skill; `rvdbg config` and `tracebridge config` use it to
say when the installed skill is missing or out of date. Copying a file
from here by hand works too, but then the version check reports it as
out of date.
