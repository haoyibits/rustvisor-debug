#!/usr/bin/env python3
"""Build a VM state timeline page: rvdbg's page template + data -> one self-contained HTML.

    # one or more rvdbg swtrace --csv files, one run each
    python3 timeline.py trace.csv -o run.html [--name NAME] [--note NOTE] [--map C0C0=VM1 --map guest0=VM1:C0C0 ...]
    # a spec file: page texts, several runs (csv or synthetic), the coverage table
    python3 timeline.py specs/board-2026-11-23.json -o out/board-2026-11-23.html [--json out/board.json]

The data contract (format "rustvisor-timeline/3") is described in README.md. Times are
ns from each run's first record. The CSV says neither which VM a core runs nor which core
a guest image is on: --map / the spec's "map" give it, default the five-VM image's PLAN
(machine k = guest k-1).
"""

import argparse
import csv
import json
import os
import random
import sys

FORMAT = "rustvisor-timeline/3"
HERE = os.path.dirname(os.path.abspath(__file__))
EL2_PREFIX = "elf:RUSTVISOR_TRACE_"
REASON = {1: "hvc", 2: "IRQ", 3: "SGI", 4: "notice"}
FIVE_VM = {"C0C0": "VM1", "C0C1": "VM2", "C1C0": "VM3", "C1C1": "VM4", "C2C0": "VM5",
           "guest0": "VM1:C0C0", "guest1": "VM2:C0C1", "guest2": "VM3:C1C0",
           "guest3": "VM4:C1C1", "guest4": "VM5:C2C0"}


# ---------------- from rvdbg swtrace --csv ----------------

def load_csv(path):
    rows = []
    with open(path, newline="") as f:
        for r in csv.DictReader(f):
            rows.append({"t": int(r["time"]), "image": r["image"], "ev": r["event"],
                         "flags": int(r["flags"], 16), "value": int(r["value"])})
    if not rows:
        sys.exit(f"{path}: no records")
    rows.sort(key=lambda r: r["t"])
    return rows


def ring_unit(image, vmap):
    """(vm, core, ring name) of a trace ring: a core's EL2 ring or a guest's ring."""
    if image.startswith(EL2_PREFIX):
        core = image[len(EL2_PREFIX):]
        return vmap.get(core, core).split(":")[0], core, "EL2 ring " + core
    vm, _, core = vmap.get(image, image + ":?").partition(":")
    return vm, core or "?", image + " ring"


def run_from_csv(path, key, name, note, vmap):
    """One unit per VM: its core's EL2 ring (state segments) and its guest ring (IPC records)."""
    rows = load_csv(path)
    t0, t1 = rows[0]["t"], rows[-1]["t"]
    units, rings = {}, {}
    for r in rows:
        vm, core, ring = ring_unit(r["image"], vmap)
        r["vm"] = vm
        units.setdefault(vm, {"vm": vm, "core": core})
        if r["image"].startswith(EL2_PREFIX):
            units[vm]["core"] = core
        rings.setdefault(vm, set()).add(ring)
    sends, recvs = {}, {}
    for vm, u in units.items():
        evs = [r for r in rows if r["vm"] == vm]
        el2_ring = [r for r in evs if r["image"].startswith(EL2_PREFIX)]
        # time is accounted from the EL2 ring's START when there is one, else from the guest ring's
        base = el2_ring or evs
        start = next((r["t"] for r in base if r["ev"] == "START"), base[0]["t"])
        u["ring"] = " + ".join(sorted(rings[vm]))
        out = u["segs"] = []
        if start > t0:
            out.append([0, start - t0, "unknown", ""])
        cursor, entry = start, None
        for r in evs:
            if r["ev"] == "EL2_ENTRY":
                if r["t"] > cursor:
                    out.append([cursor - t0, r["t"] - t0, "el1", "app"])
                entry = r
            elif r["ev"] == "EL2_EXIT" and entry is not None:
                reason = REASON.get(r["flags"], "?")
                extra = {"vec": entry["flags"]}
                if reason == "hvc":
                    extra["detail"] = "r0=0x%08X" % r["value"]
                out.append([entry["t"] - t0, r["t"] - t0, "el2", reason, extra])
                cursor, entry = r["t"], None
            elif r["ev"] in ("IPC_SEND", "IPC_RECV"):
                (sends if r["ev"] == "IPC_SEND" else recvs)[(r["flags"], r["value"])] = (vm, r["t"] - t0)
        if t1 > cursor:
            out.append([cursor - t0, t1 - t0, "el1", "app"])
    msgs = []
    for k, (src, ts) in sends.items():
        if k in recvs:
            dst, tr = recvs[k]
            msgs.append([k[0], k[1], src, dst, ts, tr])
    msgs.sort(key=lambda m: m[4])
    return {
        "key": key, "name": name, "note": note, "source": os.path.basename(path),
        "t0": t0, "span": t1 - t0,
        "units": sorted(units.values(), key=lambda u: u["vm"]),
        "msgs": msgs, "unpaired": len(sends) + len(recvs) - 2 * len(msgs),
    }




# ---------------- synthetic run ----------------

