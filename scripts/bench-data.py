#!/usr/bin/env python3
"""Write the benchmark folder: real text (the Cargo registry's sources),
incompressible blocks and mostly-zero files, about 200 MB in some 8000
files, like the folder the README's numbers were first taken on.

    python3 scripts/bench-data.py OUT
"""
import os
import random
import shutil
import sys
from pathlib import Path

TEXT_BYTES = 100 << 20
RANDOM_BYTES = 65 << 20
ZERO_BYTES = 35 << 20


def main() -> None:
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    registry = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry" / "src"
    total = n = 0
    for path in sorted(registry.rglob("*")):
        if total >= TEXT_BYTES:
            break
        if path.suffix not in {".rs", ".md", ".toml", ".txt", ".c", ".h", ".cpp"} or not path.is_file():
            continue
        size = path.stat().st_size
        if not 0 < size < 1 << 20:
            continue
        dest = out / "text" / f"{n // 500:03}" / f"{n:05}{path.suffix}"
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, dest)
        total += size
        n += 1
    rng = random.Random(1)
    files = n
    for kind, budget in (("random", RANDOM_BYTES), ("zeros", ZERO_BYTES)):
        made = i = 0
        while made < budget:
            size = rng.choice((4 << 10, 16 << 10, 64 << 10, 512 << 10))
            if kind == "random":
                body = rng.randbytes(size)
            else:
                body = bytearray(size)
                for _ in range(size // 4096):
                    body[rng.randrange(size)] = rng.randrange(256)
            dest = out / kind / f"{i // 500:03}" / f"{i:05}.bin"
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(body)
            made += size
            i += 1
        files += i
    print(f"{files} files, {total >> 20} MiB of text, in {out}")


if __name__ == "__main__":
    main()
