# Timeline data format `rustvisor-timeline/3`

What the timeline page (`crates/rustvisor-debug/assets/timeline.html`) reads. `rvdbg swtrace --html` writes it
(`src/timeline.rs`) and embeds it in the page; any other tool may write it too: put it into
`<script id="timeline-data" type="application/json">` of the page, or serve the page with
`?data=FILE` next to it.

```json
{
  "format": "rustvisor-timeline/3",
  "title": "…", "lede": "…", "meta": […], "coverage": {…}, "footnote": "…", "resolution_ns": 50, "boundary": "…",
  "fold_ns": 10000000,
  "runs": [{
    "key": "both", "name": "…", "note": "…", "source": "el2-ping-traced.csv",
    "t0": 26843528498275, "span": 12563988050,
    "units": [{"vm": "VM1", "core": "C0C0", "ring": "EL2 ring C0C0 + guest0 ring",
               "segs": [[0, 58250, "unknown", ""],
                        [58250, 61750, "el2", "hvc", {"vec": 5, "detail": "r0=0x00000000"}],
                        [61750, 12563986650, "el1", "app"]],
               "pending": [[6466, 6748, "SGI", {"msg": 0}]]}],
    "msgs": [[1, 0, "VM1", "VM2", 12498386900, 12498397500]],
    "unpaired": 0
  }]
}
```

- `fold_ns` (optional, top level): the page folds every stretch longer than this with no record on any
  unit (segment edges, `sent`, pending edges, IPC records; EL2, ISR and IPC blocks are never folded)
  into a narrow break labelled with its length, on the time axis and the overview. Default 10 ms
  (`10000000`), `0` turns folding off; the viewer can change it under Options. Folded time is not
  counted in the window's statistics (not as EL1, not as before START).
- Times are ns from the run's first record (`t0` is that record's raw timestamp); `span` is the
  run's length. `resolution_ns` (optional): the step the counter values move in. `boundary`
  (optional): what the block edges leave out, shown in the details box (a generic text otherwise).
- Rows are exception levels: EL1 on top, EL2 below (Pending under it only on request). The step line follows the level.
- `segs`: where the CPU is, `[start, end, level, kind, extra?]`, in time order, not overlapping:

  | level | kind | extra | drawn |
  |---|---|---|---|
  | `el1` | `app` (guest code) | | green |
  | `el1` | `isr` | `name`, `msg`, `parts` (spans inside it, `[start, end, kind]`, e.g. the `ipc-read` of the ISR that receives) | orange; parts as a thin outlined bar inside |
  | `el1` | `ipc-write` (guest-ipc send) / `ipc-read` (the receiving ISR: IAR, recv until empty, EOI) | `msg` | teal, filled / outlined; labelled IPC write / IPC read |
  | `el2` | how EL2 was entered (`debug/swtrace/FORMAT.md` EL2_EXIT flags): `hvc`, `IRQ` (physical interrupt), `SGI` (the guest wrote ICC_SGI1R, trapped), `notice` (FIQ from another core's EL2) | `op` (what EL2 does: `notify send`, `notice → inject`, `inject IRQ`; the block's label), `vec` (entry vector), `detail` (hvc: `r0`), `sent` (when EL2 wrote the notice carrier; drawn as a tick, links start there), `msg` | entered via HVC steel blue, else navy |
  | `unknown` | empty (before the ring's START) | | hatched |

  Without `op` an EL2 block is labelled by how it was entered (`hypercall`, `IRQ`, `SGI`).
- `pending` (optional, per unit): interrupt raised → this core enters EL2, `[start, end, source,
  extra?]`. Source `timer` (from the deadline, CVAL) or the notice carrier, `SGI 14` in a cluster or
  `IBCM` across clusters (from the sender's EL2 writing it; includes propagation, the receiver's
  exception entry and entry stub, and any time the receiver could not take it), `extra.msg` for a
  carrier of a message. Overlaps EL1 (the guest keeps running), so it is its own list; drawn as a
  thin slate-blue bar, but only when the viewer turns on Options → Pending row. By default a timer's
  pending interval is a ▼ at the deadline on the EL2 row with a line to the EL2 entry, and a
  carrier's is the horizontal part of the message link: down from the tick to the receiver's EL2
  row, then along its top to the EL2 entry. The page works out the part where the receiver was in
  EL2 (interrupts masked: the interrupt waited for that eret); it is solid on the link and striped
  in the Pending row.
- `msgs`: `[direction, sequence, from VM, to VM, IPC_SEND time, IPC_RECV time]`, one per paired
  message. Every segment and pending interval with `msg` = i belongs to message i: clicking any of
  them highlights the whole chain and lists its stages (an EL2 block with `sent` as entry → carrier
  written and carrier written → exit, an ISR with its parts, a pending interval with its waiting
  part). Without such blocks (what `rvdbg swtrace` writes today) the two records are markers on
  the EL1 row.
- Not resolved by these records: an EL2 block starts after the exception entry and the entry stub
  and ends before the EL2_EXIT record is written, the exit stub runs and eret returns; those parts are
  drawn as EL1 (`boundary` says so on the page).
- What `rvdbg swtrace --html` writes: `el1`/`app`, `el2` blocks by reason with `vec` and, for
  `hvc`, `detail`, `unknown` before a channel's `START`, and `msgs`. ISR and IPC blocks, `op`,
  `sent` and `pending` need records the images do not write yet; another tool can write them.
