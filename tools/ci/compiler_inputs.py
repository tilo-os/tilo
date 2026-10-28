#!/usr/bin/env python3
"""Observe Cargo inputs during an existing check; never change CI selection.

Receipts describe only the executed target/configuration, not other features or
architectures. Missing dep-info/build-script declarations remain explicit unknowns.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def dep_inputs(text: str) -> list[str]:
    """Read rustc's Make-style dependency rules, including escaped spaces/#/$.

    Native CI uses Unix paths. Reject unsupported syntax instead of guessing at
    a Windows drive separator or a malformed escape. Ignore phony target rules.
    """
    result = set()
    for line in text.replace("\\\n", "").splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        if ": " not in line and not line.endswith(":"):
            raise ValueError("invalid dep-info rule")
        rhs = line.split(":", 1)[1]
        word = ""
        escaped = False
        for char in rhs + " ":
            if escaped:
                word += char
                escaped = False
            elif char == "\\":
                escaped = True
            elif char.isspace():
                if word:
                    result.add(word.replace("$$", "$"))
                    word = ""
            elif char == ":":
                raise ValueError("unsupported dep-info path")
            else:
                word += char
        if escaped or word:
            raise ValueError("truncated dep-info escape")
    return sorted(result)


def input_record(path: Path, root: Path) -> dict:
    resolved = path.resolve()
    try:
        relative = resolved.relative_to(root.resolve()).as_posix()
    except ValueError:
        # No runner-local absolute paths or environment values in uploaded data.
        return {"scope": "external", "status": "unknown"}
    if not resolved.is_file():
        return {"path": relative, "status": "missing-or-directory"}
    return {"path": relative, "sha256": hashlib.sha256(resolved.read_bytes()).hexdigest()}


def build_inputs(output: str, package_dir: Path, root: Path) -> dict:
    paths, env_names = [], []
    for line in output.splitlines():
        if line.startswith(("cargo:rerun-if-changed=", "cargo::rerun-if-changed=")):
            paths.append(input_record(package_dir / line.split("=", 1)[1], root))
        elif line.startswith(("cargo:rerun-if-env-changed=", "cargo::rerun-if-env-changed=")):
            env_names.append(line.split("=", 1)[1])
    return {"inputs": paths, "environment_names": sorted(set(env_names)),
            "unknown": ([] if paths else ["no explicit file declarations; Cargo package scan applies"])
                       + (["environment values not recorded"] if env_names else [])}


def collect(messages: list[dict], metadata: dict, root: Path, binding: dict) -> dict:
    members = set(metadata["workspace_members"])
    packages = {p["id"]: p for p in metadata["packages"] if p["id"] in members}
    targets, scripts, unknown = [], [], []
    for message in messages:
        package = packages.get(message.get("package_id"))
        if not package:
            continue
        if message.get("reason") == "compiler-artifact":
            target = message["target"]
            row = {"package": package["name"], "target": target["name"],
                   "kind": target["kind"], "profile": message["profile"],
                   "features": sorted(message["features"]),
                   "source": input_record(Path(target["src_path"]), root),
                   "inputs": [], "unknown": []}
            candidates = set()
            for filename in message["filenames"]:
                artifact = Path(filename)
                stem = artifact.stem
                if stem.startswith("lib") and artifact.suffix in {".rmeta", ".rlib", ".so", ".dylib"}:
                    stem = stem[3:]
                candidates.add(artifact.with_name(stem + ".d"))
            found = [p for p in candidates if p.is_file()]
            if not found:
                row["unknown"].append("no matching rustc dep-info")
            for depfile in sorted(found):
                try:
                    text = depfile.read_text()
                    row["inputs"].extend(input_record(root / p, root) for p in dep_inputs(text))
                    if "# env-dep:" in text:
                        row["unknown"].append("rustc environment dependency values not recorded")
                except (OSError, UnicodeError, ValueError):
                    row["unknown"].append("unreadable or unsupported dep-info")
            if any("sha256" not in item for item in row["inputs"]):
                row["unknown"].append("inputs outside workspace, missing, or directories")
            targets.append(row)
        elif message.get("reason") == "build-script-executed":
            output = Path(message["out_dir"]).parent / "output"
            row = {"package": package["name"]}
            try:
                row.update(build_inputs(output.read_text(), Path(package["manifest_path"]).parent, root))
            except (OSError, UnicodeError):
                row.update(inputs=[], unknown=["build-script output unavailable"])
            scripts.append(row)
    if not any(m.get("reason") == "build-finished" and m.get("success") is True for m in messages):
        unknown.append("successful build-finished message missing")
    if not targets:
        unknown.append("no workspace target artifacts observed")
    if binding.get("dirty"):
        unknown.append("tracked worktree differs from bound commit")
    return {"schema": 1, "purpose": "observation-only", "binding": binding,
            "coverage": "executed Cargo targets/configuration only; not selection authority",
            "completeness": "partial: external Cargo config, other targets/features and external inputs not qualified",
            "targets": targets, "build_scripts": scripts, "unknown": unknown}


def command(*args: str) -> str:
    return subprocess.check_output(args, text=True).strip()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--from-messages", type=Path, help="observe existing Clippy JSON without invoking Clippy")
    parser.add_argument("--cargo-exit-code", type=int)
    parser.add_argument("--expected-commit", help="source revision captured before the existing invocation")
    parser.add_argument("cargo_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    cargo_args = args.cargo_args
    if cargo_args[:1] == ["--"]:
        cargo_args = cargo_args[1:]
    observing = args.from_messages is not None
    if not cargo_args or cargo_args[0] != ("clippy" if observing else "check"):
        parser.error("expected existing check arguments, or clippy arguments with --from-messages")
    if observing and (args.cargo_exit_code is None or not args.expected_commit):
        parser.error("existing messages require --cargo-exit-code and --expected-commit")
    root = Path.cwd()
    try:
        binding = {"commit": command("git", "rev-parse", "HEAD"),
                   "tree": command("git", "rev-parse", "HEAD^{tree}"),
                   "dirty": bool(command("git", "status", "--porcelain", "--untracked-files=no")),
                   "rustc": command("rustc", "-Vv"), "cargo_args": cargo_args,
                   "configuration": {name: hashlib.sha256(os.environ[name].encode()).hexdigest()
                                     for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
                                                  "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
                                                  "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS", "RUSTC_WRAPPER",
                                                  "CARGO_BUILD_TARGET", "CARGO_PROFILE_DEV_DEBUG") if name in os.environ},
                   "config_files": [input_record(root / name, root) for name in
                                    ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml", ".cargo/config.toml")]}
        metadata = json.loads(command("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"))
    except (OSError, ValueError, subprocess.CalledProcessError):
        if observing:
            print("::warning::Clippy input binding unavailable; existing compilation result unchanged.")
            return 0
        print("::warning::Compiler input binding unavailable; running unchanged Cargo check.")
        return subprocess.call(["cargo", *cargo_args])
    messages = []
    if observing:
        try:
            with args.from_messages.open(encoding="utf-8") as stream:
                for line in stream:
                    try:
                        message = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if isinstance(message, dict) and message.get("reason") in {
                        "compiler-artifact", "build-script-executed", "build-finished"
                    }:
                        messages.append(message)
        except (OSError, UnicodeError):
            print("::warning::Clippy messages unavailable; existing compilation result unchanged.")
            return 0
        status = args.cargo_exit_code
    else:
        proc = subprocess.Popen(["cargo", *cargo_args, "--message-format=json-render-diagnostics"],
                                stdout=subprocess.PIPE, text=True)
        assert proc.stdout is not None
        for line in proc.stdout:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                print(line, end="", flush=True)
                continue
            if not isinstance(message, dict):
                continue
            if message.get("reason") in {"compiler-artifact", "build-script-executed", "build-finished"}:
                messages.append(message)
            if message.get("reason") == "compiler-message":
                diagnostic = message.get("message")
                rendered = diagnostic.get("rendered") if isinstance(diagnostic, dict) else None
                if isinstance(rendered, str):
                    print(rendered, end="", flush=True)
                else:
                    print("::warning::Compiler diagnostic unavailable; compilation result unchanged.")
        status = proc.wait()
    try:
        receipt = collect(messages, metadata, root, binding)
        receipt["cargo_exit_code"] = status
        if observing:
            receipt["binding"]["expected_commit"] = args.expected_commit
            if binding["commit"] != args.expected_commit:
                receipt["unknown"].append("source revision differs from invocation start")
        if status != 0:
            receipt["unknown"].append("Cargo invocation failed; target coverage incomplete")
        if command("git", "rev-parse", "HEAD") != binding["commit"] or command("git", "status", "--porcelain", "--untracked-files=no"):
            receipt["unknown"].append("source binding changed or dirty after check")
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(receipt, separators=(",", ":")) + "\n")
        print(f"Compiler inputs: {len(receipt['targets'])} target observations; selection unchanged.")
    except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.CalledProcessError) as error:
        # Observability cannot turn a successful compile red or hide its failure.
        print(f"::warning::Compiler input receipt unavailable ({type(error).__name__}).")
    return 0 if observing else status


if __name__ == "__main__":
    sys.exit(main())
