#!/usr/bin/env python3
"""Verify every host-package builder compiles every executable dist/stage-payload.sh requires.

The required executable inventory lives in one place: dist/stage-payload.sh's `bins`
list (with and without `--primary-files`). Each of the four distribution builders
(Debian via dist/build-binaries.sh, Arch via dist/arch/PKGBUILD, RPM via
dist/rpm/build-rpm.sh, and Nix via dist/nix/package.nix) must build every executable
that its invocation of dist/stage-payload.sh requires, resolved against the real
workspace Cargo manifests rather than hand-written copies of binary names.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[3]
STAGE_SENTINEL_EXIT = 47


def load_workspace_packages(root: Path) -> dict[str, dict]:
    """Read every workspace member's Cargo.toml: features and binary targets."""
    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
    excluded = {(root / entry).resolve() for entry in workspace.get("exclude", ())}
    packages: dict[str, dict] = {}
    for pattern in workspace["members"]:
        for directory in sorted(root.glob(pattern)):
            resolved = directory.resolve()
            if resolved in excluded or not (directory / "Cargo.toml").is_file():
                continue
            manifest = tomllib.loads((directory / "Cargo.toml").read_text(encoding="utf-8"))
            package = manifest.get("package", {})
            name = package.get("name")
            if not name:
                continue
            bins: dict[str, list[str]] = {}
            explicit_paths: set[Path] = set()
            for target in manifest.get("bin", ()):
                bins[target["name"]] = list(target.get("required-features", ()))
                if "path" in target:
                    explicit_paths.add((directory / target["path"]).resolve())
            if package.get("autobins", True):
                main_rs = directory / "src/main.rs"
                if main_rs.is_file() and name not in bins and main_rs.resolve() not in explicit_paths:
                    bins[name] = []
                bin_dir = directory / "src/bin"
                if bin_dir.is_dir():
                    for source in sorted(bin_dir.glob("*.rs")):
                        if source.stem not in bins and source.resolve() not in explicit_paths:
                            bins[source.stem] = []
                    for source in sorted(bin_dir.glob("*/main.rs")):
                        bin_name = source.parent.name
                        if bin_name not in bins and source.resolve() not in explicit_paths:
                            bins[bin_name] = []
            features = {
                key: list(values)
                for key, values in manifest.get("features", {}).items()
            }
            packages[name] = {"bins": bins, "features": features}
    return packages


def _consume_opt(argv: list[str], index: int, long_name: str, short_name: str | None = None) -> tuple[str | None, int]:
    """Consume a single-value CLI option (`--opt val`, `--opt=val`, `-o val`, `-o=val`)."""
    token = argv[index]
    if token == long_name or (short_name is not None and token == short_name):
        if index + 1 >= len(argv):
            raise AssertionError(f"missing argument for {token}")
        return argv[index + 1], index + 2
    if token.startswith(f"{long_name}="):
        return token.split("=", 1)[1], index + 1
    if short_name is not None and token.startswith(f"{short_name}="):
        return token.split("=", 1)[1], index + 1
    return None, index