def run_synthetic(key, name, note, seed=20261123, span_us=300):
    """IPC over the planned notify path (SDK docs/26-el2-notify-plan.md, not implemented yet):

    sender EL1 writes the slot and publishes (IPC_SEND) -> HVC IPC_NOTIFY -> sender EL2 decides,
    counts, writes the record line and the carrier (Group 0 SGI 14 in the cluster, IBCM across
    clusters) -> reply, EL2_ENTRY record, EL2_EXIT, eret. Carrier pending -> receiver EL2 entered via
    notice (FIQ) -> takes the notice, reads the count, injects a virtual interrupt -> eret -> receiver
    ISR (drawn as one IPC read block): ICV_IAR1, guest-ipc recv until empty (IPC_RECV when
    consumed), ICV_EOIR1.

    VM1 pings VM2 (same cluster) and VM3 (other cluster); VM4 takes CAN interrupts and forwards every
    second frame to VM3 (same cluster); one timer tick per VM. Once, VM3's timer IRQ is in EL2 when a
    notice arrives, so the FIQ waits for its eret. EL2 dwell and ping latency follow the 2026-11-23
    board runs; pending times (timer, carriers) are assumed."""
    rnd = random.Random(seed)
    U = lambda a, b: a + (b - a) * rnd.random()
    T = span_us
    cores = {"VM1": "C0C0", "VM2": "C0C1", "VM3": "C1C0", "VM4": "C1C1", "VM5": "C2C0"}
    segs = {vm: [[0.0, T, "el1", "app"]] for vm in cores}
    pend = {vm: [] for vm in cores}

    def paint(vm, s, e, level, kind, extra=None):
        out = []
        for g in segs[vm]:
            if g[1] <= s or g[0] >= e:
                out.append(g)
                continue
            if g[0] < s:
                out.append([g[0], s] + g[2:])
            if g[1] > e:
                out.append([e, g[1]] + g[2:])
        out.append([s, e, level, kind] + ([extra] if extra else []))
        segs[vm] = sorted(out, key=lambda g: g[0])
        return e

    def timer_irq(vm, at):
        latency = U(0.15, 0.3)
        pend[vm].append([at - latency, at, "timer"])
        return paint(vm, at, at + U(1.6, 1.95), "el2", "IRQ", {"op": "inject IRQ"})

    msgs, dirs, seqs = [], {}, {}

    def send(frm, to, t, collide=False):
        i = len(msgs)
        dcode = dirs.setdefault((frm, to), len(dirs) + 1)
        seq = seqs[dcode] = seqs.get(dcode, -1) + 1
        m = {"msg": i}
        published = paint(frm, t, t + U(0.8, 1.2), "el1", "ipc-write", m)
        sent = published + U(1.05, 1.4)
        # after the carrier: reply, the EL2_ENTRY record (about 0.3-0.4 us with el2-trace), EL2_EXIT's time
        paint(frm, published, sent + U(0.35, 0.5), "el2", "hvc", {"msg": i, "op": "notify send", "sent": sent})
        same = cores[frm][:2] == cores[to][:2]
        arrive = sent + (U(0.25, 0.45) if same else U(0.4, 0.7))
        isrs = []
        if collide:
            busy = timer_irq(to, sent - 0.4)
            arrive = max(arrive, busy + 0.05)
            isrs.append(("timer", U(1.8, 2.6)))
        pend[to].append([sent, arrive, "SGI 14" if same else "IBCM", m])
        t = paint(to, arrive, arrive + U(1.5, 1.85), "el2", "notice", {"msg": i, "op": "notice → inject"})
        for isr, d in isrs:
            t = paint(to, t, t + d, "el1", "isr", {"name": isr})
        # the receiving ISR as one block: ICV_IAR1, guest-ipc recv until empty (IPC_RECV), ICV_EOIR1
        read_e = t + U(0.25, 0.4) + U(1.0, 1.5)
        end = read_e + U(0.2, 0.35)
        paint(to, t, end, "el1", "ipc-read", m)
        msgs.append([dcode, seq, frm, to, published, read_e])
        return end

    def can_irq(vm, t):
        t = paint(vm, t, t + U(1.6, 1.95), "el2", "IRQ", {"op": "inject IRQ"})   # external: no timestamp of the event
        return paint(vm, t, t + U(3, 4.5), "el1", "isr", {"name": "CAN RX"})

    for vm, at in {"VM1": 133, "VM2": 47, "VM4": 160, "VM5": 86}.items():
        e = timer_irq(vm, at)
        paint(vm, e, e + U(1.8, 2.6), "el1", "isr", {"name": "timer"})
    t, rnd_i = 4, 0
    while t < T - 70:
        a = send("VM1", "VM2", t)
        b = send("VM2", "VM1", a + U(8.5, 10.5))
        c = send("VM1", "VM3", b + U(8.5, 10.5), collide=rnd_i == 1)
        d = send("VM3", "VM1", c + U(9, 10.5))
        t, rnd_i = d + U(8.5, 10.5), rnd_i + 1
    t, n = U(8, 14), 0
    while t < T - 20:
        end = can_irq("VM4", t)
        if n % 2 == 1:
            send("VM4", "VM3", end + U(1, 2.5))
        n, t = n + 1, t + U(38, 52)
    ns = lambda x: int(round(x * 1000))

    def conv(extra):
        out = dict(extra)
        if "sent" in out:
            out["sent"] = ns(out["sent"])
        if "parts" in out:
            out["parts"] = [[ns(p[0]), ns(p[1])] + p[2:] for p in out["parts"]]
        return out

    units = []
    for vm, core in cores.items():
        out = [[ns(g[0]), ns(g[1])] + g[2:4] + ([conv(g[4])] if len(g) > 4 else []) for g in segs[vm]]
        units.append({"vm": vm, "core": core, "ring": "synthetic",
                      "segs": [g for g in out if g[1] > g[0]],
                      "pending": [[ns(h[0]), ns(h[1])] + h[2:] for h in sorted(pend[vm], key=lambda h: h[0])]})
    return {
        "key": key, "name": name, "note": note, "source": "synthetic, seed %d" % seed,
        "t0": 0, "span": ns(T), "units": units,
        # in creation order: segments refer to a message by its index here
        "msgs": [m[:4] + [ns(m[4]), ns(m[5])] for m in msgs], "unpaired": 0,
    }


