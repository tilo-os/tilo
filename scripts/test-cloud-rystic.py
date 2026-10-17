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


def run_provider(provider, contracts, volume=False, changes_only=False):
    package, target, product, variable = PROVIDERS[provider]
    if provider == "gdrive":
        subprocess.run(["rystic", "update", "-no-self", product], check=True, timeout=120)
    # Build every selected target before starting the metered simulator.
    executables = [compile_test(package, target, contracts or changes_only)]
    if volume and provider == "gdrive":
        executables.append(compile_test("tilo-fs", "gdrive-rystic-volume", False))
    test_filter = ["change"] if changes_only else []
    for executable in executables:
        listed = subprocess.run(
            [str(executable), *test_filter, "--list"], cwd=ROOT,
            text=True, stdout=subprocess.PIPE, check=True, timeout=30,
        )
        count = sum(line.endswith(": test") for line in listed.stdout.splitlines())
        if count == 0 or (changes_only and count != 2):
            raise RuntimeError(f"unexpected selected test count for {executable.name}: {count}")
    name = f"tilo-ci-{provider}-{uuid.uuid4().hex}"
    try:
        result = subprocess.run(
            ["rystic", "run", product, "--name", name, "--port", "0", "-d", "-json"],
            text=True, stdout=subprocess.PIPE, check=True, timeout=60,
        )
        url = json.loads(result.stdout)["data"]["url"]
        env = dict(os.environ, **{variable: url})
        failed = False
        for executable in executables:
            try:
                result = subprocess.run(
                    [str(executable), "--nocapture", "--test-threads=1", *test_filter],
                    cwd=ROOT, env=env, timeout=240,
                )
                failed |= result.returncode != 0
            except subprocess.TimeoutExpired:
                print(f"FAIL: {executable.name} exceeded 240-second paid-run bound", flush=True)
                failed = True
        return int(failed)
    finally:
        # Only touch the unique instance owned by this invocation.
        try:
            stopped = subprocess.run(["rystic", "stop", name], check=False, timeout=30)
        finally:
            removed = subprocess.run(["rystic", "rm", name], check=False, timeout=30)
        if stopped.returncode or removed.returncode:
            raise RuntimeError(f"failed to clean up Rystic instance {name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("provider", choices=["all", *PROVIDERS], default="all", nargs="?")
    parser.add_argument("--contracts", action="store_true",
                        help="also run strict vendor contracts; unsupported operations fail")
    parser.add_argument("--volume", action="store_true",
                        help="also test the real Google Drive CloudVolume (requires upload support)")
    parser.add_argument("--changes-only", action="store_true",
                        help="run only Drive initial/change-cursor contracts, independently of DOCX export")
    args = parser.parse_args()
    if args.changes_only and (args.provider != "gdrive" or args.volume):
        parser.error("--changes-only requires gdrive without --volume")
    providers = PROVIDERS if args.provider == "all" else [args.provider]
    failed = False
    for provider in providers:
        failed |= run_provider(provider, args.contracts, args.volume, args.changes_only) != 0
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