def built_bins_for_cargo_args(argv: list[str], packages: dict[str, dict]) -> set[str]:
    """Resolve the exact binary names a `cargo build ...` invocation compiles."""
    if not argv or argv[0] != "build":
        return set()
    selected_packages: list[str] = []
    explicit_bins: list[str] = []
    feature_tokens: list[str] = []
    has_lib = False
    has_bins = False
    has_all_targets = False
    has_other_filter = False
    no_default_features = False
    all_features = False
    workspace_all = False

    skip_value_opts = (
        ("--profile", None),
        ("--target", None),
        ("--target-dir", None),
        ("--manifest-path", None),
        ("--jobs", "-j"),
        ("--color", None),
        ("--message-format", None),
        ("--config", None),
        ("--artifact-dir", None),
        ("--timings", None),
    )
    other_target_value_opts = ("--example", "--test", "--bench")
    other_target_flags = {"--examples", "--tests", "--benches"}

    i = 1
    while i < len(argv):
        token = argv[i]
        if token == "--":
            break
        val, nxt = _consume_opt(argv, i, "--package", "-p")
        if val is not None:
            selected_packages.append(val)
            i = nxt
            continue
        val, nxt = _consume_opt(argv, i, "--bin")
        if val is not None:
            explicit_bins.append(val)
            i = nxt
            continue
        val, nxt = _consume_opt(argv, i, "--features", "-F")
        if val is not None:
            feature_tokens.extend(t for t in re.split(r"[,\s]+", val.strip()) if t)
            i = nxt
            continue
        matched_other = False
        for opt in other_target_value_opts:
            val, nxt = _consume_opt(argv, i, opt)
            if val is not None:
                has_other_filter = True
                i = nxt
                matched_other = True
                break
        if matched_other:
            continue
        matched_skip = False
        for long_opt, short_opt in skip_value_opts:
            val, nxt = _consume_opt(argv, i, long_opt, short_opt)
            if val is not None:
                i = nxt
                matched_skip = True
                break
        if matched_skip:
            continue
        if token == "--lib":
            has_lib = True
        elif token == "--bins":
            has_bins = True
        elif token == "--all-targets":
            has_all_targets = True
        elif token in other_target_flags:
            has_other_filter = True
        elif token == "--no-default-features":
            no_default_features = True
        elif token == "--all-features":
            all_features = True
        elif token in ("--workspace", "--all"):
            workspace_all = True
        i += 1

    selected = set(packages) if workspace_all else set(selected_packages)
    if not selected:
        raise AssertionError(f"cargo build invocation selected no packages: {argv}")
    unknown = sorted(selected - set(packages))
    if unknown:
        raise AssertionError(f"cargo build invocation named unknown packages: {unknown}")

    active_by_pkg: dict[str, set[str]] = {}
    for pkg in selected:
        pkg_features = packages[pkg]["features"]
        if all_features:
            active = set(pkg_features)
        else:
            active = set() if no_default_features else set(pkg_features.get("default", ()))
            for tok in feature_tokens:
                if "/" in tok:
                    target_pkg, feat = tok.split("/", 1)
                    if target_pkg == pkg:
                        active.add(feat)
                elif tok in pkg_features:
                    active.add(tok)
            pending = list(active)
            while pending:
                for dep in pkg_features.get(pending.pop(), ()):
                    if dep in pkg_features and dep not in active:
                        active.add(dep)
                        pending.append(dep)
        active_by_pkg[pkg] = active

    built: set[str] = set()
    if explicit_bins:
        for bin_name in explicit_bins:
            owners = [pkg for pkg in selected if bin_name in packages[pkg]["bins"]]
            if not owners:
                raise AssertionError(
                    f"cargo build requested --bin {bin_name!r}, which none of {sorted(selected)} defines"
                )
            eligible = [
                pkg for pkg in owners
                if set(packages[pkg]["bins"][bin_name]).issubset(active_by_pkg[pkg])
            ]
            if not eligible:
                raise AssertionError(
                    f"cargo build requested --bin {bin_name!r} without its required features"
                )
            built.add(bin_name)
    if has_bins or has_all_targets or (not explicit_bins and not has_lib and not has_other_filter):
        for pkg in selected:
            for bin_name, required in packages[pkg]["bins"].items():
                if set(required).issubset(active_by_pkg[pkg]):
                    built.add(bin_name)
    return built


