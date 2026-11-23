# timeline

Multi-run VM state timeline pages from saved `rvdbg swtrace --csv` files, and a synthetic run
of the planned EL2 notify path (rustvisor-sdk `docs/26-el2-notify-plan.md`), drawn with rvdbg's
page (`crates/rustvisor-debug/assets/timeline.html`, the one copy). One run from the board needs
none of this: `rvdbg swtrace --html run.html`, or `rvdbg swtrace --from run.csv --html run.html`.

```bash
# one run per CSV, default VM/core mapping (five-VM PLAN)
python3 timeline.py trace.csv -o run.html --name "ping-traced" --note "VM1 pings VM2 and VM3"
# a spec: page texts, several runs, coverage table
python3 timeline.py specs/board-2026-11-23.json -o out/board-2026-11-23.html
python3 timeline.py specs/sample.json -o out/sample.html
```

| File | What |
|---|---|
| `timeline.py` | the generator: CSVs (or synthetic data) → data → rvdbg's page with the data embedded (`--template` for another) |
| `specs/*.json` | page texts and the runs of one page |
| `out/*.html` | generated pages, self-contained (not in git) |
| `samples/*.csv` | board records, SR6P6, 2026-11-23, ETM off; also rvdbg's test data (`crates/rustvisor-debug/tests/timeline.rs`) |
| `legacy/` | the earlier pages and scripts this replaces (not in git, local only) |

- `timeline.py` writes the data into `<script id="timeline-data">` of the template. With
  `--json FILE` it also writes the data alone. The template opened without data fetches
  `?data=FILE` (default `timeline.json`) next to it.
- `--map KEY=VM[:CORE]` (or `"map"` in a spec) says which VM a core runs (`C0C0=VM1`) and
  which VM and core a guest ring belongs to (`guest0=VM1:C0C0`); the CSV says neither.
  Default: the five-VM image's PLAN (machine k = guest k−1).
- Pages over ~1 MB do not open from `file://` in the Claude browser pane; serve the folder
  (`python3 -m http.server`) or open them in a normal browser.

## Spec

```json
{
  "title": "…", "lede": "…",
  "meta": [{"label": "Source", "text": "…"}, {"label": "Sample data", "text": "…", "warn": true}],
  "map": {"C0C0": "VM1", "guest0": "VM1:C0C0"},
  "runs": [
    {"key": "both", "name": "EL2 + IPC run", "note": "…", "csv": "../samples/el2-ping-traced.csv"},
    {"key": "sample", "name": "…", "synthetic": {"seed": 20261123, "span_us": 300},
     "select": ["VM1", "VM2"], "window": [0, 160000],
     "presets": [{"label": "IPC three-way", "units": ["VM1", "VM2", "VM3"]}]}
  ],
  "coverage": {"title": "…", "head": ["Shown", "…"],
               "rows": [["text", {"text": "recorded", "ok": true}, {"text": "…", "ok": false, "span": 2}]]},
  "footnote": "…"
}
```

`csv` paths are relative to the spec. `select` (panels shown first), `window` (first view, ns) and
`presets` (buttons that pick panels) are optional per run; they are copied into the data.

## Data format

`rustvisor-timeline/3`: `docs/timeline-format.md`. `timeline.py` additionally writes
what the board does not record yet (ISR and IPC blocks, `op`, `sent`, `pending`) in the synthetic run.
