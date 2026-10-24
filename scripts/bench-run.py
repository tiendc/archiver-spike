#!/usr/bin/env python3
"""Time the ways of reading each archive in turn, three rounds, and print
one table (MB/s of unpacked data; the range over the rounds).

    python3 scripts/bench-run.py UNPACKED_BYTES ARCHIVE... -- NAME=COMMAND...

COMMAND is a shell-free command line with {a} for the archive and {out} for
a fresh empty folder; when it prints "= N MB/s" (rar-perf does) that is
taken, otherwise the wall time is.
"""
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def main() -> None:
    args = sys.argv[1:]
    split = args.index("--")
    unpacked = int(args[0])
    archives = [Path(a) for a in args[1:split]]
    ways = [w.split("=", 1) for w in args[split + 1:]]
    rates: dict[tuple[str, str], list[float]] = {}
    for archive in archives:
        for _ in range(3):
            for name, command in ways:
                out = Path(tempfile.mkdtemp(prefix="bench-out-"))
                cmd = [part.replace("{a}", str(archive)).replace("{out}", str(out)) for part in command.split()]
                start = time.perf_counter()
                done = subprocess.run(cmd, capture_output=True, text=True, errors="replace")
                seconds = time.perf_counter() - start
                shutil.rmtree(out, ignore_errors=True)
                if done.returncode:
                    print(f"{name} on {archive.name} failed: {(done.stderr or done.stdout).strip()[:200]}", flush=True)
                    continue
                found = re.search(r"= (\d+) MB/s", done.stdout)
                rate = float(found.group(1)) if found else unpacked / seconds / 1e6
                rates.setdefault((archive.name, name), []).append(rate)
    names = [name for name, _ in ways]
    lines = ["| archive | " + " | ".join(names) + " |", "|---|" + "---|" * len(names)]
    for archive in archives:
        cells = []
        for name in names:
            got = rates.get((archive.name, name))
            cells.append(f"{min(got):.0f}–{max(got):.0f}" if got else "failed")
        lines.append(f"| {archive.name} | " + " | ".join(cells) + " |")
    table = "\n".join(lines)
    print(table)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(f"\n{os.environ.get('BENCH_TITLE', '')}\n\n{table}\n")


if __name__ == "__main__":
    main()