def required_staged_payload(stage_script: Path, *, primary_files: bool) -> tuple[set[str], dict[str, str]]:
    """Derive required executables and their staged paths by running dist/stage-payload.sh."""
    with tempfile.TemporaryDirectory(prefix="stage-inventory-") as temporary:
        temp = Path(temporary)
        bin_dir = temp / "bins"
        bin_dir.mkdir()
        stage_dir = temp / "stage"
        found: set[str] = set()
        args = ["bash", str(stage_script), str(bin_dir), str(stage_dir)]
        if primary_files:
            args.append("--primary-files")
        prefix = f"missing executable: {bin_dir}/"
        while True:
            if stage_dir.exists():
                shutil.rmtree(stage_dir)
            result = subprocess.run(args, capture_output=True, text=True)
            if result.returncode == 0:
                break
            missing = None
            for line in result.stderr.splitlines():
                if line.startswith(prefix):
                    missing = line[len(prefix):].strip()
                    break
            if not missing:
                raise AssertionError(
                    f"{stage_script} failed without naming a missing executable "
                    f"(exit {result.returncode}): {result.stderr}"
                )
            if missing in found:
                raise AssertionError(f"{stage_script} repeatedly reported {missing!r} missing")
            found.add(missing)
            stub = bin_dir / missing
            stub.write_text(f"#!/bin/sh\n# staged-bin:{missing}\nexit 0\n", encoding="utf-8")
            stub.chmod(0o755)

        if not found:
            raise AssertionError(f"{stage_script} required no executables")

        staged_paths: dict[str, str] = {}
        for path in sorted(stage_dir.rglob("*")):
            if not path.is_file():
                continue
            text = path.read_text(encoding="utf-8", errors="ignore")
            match = re.search(r"^# staged-bin:(\S+)$", text, re.MULTILINE)
            if match:
                staged_paths[match.group(1)] = path.relative_to(stage_dir).as_posix()
        return found, staged_paths


def _write_cargo_stub(path: Path, *, materialize_release_bins: bool = False) -> None:
    if materialize_release_bins:
        script = (
            f"#!{sys.executable}\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            f"sys.path.insert(0, {str(Path(__file__).resolve().parent)!r})\n"
            "from test_payload_builders import built_bins_for_cargo_args\n"
            "with open(os.environ['CARGO_CALLS'], 'a', encoding='utf-8') as f:\n"
            "    f.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "packages = json.loads(Path(os.environ['PACKAGES_JSON']).read_text(encoding='utf-8'))\n"
            "built = built_bins_for_cargo_args(sys.argv[1:], packages)\n"
            "rel = Path(os.environ['MATERIALIZE_DIR'])\n"
            "rel.mkdir(parents=True, exist_ok=True)\n"
            "for name in built:\n"
            "    exe = rel / name\n"
            "    exe.write_text('#!/bin/sh\\nexit 0\\n', encoding='utf-8')\n"
            "    exe.chmod(0o755)\n"
        )
    else:
        script = (
            f"#!{sys.executable}\n"
            "import json, os, sys\n"
            "with open(os.environ['CARGO_CALLS'], 'a', encoding='utf-8') as f:\n"
            "    f.write(json.dumps(sys.argv[1:]) + '\\n')\n"
        )
    path.write_text(script, encoding="utf-8")
    path.chmod(0o755)


def _write_stage_stub(path: Path, *, exit_code: int) -> None:
    script = (
        f"#!{sys.executable}\n"
        "import json, os, sys\n"
        "with open(os.environ['STAGE_CALL'], 'w', encoding='utf-8') as f:\n"
        "    json.dump(sys.argv[1:], f)\n"
        f"sys.exit({exit_code})\n"
    )
    path.write_text(script, encoding="utf-8")
    path.chmod(0o755)


def _write_portable_realpath(path: Path) -> None:
    """Provide GNU `realpath -m` semantics on hosts whose system realpath rejects `-m`."""
    script = (
        f"#!{sys.executable}\n"
        "import os, sys\n"
        "args = [a for a in sys.argv[1:] if a != '-m']\n"
        "for item in args:\n"
        "    print(os.path.normpath(os.path.abspath(item)))\n"
    )
    path.write_text(script, encoding="utf-8")
    path.chmod(0o755)


