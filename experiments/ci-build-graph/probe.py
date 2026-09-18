#!/usr/bin/env python3
"""Bounded Linux experiment. Copies sources; never edits the product checkout.

Usage: python3 probe.py --root /path/to/qos --out /fresh/experiment --bazel /path/to/bazel
Requires the pinned Rust toolchain, gcc, Python and Bazel. Network used by Bazel.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import time


def run(command, cwd, log, env=None, check=True):
    start = time.monotonic()
    result = subprocess.run(command, cwd=cwd, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=600)
    log.write_text(result.stdout)
    row = {"seconds": round(time.monotonic() - start, 3), "exit": result.returncode,
           "log": log.name}
    if check and result.returncode:
        raise RuntimeError(f"Failed: {command}\n{result.stdout[-6000:]}")
    return row


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--bazel", type=Path, required=True)
    parser.add_argument("--revision", help="Verified host git revision for Windows worktrees read from WSL")
    args = parser.parse_args()
    root, out = args.root.resolve(), args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    logs = out / "logs"
    logs.mkdir()
    sys.path.insert(0, str(root / "tools"))
    import app_release
    import ci_select
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"], cwd=root))
    catalog = app_release.app_catalog(root)
    scenarios = {
        "calculator": ["apps/calculator/src/engine.rs"],
        "toolkit": ["ui/src/lib.rs"],
        "lockfile": ["Cargo.lock"],
        "packaging": ["dist/apps/package-apps.sh"],
        "docs": ["README.md"],
        "browser": ["apps/browser/src/main.rs"],
        "atomic_file": ["crates/system/atomic-file/src/lib.rs"],
    }
    results = {"revision": args.revision or subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
               "scope": "Linux ARM64 sliced production modules; synthetic application topology, not GUI apps",
               "planner": {}, "builds": {}, "sources": {}}
    for name, paths in scenarios.items():
        plan = ci_select.make_plan(metadata, root, paths, 4)
        apps = app_release.apps_from_changes(paths, catalog, metadata, root)
        results["planner"][name] = {"apps": apps, "app_count": len(apps),
                                     "checks": plan["selected_count"], "full": plan["full"]}
    results["workspace_packages"] = len(metadata["packages"])
    results["catalog_apps"] = len(catalog)
    result_path = out / "results.json"
    result_path.write_text(json.dumps(results, indent=2))

    work = out / "slice"
    work.mkdir()
    modules = {"calc": "apps/calculator/src/engine.rs",
               "order": "crates/doc/qdoc/src/order.rs",
               "atomic_file": "crates/system/atomic-file/src/lib.rs"}
    for name, source in modules.items():
        data = (root / source).read_bytes()
        results["sources"][source] = hashlib.sha256(data).hexdigest()
        path = work / name / "src/lib.rs"
        path.parent.mkdir(parents=True)
        path.write_bytes(data)
        write(work / name / "Cargo.toml", f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2024"\n')
    for name, lib, body in [
        ("app_a", "calc", 'assert_eq!(calc::evaluate("2+3", &calc::Ctx::default()).unwrap(), 5.0);'),
        ("app_b", "order", 'assert_eq!(order::cmp_keys(b"a", b"b"), std::cmp::Ordering::Less);'),
    ]:
        write(work / name / "Cargo.toml", f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2024"\n[dependencies]\n{lib} = {{ path = "../{lib}" }}\natomic_file = {{ path = "../atomic_file" }}\n')
        write(work / name / "src/main.rs", 'fn main() { ' + body + '\nlet path = std::env::temp_dir().join(format!("qos-ci-probe-{}", std::process::id()));\natomic_file::write(&path, b"probe").unwrap();\nassert_eq!(std::fs::read(&path).unwrap(), b"probe"); std::fs::remove_file(path).unwrap();\n}\n')
    write(work / "Cargo.toml", '[workspace]\nresolver = "3"\nmembers = ["calc", "order", "atomic_file", "app_a", "app_b"]\n[profile.release]\nopt-level = 2\ndebug = 0\nlto = false\ncodegen-units = 16\n')
    write(work / "MODULE.bazel", 'module(name = "qos_ci_slice")\nbazel_dep(name = "rules_rust", version = "0.74.0")\nrust = use_extension("@rules_rust//rust:extensions.bzl", "rust")\nrust.toolchain(edition = "2024", versions = ["1.97.1"])\n')
    build = ['load("@rules_rust//rust:defs.bzl", "rust_library", "rust_binary", "rust_test")']
    for name in modules:
        build.append(f'rust_library(name = "{name}", srcs = ["{name}/src/lib.rs"], crate_root = "{name}/src/lib.rs", edition = "2024")')
    build.append('rust_test(name = "calculator_tests", crate = ":calc")')
    for name, lib in [("app_a", "calc"), ("app_b", "order")]:
        build.append(f'rust_binary(name = "{name}", srcs = ["{name}/src/main.rs"], crate_root = "{name}/src/main.rs", edition = "2024", deps = [":{lib}", ":atomic_file"])')
        # Deliberately simple tar fixture: proves packaging dependency edges, not release hermeticity.
        build.append(f'genrule(name = "{name}_package", srcs = [":{name}", "package.txt"], outs = ["{name}.tar"], cmd = "tar --dereference --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -cf $@ $(locations :{name}) $(location package.txt)")')
    write(work / "BUILD.bazel", "\n".join(build) + "\n")
    write(work / "package.txt", "alpha package fixture v1\n")
    env = dict(os.environ, RUSTUP_TOOLCHAIN="1.97.1", CARGO_INCREMENTAL="0")
    env.pop("CARGO_TARGET_DIR", None)
    env.pop("RUSTC_WRAPPER", None)
    bazel = [str(args.bazel), "--ignore_all_rc_files", f"--output_user_root={out / 'bazel-user'}"]
    flags = ["--jobs=2", "--local_resources=memory=2048", "--color=no", "--curses=no",
             "--noshow_progress", "--remote_cache=", "--remote_executor=", "--experimental_remote_downloader=",
             f"--disk_cache={out / 'action-cache'}", "-c", "opt"]
    results["bazel_isolation"] = {"ignore_all_rc_files": True, "remote_cache": "",
                                  "remote_executor": "", "experimental_remote_downloader": ""}
    targets = ["//:app_a_package", "//:app_b_package"]
    run(["cargo", "generate-lockfile", "--offline"], work, logs / "fixture-lock.log", env)

    def build_case(case, extra=None, other_base=None):
        rows = {}
        cmd = bazel + ([f"--output_base={out / other_base}"] if other_base else [])
        rows["bazel"] = run(cmd + ["build"] + flags + (extra or []) + targets +
                            [f"--execution_log_json_file={logs / (case + '-actions.json')}"],
                            work, logs / (case + "-bazel.log"))
        if not other_base and not extra:
            rows["cargo"] = run(["cargo", "build", "--release", "--locked", "-j2", "-vv", "-p", "app_a", "-p", "app_b"],
                                 work, logs / (case + "-cargo.log"), env)
        # A fresh action cache hit materializes top-level archives only. Validate
        # their contents, not an intermediate binary that may remain in the cache.
        for executable in ["app_a", "app_b"]:
            unpack = out / "unpacked" / case / executable
            unpack.mkdir(parents=True)
            with tarfile.open(work / "bazel-bin" / f"{executable}.tar") as archive:
                archive.extractall(unpack, filter="data")
            binary = next(unpack.rglob(executable))
            run([str(binary)], work, logs / f"{case}-{executable}-run.log")
        results["builds"][case] = rows
        result_path.write_text(json.dumps(results, indent=2))
        print(case, json.dumps(rows), flush=True)

    try:
        build_case("cold")
        build_case("warm")
        with (work / "calc/src/lib.rs").open("a") as f:
            f.write('\npub fn experiment_revision() -> u32 { 2 }\n')
        build_case("app_change")
        with (work / "atomic_file/src/lib.rs").open("a") as f:
            f.write('\npub fn experiment_revision() -> u32 { 2 }\n')
        build_case("shared_change")
        write(work / "package.txt", "alpha package fixture v2\n")
        build_case("package_change")
        write(work / "README.md", "Unrelated documentation change\n")
        build_case("docs_change")
        build_case("fresh_output_base", other_base="fresh-output")
        build_case("compiler_flag_change", extra=["--@rules_rust//rust/settings:extra_rustc_flags=--cfg=experiment"])
        results["calculator_tests_bazel"] = run(bazel + ["test"] + flags + ["//:calculator_tests"], work, logs / "tests-bazel.log")
        results["calculator_tests_cargo"] = run(["cargo", "test", "--locked", "-p", "calc", "-j2"], work, logs / "tests-cargo.log", env)
        result_path.write_text(json.dumps(results, indent=2))
    finally:
        for base in [None, "fresh-output"]:
            run(bazel + ([f"--output_base={out / base}"] if base else []) + ["shutdown"], work, logs / f"shutdown-{base}.log", check=False)
    print(result_path)


if __name__ == "__main__":
    main()
