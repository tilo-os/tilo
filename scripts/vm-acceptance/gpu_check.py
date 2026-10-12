#!/usr/bin/env python3
"""Judge which GPU an installed image's app windows render on, for gates outside run.py.

    ./os/dev/vm.sh ssh "$(python3 scripts/vm-acceptance/gpu_check.py --probe)" \\
        | python3 scripts/vm-acceptance/gpu_check.py

`--probe` prints the guest script (run it as the session's user once apps have opened
windows); without it the script's output is read from stdin and judged as R05 judges it.
Exit 0 when every window renders on the GPU, or when the machine has no GPU to render on
(a software-only VM, reported as a notice); 1 otherwise.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import gpu  # noqa: E402


def main(argv: list[str]) -> int:
    if argv[1:] == ["--probe"]:
        print(gpu.PROBE)
        return 0
    if argv[1:]:
        print(__doc__, file=sys.stderr)
        return 2
    health_text, log = gpu.split_probe(sys.stdin.read())
    health = gpu.parse_health(health_text)
    adapters = gpu.parse_adapters(log)
    for app, lines in gpu.summary(adapters).items():
        for line in lines:
            print(f"{app}: {line}")
    verdict, why = gpu.judge(health, adapters, gpu.parse_fallbacks(log), health_text)
    if verdict == "pass":
        print(f"GPU adapters: {why}")
        return 0
    if verdict == "unobservable" and (health or {}).get("status") == "no-gpu":
        print(f"::notice::GPU adapters not judged: {why}")
        return 0
    print(f"::error::GPU adapters: {why}")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