def _clean_env(stub_bin: Path, extra: dict[str, str]) -> dict[str, str]:
    env = dict(os.environ)
    for var in ("CARGO_TARGET_DIR", "TILO_BIN_DIR", "TILO_CARGO_TARGET", "TILO_BUILD_PROFILE", "TILO_SKIP_BUILD"):
        env.pop(var, None)
    env["PATH"] = os.pathsep.join([str(stub_bin), str(Path(sys.executable).parent), env.get("PATH", "")])
    env.update(extra)
    return env


def _read_calls(log: Path) -> list[list[str]]:
    if not log.is_file():
        return []
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines() if line.strip()]


def inspect_debian_builder(
    root: Path,
    packages: dict[str, dict],
    *,
    build_binaries: Path | None = None,
    build_deb: Path | None = None,
) -> tuple[set[str], bool]:
    """Run dist/debian/build-deb.sh -> dist/build-binaries.sh against a stub cargo."""
    with tempfile.TemporaryDirectory(prefix="debian-builder-") as temporary:
        tmp = Path(temporary)
        (tmp / "dist/debian").mkdir(parents=True)
        (tmp / "bin").mkdir()
        shutil.copy2(root / "dist/VERSION", tmp / "dist/VERSION")
        shutil.copy2(build_binaries or (root / "dist/build-binaries.sh"), tmp / "dist/build-binaries.sh")
        (tmp / "dist/build-binaries.sh").chmod(0o755)
        shutil.copy2(build_deb or (root / "dist/debian/build-deb.sh"), tmp / "dist/debian/build-deb.sh")
        (tmp / "dist/debian/build-deb.sh").chmod(0o755)
        _write_cargo_stub(tmp / "bin/cargo")
        _write_stage_stub(tmp / "dist/stage-payload.sh", exit_code=STAGE_SENTINEL_EXIT)
        cargo_log = tmp / "cargo.calls"
        stage_log = tmp / "stage.call"
        env = _clean_env(
            tmp / "bin",
            {
                "CARGO_CALLS": str(cargo_log),
                "STAGE_CALL": str(stage_log),
                "TILO_DEB_ARCH": "amd64",
                "TILO_PACKAGE_OUT": str(tmp / "out"),
            },
        )
        result = subprocess.run(
            ["bash", str(tmp / "dist/debian/build-deb.sh")],
            env=env, capture_output=True, text=True,
        )
        if result.returncode != STAGE_SENTINEL_EXIT:
            raise AssertionError(
                f"dist/debian/build-deb.sh failed before staging (exit {result.returncode}): {result.stderr}"
            )
        stage_args = json.loads(stage_log.read_text(encoding="utf-8"))
        built: set[str] = set()
        for call in _read_calls(cargo_log):
            built.update(built_bins_for_cargo_args(call, packages))
        return built, "--primary-files" in stage_args


def inspect_arch_builder(
    root: Path,
    packages: dict[str, dict],
    *,
    pkgbuild: Path | None = None,
) -> tuple[set[str], bool]:
    """Execute build() from dist/arch/PKGBUILD against a stub cargo and stage-payload.sh."""
    with tempfile.TemporaryDirectory(prefix="arch-builder-") as temporary:
        tmp = Path(temporary)
        (tmp / "bin").mkdir()
        pkgbuild_copy = tmp / "PKGBUILD"
        shutil.copy2(pkgbuild or (root / "dist/arch/PKGBUILD"), pkgbuild_copy)
        _write_cargo_stub(tmp / "bin/cargo")
        stage_stub = tmp / "stage-stub.sh"
        _write_stage_stub(stage_stub, exit_code=0)
        cargo_log = tmp / "cargo.calls"
        stage_log = tmp / "stage.call"
        env = _clean_env(tmp / "bin", {"CARGO_CALLS": str(cargo_log), "STAGE_CALL": str(stage_log)})
        harness = (
            'set -euo pipefail\n'
            'source "$1"\n'
            'srcdir="$2"\n'
            'mkdir -p "$srcdir/tilo-$pkgver/dist"\n'
            'cp "$3" "$srcdir/tilo-$pkgver/dist/stage-payload.sh"\n'
            'chmod +x "$srcdir/tilo-$pkgver/dist/stage-payload.sh"\n'
            'cd "$srcdir"\n'
            'build\n'
        )
        result = subprocess.run(
            ["bash", "-c", harness, "_", str(pkgbuild_copy), str(tmp), str(stage_stub)],
            env=env, capture_output=True, text=True,
        )
        if result.returncode != 0:
            raise AssertionError(f"dist/arch/PKGBUILD build() failed (exit {result.returncode}): {result.stderr}")
        stage_args = json.loads(stage_log.read_text(encoding="utf-8"))
        built: set[str] = set()
        for call in _read_calls(cargo_log):
            built.update(built_bins_for_cargo_args(call, packages))
        return built, "--primary-files" in stage_args


