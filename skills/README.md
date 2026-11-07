# Agent skills

The skill that tells an AI agent (Claude Code's layout) which command does
what. It is the only source: rvdbg compiles it in with `include_str!` and
installs it with the version filled in.

| Skill | Install | Installed to |
|---|---|---|
| [`rvdbg`](rvdbg/SKILL.md) | `rvdbg skill [--dir <skills>]` | `~/.claude/skills/rvdbg/SKILL.md` |

The default directory is `$CLAUDE_CONFIG_DIR/skills`, else
`~/.claude/skills`. A changed installed file is kept as `SKILL.md.bak`.

`{version}` in the frontmatter is replaced by the program's version when
it installs the skill; `rvdbg config` uses it to
say when the installed skill is missing or out of date. Copying a file
from here by hand works too, but then the version check reports it as
out of date.
