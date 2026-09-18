#!/usr/bin/env python3
"""Two isolated Cargo workspaces share only an experiment-local sccache store."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess

from probe import run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--slice", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--port", type=int, default=43245)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    sccache = shutil.which("sccache")
    assert sccache, "sccache is required"
    env = {key: value for key, value in os.environ.items() if not key.startswith("SCCACHE_")}
    isolated_config = args.out / "sccache-empty.toml"
    isolated_config.write_text("# Experiment uses only its local disk cache.\n")
    env.update(RUSTUP_TOOLCHAIN="1.97.1", CARGO_INCREMENTAL="0",
               RUSTC_WRAPPER=sccache, SCCACHE_DIR=str(args.out / "cache"),
               SCCACHE_SERVER_PORT=str(args.port), SCCACHE_IDLE_TIMEOUT="60",
               SCCACHE_CACHE_SIZE="128M", SCCACHE_CONF=str(isolated_config))
    env.pop("CARGO_TARGET_DIR", None)
    rows = {}
    started = False
    try:
        run([sccache, "--start-server"], args.out, args.out / "server.log", env)
        started = True
        for worker in ["worker-a", "worker-b", "worker-a-repeat"]:
            path = args.out / ("worker-a" if worker == "worker-a-repeat" else worker)
            if worker == "worker-a-repeat":
                # Empty Cargo outputs at the identical checkout path, without
                # setting CARGO_TARGET_DIR or sharing live target directories.
                (path / "target").rename(path / "target-first-build")
            else:
                path.mkdir()
                for name in ["calc", "order", "atomic_file", "app_a", "app_b"]:
                    shutil.copytree(args.slice / name, path / name)
                for name in ["Cargo.toml", "Cargo.lock"]:
                    shutil.copyfile(args.slice / name, path / name)
            run([sccache, "--zero-stats"], path, args.out / f"{worker}-zero.log", env)
            rows[worker] = run(["cargo", "build", "--release", "--locked", "-j2", "-vv", "-p", "app_a", "-p", "app_b"],
                               path, args.out / f"{worker}-build.log", env)
            rows[worker]["stats"] = json.loads(subprocess.check_output([sccache, "--show-stats", "--stats-format=json"], env=env))
            for name in ["app_a", "app_b"]:
                run([str(path / "target/release" / name)], path, args.out / f"{worker}-{name}.log", env)
        (args.out / "results.json").write_text(json.dumps(rows, indent=2))
        print(json.dumps(rows, indent=2))
    finally:
        if started:
            run([sccache, "--stop-server"], args.out, args.out / "stop.log", env, check=False)


if __name__ == "__main__":
    main()