def inspect_rpm_builder(
    root: Path,
    packages: dict[str, dict],
    *,
    build_rpm: Path | None = None,
) -> tuple[set[str], bool, subprocess.CompletedProcess[str]]:
    """Run dist/rpm/build-rpm.sh against a stub cargo that materializes only built binaries."""
    with tempfile.TemporaryDirectory(prefix="rpm-builder-") as temporary:
        tmp = Path(temporary)
        (tmp / "dist/rpm").mkdir(parents=True)
        (tmp / "bin").mkdir()
        shutil.copy2(root / "dist/VERSION", tmp / "dist/VERSION")
        rpm_script = build_rpm or (root / "dist/rpm/build-rpm.sh")
        shutil.copy2(rpm_script, tmp / "dist/rpm/build-rpm.sh")
        (tmp / "dist/rpm/build-rpm.sh").chmod(0o755)
        (tmp / "dist/rpm/test-rpmlint-config.py").write_text("", encoding="utf-8")
        rpmbuild = tmp / "dist/rpm/rpmbuild.sh"
        rpmbuild.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        rpmbuild.chmod(0o755)
        _write_portable_realpath(tmp / "bin/realpath")
        _write_cargo_stub(tmp / "bin/cargo", materialize_release_bins=True)
        _write_stage_stub(tmp / "dist/stage-payload.sh", exit_code=STAGE_SENTINEL_EXIT)
        packages_json = tmp / "packages.json"
        packages_json.write_text(json.dumps(packages), encoding="utf-8")
        cargo_log = tmp / "cargo.calls"
        stage_log = tmp / "stage.call"
        env = _clean_env(
            tmp / "bin",
            {
                "CARGO_CALLS": str(cargo_log),
                "STAGE_CALL": str(stage_log),
                "PACKAGES_JSON": str(packages_json),
                "MATERIALIZE_DIR": str(tmp / "target/release"),
                "TILO_PACKAGE_OUT": str(tmp / "out"),
            },
        )
        result = subprocess.run(
            ["bash", str(tmp / "dist/rpm/build-rpm.sh")],
            env=env, capture_output=True, text=True,
        )
        built: set[str] = set()
        for call in _read_calls(cargo_log):
            built.update(built_bins_for_cargo_args(call, packages))
        if stage_log.is_file():
            stage_args = json.loads(stage_log.read_text(encoding="utf-8"))
            primary_files = "--primary-files" in stage_args
        else:
            text = rpm_script.read_text(encoding="utf-8")
            stage_lines = [line for line in text.splitlines() if "dist/stage-payload.sh" in line]
            primary_files = any("--primary-files" in line for line in stage_lines)
        return built, primary_files, result