# ---------------- page ----------------

def parse_map(items, base=None):
    vmap = dict(FIVE_VM if base is None else base)
    for it in items:
        k, _, v = it.partition("=")
        if not v:
            sys.exit(f"--map {it}: expected KEY=VM or GUEST=VM:CORE")
        vmap[k] = v
    return vmap


def build_spec(path, map_args):
    with open(path) as f:
        spec = json.load(f)
    root = os.path.dirname(os.path.abspath(path))
    vmap = parse_map(map_args, {**FIVE_VM, **spec.get("map", {})})
    runs = []
    for r in spec["runs"]:
        name, note = r.get("name", r["key"]), r.get("note", "")
        if "csv" in r:
            run = run_from_csv(os.path.join(root, r["csv"]), r["key"], name, note, vmap)
        elif "synthetic" in r:
            run = run_synthetic(r["key"], name, note, **r["synthetic"])
        else:
            sys.exit(f"{path}: run {r['key']} has neither csv nor synthetic")
        for k in ("select", "window", "presets"):
            if k in r:
                run[k] = r[k]
        runs.append(run)
    doc = {k: spec[k] for k in ("title", "lede", "meta", "coverage", "footnote", "resolution_ns", "boundary") if k in spec}
    return {"format": FORMAT, **doc, "runs": runs}


def build_csvs(paths, a):
    vmap = parse_map(a.map)
    runs = []
    for i, p in enumerate(paths):
        stem = os.path.splitext(os.path.basename(p))[0]
        name = a.name if a.name and len(paths) == 1 else stem
        runs.append(run_from_csv(p, "run%d" % i, name, a.note or "", vmap))
    return {"format": FORMAT, "title": a.title or "VM State Timeline",
            "meta": [{"label": "Source", "text": "rvdbg swtrace · " + ", ".join(os.path.basename(p) for p in paths)}],
            "runs": runs}


def render(doc, template):
    with open(template) as f:
        page = f.read()
    tag = '<script id="timeline-data" type="application/json">null</script>'
    if tag not in page:
        sys.exit(f"{template}: no {tag}")
    data = json.dumps(doc, separators=(",", ":"), ensure_ascii=False).replace("</", "<\\/")
    return page.replace(tag, tag.replace(">null<", ">" + data + "<"))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("inputs", nargs="+", help="one spec .json, or rvdbg swtrace --csv files (one run each)")
    ap.add_argument("-o", "--out", help="self-contained HTML page")
    ap.add_argument("--json", help="also (or only) write the data file")
    ap.add_argument("--template", default=os.path.join(HERE, "..", "..", "crates", "rustvisor-debug", "assets", "timeline.html"),
                    help="the page (default: rvdbg's, the one copy)")
    ap.add_argument("--map", action="append", default=[], help="CORE=VM or GUEST_IMAGE=VM:CORE (default: five-VM PLAN)")
    ap.add_argument("--name"); ap.add_argument("--note"); ap.add_argument("--title")
    a = ap.parse_args()
    if not a.out and not a.json:
        ap.error("give -o and/or --json")
    if len(a.inputs) == 1 and a.inputs[0].endswith(".json"):
        doc = build_spec(a.inputs[0], a.map)
    else:
        doc = build_csvs(a.inputs, a)
    for path, text in ((a.json, lambda: json.dumps(doc, separators=(",", ":"), ensure_ascii=False)),
                       (a.out, lambda: render(doc, a.template))):
        if path:
            os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
            with open(path, "w") as f:
                f.write(text())
    for r in doc["runs"]:
        print("%-8s %d units, %d segments, %d pending, %d messages, %d unpaired, span %.3f ms" % (
            r["key"], len(r["units"]), sum(len(u["segs"]) for u in r["units"]), sum(len(u.get("pending", [])) for u in r["units"]),
            len(r["msgs"]), r["unpaired"], r["span"] / 1e6))


if __name__ == "__main__":
    main()
