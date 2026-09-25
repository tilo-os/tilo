#!/usr/bin/env python3
"""Compile external consumers against exact Cargo-emitted library artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
PROBES = HERE / "consumer_probes"
ENVIRONMENT_CODES = {"E0463", "E0464", "E0514", "E0786"}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def cargo_command(group: str, target: str | None) -> list[str]:
    if group == "service-normal":
        command = [
            "cargo", "build", "-p", "tilo-install-artifacts", "--lib",
            "--offline", "--locked", "--message-format=json-render-diagnostics",
        ]
    elif group == "service-support":
        command = [
            "cargo", "build", "-p", "tilo-install-artifacts", "--lib",
            "--features", "test-support", "--offline", "--locked",
            "--message-format=json-render-diagnostics",
        ]
    else:
        features = "install-artifacts"
        if group == "sdk-support":
            features += ",tilo-install-artifacts/test-support"
        command = [
            "cargo", "build", "-p", "tilo-app", "--lib", "--features", features,
            "--offline", "--locked", "--message-format=json-render-diagnostics",
        ]
    if target is not None:
        command += ["--target", target]
    return command


def compiler_artifacts(raw: str) -> list[dict[str, object]]:
    rows = []
    for line in raw.splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if row.get("reason") == "compiler-artifact":
            rows.append(row)
    return rows


def artifact_row(rows: list[dict[str, object]], target: str) -> dict[str, object]:
    found = [
        row
        for row in rows
        if row["target"]["name"] == target
        and "lib" in row["target"].get("kind", [])
    ]
    if len(found) != 1:
        raise AssertionError(f"expected one Cargo artifact for {target}, found {len(found)}")
    return found[0]


def immutable_pair(row: dict[str, object], target: str) -> tuple[Path, Path]:
    filenames = [Path(value).resolve() for value in row["filenames"]]
    metas = [
        path for path in filenames
        if path.suffix == ".rmeta"
        and path.parent.name == "deps"
        and path.name.startswith(f"lib{target}-")
    ]
    if len(metas) != 1:
        raise AssertionError(f"expected one immutable emitted rmeta for {target}: {metas}")
    rmeta = metas[0]
    rlib = rmeta.with_suffix(".rlib")
    if not rmeta.is_file() or not rlib.is_file():
        raise AssertionError(f"missing matching artifact pair: {rmeta}, {rlib}")
    if rmeta.stem != rlib.stem or "-" not in rmeta.stem:
        raise AssertionError(f"artifact pair is not hash-matched: {rmeta}, {rlib}")
    return rmeta, rlib


def rustc_case(
    *,
    name: str,
    source: Path,
    crate: str,
    rlib: Path,
    serde_rlib: Path,
    cfg: str | None,
    target: str | None,
    output: Path,
) -> tuple[int, set[str], str]:
    command = [
        "rustc", "--edition=2024", "--crate-type=lib",
        "--crate-name", name.replace("-", "_"), "--emit=metadata",
        "--error-format=json", "--extern", f"{crate}={rlib}",
        "--extern", f"serde={serde_rlib}", "-L", f"dependency={rlib.parent}",
    ]
    if cfg is not None:
        command += ["--cfg", cfg]
    if target is not None:
        command += ["--target", target]
    command += [str(source), "-o", str(output / f"{name}.rmeta")]
    result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True)
    (output / f"{name}.command.json").write_text(json.dumps(command, indent=2) + "\n")
    (output / f"{name}.stdout.log").write_text(result.stdout)
    (output / f"{name}.diagnostics.jsonl").write_text(result.stderr)
    diagnostics = []
    for line in result.stderr.splitlines():
        try:
            diagnostic = json.loads(line)
        except json.JSONDecodeError:
            continue
        if diagnostic.get("$message_type") == "diagnostic":
            diagnostics.append(diagnostic)
    codes = {
        diagnostic["code"]["code"]
        for diagnostic in diagnostics
        if diagnostic.get("level") == "error" and diagnostic.get("code")
    }
    return result.returncode, codes, result.stderr


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--group",
        required=True,
        choices=("service-normal", "service-support", "sdk-normal", "sdk-support"),
    )
    parser.add_argument("--target")
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)

    build = cargo_command(args.group, args.target)
    built = subprocess.run(build, cwd=ROOT, text=True, capture_output=True)
    (output / "cargo-command.json").write_text(json.dumps(build, indent=2) + "\n")
    (output / "cargo-artifacts.jsonl").write_text(built.stdout)
    (output / "cargo-stderr.log").write_text(built.stderr)
    if built.returncode != 0:
        raise AssertionError(f"Cargo artifact rebuild failed ({built.returncode}); see {output}")

    rows = compiler_artifacts(built.stdout)
    service_row = artifact_row(rows, "tilo_install_artifacts")
    serde_row = artifact_row(rows, "serde")
    service_features = set(service_row["features"])
    expects_support = args.group.endswith("support")
    if ("test-support" in service_features) != expects_support:
        raise AssertionError(
            f"service feature receipt mismatch for {args.group}: {sorted(service_features)}"
        )
    service_rmeta, service_rlib = immutable_pair(service_row, "tilo_install_artifacts")
    serde_rmeta, serde_rlib = immutable_pair(serde_row, "serde")

    sdk_row = None
    if args.group.startswith("sdk-"):
        sdk_row = artifact_row(rows, "tilo_app")
        sdk_features = set(sdk_row["features"])
        if "install-artifacts" not in sdk_features or "test-support" in sdk_features:
            raise AssertionError(f"SDK feature receipt mismatch: {sorted(sdk_features)}")
        primary_rmeta, primary_rlib = immutable_pair(sdk_row, "tilo_app")
        crate = "tilo_app"
        cfg = "sdk_api"
    else:
        sdk_features = set()
        primary_rmeta, primary_rlib = service_rmeta, service_rlib
        crate = "tilo_install_artifacts"
        cfg = "service_api"

    receipts = {
        "group": args.group,
        "target": args.target,
        "cargo_exit": built.returncode,
        "service_features": sorted(service_features),
        "sdk_features": sorted(sdk_features),
        "primary": {
            "rmeta": str(primary_rmeta.relative_to(ROOT)),
            "rmeta_sha256": sha256(primary_rmeta),
            "rlib": str(primary_rlib.relative_to(ROOT)),
            "rlib_sha256": sha256(primary_rlib),
        },
        "service_dependency": {
            "rmeta": str(service_rmeta.relative_to(ROOT)),
            "rmeta_sha256": sha256(service_rmeta),
            "rlib": str(service_rlib.relative_to(ROOT)),
            "rlib_sha256": sha256(service_rlib),
        },
        "serde_dependency": {
            "rmeta": str(serde_rmeta.relative_to(ROOT)),
            "rmeta_sha256": sha256(serde_rmeta),
            "rlib": str(serde_rlib.relative_to(ROOT)),
            "rlib_sha256": sha256(serde_rlib),
        },
    }
    (output / "feature-and-artifact-receipt.json").write_text(
        json.dumps(receipts, indent=2) + "\n"
    )

    if args.group.startswith("service-"):
        cache_codes = {"E0603"}
        transport_codes = {"E0603"}
        target_is_linux = (
            args.target.startswith(("aarch64-unknown-linux", "x86_64-unknown-linux"))
            if args.target is not None
            else sys.platform.startswith("linux")
        )
        backend_codes = {"E0603"} if target_is_linux else {"E0432", "E0603"}
    else:
        cache_codes = {"E0433"}
        transport_codes = {"E0432"}
        backend_codes = {"E0433"}
    cases: list[tuple[str, set[str] | None, str | None]] = [
        ("positive", None, cfg),
        ("private_cache", cache_codes, cfg),
        ("private_transport", transport_codes, cfg),
        ("forged_selection", {"E0451"}, cfg),
        ("private_field", {"E0616"}, cfg),
        ("no_deserialize", {"E0277"}, cfg),
        ("no_consume", {"E0599"}, cfg),
        ("no_backend", backend_codes, cfg),
    ]
    if args.group == "service-support":
        cases.insert(1, ("support_positive", None, None))
        # The fixture retains a crate-private helper for its own moved unit
        # tests. An external call therefore receives the precise private-method
        # diagnostic E0624; it still obtains neither a transport nor a handle.
        cases.append(("support_no_handles", {"E0624"}, None))
    elif args.group == "service-normal" or args.group.startswith("sdk-"):
        cases.append(("normal_no_test_support", {"E0432"}, cfg))

    results = []
    for index, (name, expected, case_cfg) in enumerate(cases):
        # The positive control is deliberately first; no negative is accepted
        # after a metadata, target, or dependency loading failure.
        assert index != 0 or name == "positive"
        source = PROBES / f"{name}.rs"
        code, codes, stderr = rustc_case(
            name=f"{args.group}-{name}",
            source=source,
            crate=crate if not name.startswith("support_") else "tilo_install_artifacts",
            rlib=primary_rlib if not name.startswith("support_") else service_rlib,
            serde_rlib=serde_rlib,
            cfg=case_cfg,
            target=args.target,
            output=output,
        )
        if codes.intersection(ENVIRONMENT_CODES):
            raise AssertionError(f"{name} hit artifact/environment diagnostics {sorted(codes)}")
        if expected is None:
            if code != 0 or codes:
                raise AssertionError(f"positive control {name} failed: {stderr}")
        elif code == 0 or codes != expected:
            raise AssertionError(
                f"{name} expected exactly {sorted(expected)}, got exit={code}, codes={sorted(codes)}\n{stderr}"
            )
        results.append(
            {
                "name": name,
                "source": str(source.relative_to(ROOT)),
                "source_sha256": sha256(source),
                "expected": None if expected is None else sorted(expected),
                "exit": code,
                "error_codes": sorted(codes),
            }
        )
        print(f"{args.group} {name}: PASS {sorted(codes)}", flush=True)

    (output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
