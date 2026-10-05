#!/usr/bin/env python3
"""Print one line per channel-spike result (JSON lines from files or stdin), RTTs in ms."""

import fileinput
import json

for line in fileinput.input():
    line = line.strip()
    if not line.startswith("{"):
        continue
    d = json.loads(line)
    if "error" in d:
        print("ERROR", d["error"])
        continue
    r = d["rtt_us"]
    ms = {k: v / 1000 for k, v in r.items()}
    print(
        f"{d['label']:<22} {d['transport']:<5} {d['bulk']:<9} "
        f"rate={d.get('bulk_rate_mbps', 0):>4} cc={d.get('cc', '-'):<5} "
        f"recv={d['received']:>5}/{d['sent']:<5} "
        f"p50={ms['p50']:>7.2f} p95={ms['p95']:>7.2f} p99={ms['p99']:>7.2f} "
        f"p999={ms['p999']:>7.2f} max={ms['max']:>7.2f} ms  bulk={d['bulk_mbps']:>5} Mbps"
    )