def inspect_nix_builder(
    root: Path,
    packages: dict[str, dict],
    *,
    package_nix: Path | None = None,
) -> tuple[set[str], bool]:
    """Parse cargoBuildFlags and the stage-payload.sh call from dist/nix/package.nix."""
    text = (package_nix or (root / "dist/nix/package.nix")).read_text(encoding="utf-8")
    match = re.search(r"\bcargoBuildFlags\s*=\s*\[(.*?)\];", text, re.DOTALL)
    if not match:
        raise AssertionError("dist/nix/package.nix has no cargoBuildFlags list")
    flags: list[str] = []
    for token in re.finditer(r'"([^"\\]*(?:\\.[^"\\]*)*)"|#[^\n]*|(\S+)', match.group(1)):
        if token.group(1) is not None:
            flags.append(token.group(1))
        elif token.group(2) is not None:
            raise AssertionError(
                f"dist/nix/package.nix cargoBuildFlags contains unsupported token: {token.group(2)!r}"
            )
    stage_lines = [
        line for line in text.splitlines()
        if "dist/stage-payload.sh" in line and not line.lstrip().startswith("#")
    ]
    if not stage_lines:
        raise AssertionError("dist/nix/package.nix does not invoke dist/stage-payload.sh")
    primary_files = any("--primary-files" in line for line in stage_lines)
    return built_bins_for_cargo_args(["build", *flags], packages), primary_files


class PayloadBuilderContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.packages = load_workspace_packages(ROOT)
        cls.stage_script = ROOT / "dist/stage-payload.sh"
        cls.base_bins, cls.base_staged = required_staged_payload(cls.stage_script, primary_files=False)
        cls.primary_bins, cls.primary_staged = required_staged_payload(cls.stage_script, primary_files=True)

    def required_for(self, primary_files: bool) -> set[str]:
        return self.primary_bins if primary_files else self.base_bins

    def test_stage_payload_inventory_distinguishes_primary_files(self):
        self.assertTrue(self.base_bins)
        self.assertTrue(self.base_bins < self.primary_bins,
                        "dist/stage-payload.sh --primary-files should add host-primary executables")
        self.assertEqual(set(self.base_staged), self.base_bins)
        self.assertEqual(set(self.primary_staged), self.primary_bins)

    def test_debian_builder_builds_every_staged_executable(self):
        built, primary_files = inspect_debian_builder(ROOT, self.packages)
        self.assertFalse(primary_files)
        missing = sorted(self.required_for(primary_files) - built)
        self.assertEqual(missing, [], f"dist/build-binaries.sh does not build: {missing}")

    def test_arch_builder_builds_every_staged_executable(self):
        built, primary_files = inspect_arch_builder(ROOT, self.packages)
        self.assertFalse(primary_files)
        missing = sorted(self.required_for(primary_files) - built)
        self.assertEqual(missing, [], f"dist/arch/PKGBUILD does not build: {missing}")

    def test_rpm_builder_builds_every_staged_executable_including_primary_files(self):
        built, primary_files, result = inspect_rpm_builder(ROOT, self.packages)
        self.assertTrue(primary_files)
        missing = sorted(self.required_for(primary_files) - built)
        self.assertEqual(missing, [], f"dist/rpm/build-rpm.sh does not build: {missing}")
        self.assertEqual(
            result.returncode, STAGE_SENTINEL_EXIT,
            f"dist/rpm/build-rpm.sh failed before staging: {result.stderr}",
        )

    def test_nix_builder_builds_every_staged_executable(self):
        built, primary_files = inspect_nix_builder(ROOT, self.packages)
        self.assertFalse(primary_files)
        missing = sorted(self.required_for(primary_files) - built)
        self.assertEqual(missing, [], f"dist/nix/package.nix does not build: {missing}")

    def test_verify_payload_checks_every_base_staged_executable(self):
        verifier = (ROOT / "dist/verify-payload.sh").read_text(encoding="utf-8")
        file_loop = re.search(r"for file in (.*?);\s*do", verifier, re.DOTALL)
        elf_loop = re.search(r"for elf in (.*?);\s*do", verifier, re.DOTALL)
        self.assertIsNotNone(file_loop, "dist/verify-payload.sh has no executable presence loop")
        self.assertIsNotNone(elf_loop, "dist/verify-payload.sh has no ELF inspection loop")
        nix_verifier = (ROOT / "dist/ci/verify-nix-result.sh").read_text(encoding="utf-8")
        nix_file_loop = re.search(r"for file in (.*?);\s*do", nix_verifier, re.DOTALL)
        self.assertIsNotNone(nix_file_loop, "dist/ci/verify-nix-result.sh has no executable presence loop")
        for staged_rel in sorted(self.base_staged.values()):
            basename = Path(staged_rel).name
            with self.subTest(executable=basename):
                self.assertRegex(file_loop.group(1), rf"/{re.escape(basename)}(?:[\"'\s]|$)")
                self.assertRegex(elf_loop.group(1), rf"/{re.escape(basename)}(?:[\"'\s]|$)")
                self.assertRegex(nix_file_loop.group(1), rf"/{re.escape(basename)}(?:[\"'\s]|$)")

    def test_dropping_a_staged_executable_from_any_builder_is_caught(self):
        with tempfile.TemporaryDirectory(prefix="builder-mutation-") as temporary:
            tmp = Path(temporary)

            # Debian: drop tilolang-process build line.
            deb = tmp / "build-binaries.sh"
            deb_src = (ROOT / "dist/build-binaries.sh").read_text(encoding="utf-8")
            deb_mutated = "\n".join(
                line for line in deb_src.splitlines() if "tilolang-process" not in line
            ) + "\n"
            self.assertNotEqual(deb_mutated, deb_src)
            deb.write_text(deb_mutated, encoding="utf-8")
            built, primary = inspect_debian_builder(ROOT, self.packages, build_binaries=deb)
            self.assertIn("tilolang-process", self.required_for(primary) - built)

            # Arch: drop tilolang-process build line.
            arch = tmp / "PKGBUILD"
            arch_src = (ROOT / "dist/arch/PKGBUILD").read_text(encoding="utf-8")
            arch_mutated = "\n".join(
                line for line in arch_src.splitlines()
                if "cargo build" not in line or "tilolang-process" not in line
            ) + "\n"
            self.assertNotEqual(arch_mutated, arch_src)
            arch.write_text(arch_mutated, encoding="utf-8")
            built, primary = inspect_arch_builder(ROOT, self.packages, pkgbuild=arch)
            self.assertIn("tilolang-process", self.required_for(primary) - built)

            # RPM: drop a --primary-files binary (fs) and a base binary (tilolang-process).
            rpm = tmp / "build-rpm.sh"
            rpm_src = (ROOT / "dist/rpm/build-rpm.sh").read_text(encoding="utf-8")
            rpm_mutated = rpm_src.replace(" --bin tilolang-process", "").replace(" --bin fs", "")
            self.assertNotEqual(rpm_mutated, rpm_src)
            rpm.write_text(rpm_mutated, encoding="utf-8")
            built, primary, _ = inspect_rpm_builder(ROOT, self.packages, build_rpm=rpm)
            self.assertTrue(primary)
            self.assertTrue({"tilolang-process", "fs"}.issubset(self.required_for(primary) - built))

            # Nix: drop --bin tilolang-process from cargoBuildFlags.
            nix = tmp / "package.nix"
            nix_src = (ROOT / "dist/nix/package.nix").read_text(encoding="utf-8")
            nix_mutated = nix_src.replace('    "--bin" "tilolang-process"\n', "")
            self.assertNotEqual(nix_mutated, nix_src)
            nix.write_text(nix_mutated, encoding="utf-8")
            built, primary = inspect_nix_builder(ROOT, self.packages, package_nix=nix)
            self.assertIn("tilolang-process", self.required_for(primary) - built)


if __name__ == "__main__":
    unittest.main()
