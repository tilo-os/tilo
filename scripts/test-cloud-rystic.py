#!/usr/bin/env python3
"""Compile with normal Cargo routing, then test against short-lived local twins."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import uuid

ROOT = Path(__file__).resolve().parents[1]
PROVIDERS = {
    "dropbox": ("cloud-source-dropbox", "rystic", "dropbox-twin", "TILO_RYSTIC_DROPBOX_URL"),
    "gdrive": ("cloud-source-gdrive", "gdrive-rystic", "google-drive-twin", "TILO_RYSTIC_GDRIVE_URL"),
}


def compile_test(package, target, contracts):
    feature = "rystic-contract-tests" if contracts else "rystic-tests"
    result = subprocess.run(
        ["cargo", "test", "--locked", "-p", package, "--features", feature,
         "--test", target, "--no-run", "--message-format=json"],
        cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
    )
    executables = set()
    for line in result.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (message.get("reason") == "compiler-artifact"
                and message.get("target", {}).get("name") == target
                and message.get("executable")):
            executables.add(message["executable"])
    if len(executables) != 1:
        raise RuntimeError(f"expected one current test executable, got {executables}")
    executable = Path(executables.pop())
    if not executable.is_absolute():
        executable = ROOT / executable
    if not executable.is_file():
        raise RuntimeError(f"Cargo did not fetch test executable: {executable}")
    return executable


def run_provider(provider, contracts):
    package, target, product, variable = PROVIDERS[provider]
    # Build before starting the metered simulator; never pay for compile time.
    executable = compile_test(package, target, contracts)
    name = f"tilo-ci-{provider}-{uuid.uuid4().hex}"
    try:
        result = subprocess.run(
            ["rystic", "run", product, "--name", name, "--port", "0", "-d", "-json"],
            text=True, stdout=subprocess.PIPE, check=True,
        )
        url = json.loads(result.stdout)["data"]["url"]
        env = dict(os.environ, **{variable: url})
        return subprocess.run(
            [str(executable), "--nocapture", "--test-threads=1"], cwd=ROOT, env=env,
        ).returncode
    finally:
        # Only touch the unique instance owned by this invocation.
        stopped = subprocess.run(["rystic", "stop", name], check=False)
        removed = subprocess.run(["rystic", "rm", name], check=False)
        if stopped.returncode or removed.returncode:
            raise RuntimeError(f"failed to clean up Rystic instance {name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("provider", choices=["all", *PROVIDERS], default="all", nargs="?")
    parser.add_argument("--contracts", action="store_true",
                        help="also run strict vendor contracts (known to fail on current twins)")
    args = parser.parse_args()
    providers = PROVIDERS if args.provider == "all" else [args.provider]
    failed = False
    for provider in providers:
        failed |= run_provider(provider, args.contracts) != 0
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
