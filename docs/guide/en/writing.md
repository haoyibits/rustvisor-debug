# Maintain this guide

[中文](../cn/writing.md)

Chinese task pages are flat within `docs/guide/cn/`, with matching English files under `docs/guide/en/`. The root holds the language entry page, `SUMMARY.md` and `book.toml`. Keep short topics in one file; add directories only for substantial independently maintained topics. `README.md` provides entry points and `SUMMARY.md` the mdBook order; neither replaces the content.

## Follow one concrete operation

State project, backend, images and endpoints. Explain prerequisites, commands, target effects, expected results and next steps on failure. Put backend differences beside the affected step.

Explain side effects at first use, distinguishing flash, reset, halt, symbol loading, live RAM and firmware messages. Ordinary RAM examples do not generalize to MMIO. Mark sample addresses, symbols, messages and fictional registers that must be replaced.

Check behavior against current [CLI definitions](https://github.com/haoyibits/rustvisor-debug/blob/main/crates/rustvisor-debug/src/main.rs), [initialization template](https://github.com/haoyibits/rustvisor-debug/blob/main/crates/rustvisor-debug/src/init.rs), [configuration parser](https://github.com/haoyibits/rustvisor-debug/blob/main/crates/rustvisor-debug/src/config.rs), [session protocol](https://github.com/haoyibits/rustvisor-debug/blob/main/crates/rustvisor-debug/src/session.rs) and backend implementation. Links to `main` track the main branch; select the appropriate tag/commit for a specific release.

Source and simulated tests establish implementation/expectations, not a successful board run. Hardware claims need dates, images, backend, criteria and evidence locations. Guide examples are not measurement records.

## Links and backlinks

Use standard relative Markdown links such as `[configuration](configuration.md)` across GitHub, mdBook and Obsidian. Keep one primary explanation of each behavior and link to it where needed.

Open `docs/guide/` as an Obsidian vault and use Markdown link format. At build time `tools/docs/links.py` adds “Referenced by” to English pages and “引用此页” to Chinese pages. It changes build content only and does not count generated backlinks as new references.

Add every page to `SUMMARY.md`; validation rejects omissions, duplicates and broken local destinations. Use repository web URLs for sources/examples outside the guide. Reference links are supported; fenced/inline code examples do not count as references.

Maintain matching filenames and coverage in both languages, and keep direct language-switch links. Both languages are built together with one configuration and separate table-of-contents groups. Keep English links within `en/` except for explicit language switches.

## Build and check locally

Run at repository root with Python 3.10+ and mdBook 0.5.4. These are documentation dependencies, not rvdbg runtime dependencies:

```sh
cargo install mdbook --version 0.5.4 --locked
python3 -m venv target/docs-venv
. target/docs-venv/bin/activate
python -m pip install -r tools/docs/requirements.txt
python -m unittest discover -s tools/docs -p 'test_*.py'
mdbook build docs/guide
python tools/docs/links.py --check-html target/guide
```

Output is under `target/guide/`: open `index.html` to choose a language, `cn/index.html` for Chinese, or `en/index.html` for English. For live rebuilding use `mdbook serve docs/guide --open`. Keep the virtual environment active because mdBook invokes `python3` for the preprocessor. See the [mdBook preprocessor documentation](https://rust-lang.github.io/mdBook/format/configuration/preprocessors.html).

Check navigation, backlinks and long tables visually, then run `git diff --check`. HTML checks cover local files and fragments, not external site availability or board behavior.
