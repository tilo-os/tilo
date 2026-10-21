"""The pre-commit allowance for a declared lock-only `cargo update`.

The fixture is a real Cargo workspace over a local registry, so every
lock change below is one Cargo itself wrote with `cargo update --offline`.
"""
from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import sys
import tarfile
import textwrap
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
HOOK = ROOT / ".githooks" / "pre-commit"
TOOLCHAIN = tomllib.loads((ROOT / "rust-toolchain.toml").read_text(encoding="utf-8"))[
    "toolchain"
]["channel"]
SPEC = importlib.util.spec_from_file_location(
    "check_lock_update", ROOT / "tools/ci/check_lock_update.py"
)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = checker  # dataclasses resolve annotations through the module
SPEC.loader.exec_module(checker)

# name -> version -> dependencies (name, requirement)
REGISTRY = {
    "dep-x": {"1.0.0": [], "1.0.1": [], "1.0.2": [("dep-z", "1")]},
    "dep-y": {"1.0.0": [], "1.0.1": []},
    "dep-z": {"1.0.0": []},
}


def _publish(registry: Path, name: str, version: str, deps: list[tuple[str, str]]) -> None:
    """Add ``name`` ``version`` to a Cargo local registry (index + .crate)."""
    dependencies = "".join(f'{dep} = "{req}"\n' for dep, req in deps)
    manifest = (
        textwrap.dedent(
            f"""\
            [package]
            name = "{name}"
            version = "{version}"
            edition = "2021"

            [dependencies]
            """
        )
        + dependencies
    ).encode()
    archive = io.BytesIO()
    with tarfile.open(fileobj=archive, mode="w:gz") as tar:
        for relative, data in (("Cargo.toml", manifest), ("src/lib.rs", b"")):
            info = tarfile.TarInfo(f"{name}-{version}/{relative}")
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    crate = archive.getvalue()
    registry.mkdir(parents=True, exist_ok=True)
    (registry / f"{name}-{version}.crate").write_bytes(crate)
    index = registry / "index" / name[:2] / name[2:4] / name
    index.parent.mkdir(parents=True, exist_ok=True)
    record = {
        "name": name,
        "vers": version,
        "deps": [
            {
                "name": dep,
                "req": req,
                "features": [],
                "optional": False,
                "default_features": True,
                "target": None,
                "kind": "normal",
            }
            for dep, req in deps
        ],
        "cksum": hashlib.sha256(crate).hexdigest(),
        "features": {},
        "yanked": False,
    }
    with index.open("a", encoding="utf-8") as lines:
        lines.write(json.dumps(record) + "\n")


class LockUpdateAllowanceTests(unittest.TestCase):
    def setUp(self) -> None:
        if shutil.which("cargo") is None:
            self.fail("cargo is required: the allowance is defined by cargo metadata --locked")
        self._tmp = tempfile.TemporaryDirectory(prefix="lock-update-")
        self.root = Path(self._tmp.name) / "ws"
        vendor = Path(self._tmp.name) / "registry"
        for name, versions in REGISTRY.items():
            for version, deps in versions.items():
                _publish(vendor, name, version, deps)
        (self.root / "app/src").mkdir(parents=True)
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["app"]\nresolver = "2"\n', encoding="utf-8"
        )
        (self.root / "app/Cargo.toml").write_text(
            textwrap.dedent(
                """\
                [package]
                name = "app"
                version = "0.1.0"
                edition = "2021"

                [dependencies]
                dep-x = "1"
                dep-y = "1"
                """
            ),
            encoding="utf-8",
        )
        (self.root / "app/src/lib.rs").write_text("", encoding="utf-8")
        (self.root / ".cargo").mkdir()
        (self.root / ".cargo/config.toml").write_text(
            textwrap.dedent(
                f"""\
                [source.crates-io]
                replace-with = "fixture"

                [source.fixture]
                local-registry = "{vendor}"
                """
            ),
            encoding="utf-8",
        )
        self.env = {
            **os.environ,
            "CARGO_HOME": str(Path(self._tmp.name) / "cargo-home"),
            "CARGO_NET_OFFLINE": "true",
            # The fixture lies outside the repository, where rustup would not
            # see rust-toolchain.toml; images may install only that toolchain.
            "RUSTUP_TOOLCHAIN": TOOLCHAIN,
            "GIT_AUTHOR_NAME": "fixture",
            "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
            "GIT_COMMITTER_NAME": "fixture",
            "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
        }
        self.env.pop(checker.INTENT_VARIABLE, None)
        self._run("git", "init", "-q")
        self._cargo("generate-lockfile")
        self._cargo("update", "-p", "dep-x", "--precise", "1.0.0")
        self._cargo("update", "-p", "dep-y", "--precise", "1.0.0")
        self._run("git", "add", "-A")
        self._run("git", "commit", "-q", "--no-verify", "-m", "fixture")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _run(self, *command: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            command, cwd=self.root, env=self.env, text=True, capture_output=True, check=True
        )

    def _cargo(self, *args: str) -> None:
        self._run("cargo", *args, "--offline")

    def _stage_update(self, *args: str) -> None:
        self._cargo("update", *args)
        self._run("git", "add", "Cargo.lock")

    def _check(self, intent: str | None) -> list[str]:
        old_env = dict(os.environ)
        os.environ.clear()
        os.environ.update(self.env)
        try:
            return checker.check(self.root, intent)
        finally:
            os.environ.clear()
            os.environ.update(old_env)

    def test_declared_precise_bump_is_admitted(self) -> None:
        self._stage_update("-p", "dep-x", "--precise", "1.0.1")

        self.assertEqual(self._check("-p dep-x --precise 1.0.1"), [])
        self.assertEqual(self._check("cargo update --package=dep-x@1.0.0 --precise=1.0.1"), [])

    def test_bump_that_pulls_in_a_new_dependency_is_admitted(self) -> None:
        self._stage_update("-p", "dep-x", "--precise", "1.0.2")

        lock = (self.root / "Cargo.lock").read_text(encoding="utf-8")
        self.assertIn('name = "dep-z"', lock)
        self.assertEqual(self._check("-p dep-x --precise 1.0.2"), [])

    def test_several_declared_updates_are_admitted(self) -> None:
        self._cargo("update", "-p", "dep-x", "--precise", "1.0.1")
        self._stage_update("-p", "dep-y", "--precise", "1.0.1")

        self.assertEqual(self._check("-p dep-x --precise 1.0.1; -p dep-y --precise 1.0.1"), [])

    def test_bump_not_matching_the_intent_is_refused(self) -> None:
        self._stage_update("-p", "dep-x", "--precise", "1.0.1")

        wrong_crate = self._check("-p dep-y --precise 1.0.1")
        self.assertTrue(any("dep-x moved 1.0.0 -> 1.0.1 but is not declared" in p for p in wrong_crate))
        self.assertTrue(any("dep-y is declared but its lock entry did not change" in p for p in wrong_crate))

        wrong_version = self._check("-p dep-x --precise 1.0.2")
        self.assertTrue(any("declared at 1.0.2 but moved to 1.0.1" in p for p in wrong_version))

    def test_undeclared_transitive_move_is_refused(self) -> None:
        self._stage_update()  # a fresh resolve moves every crate

        problems = self._check("-p dep-x")
        self.assertTrue(any("dep-y moved 1.0.0 -> 1.0.1 but is not declared" in p for p in problems))

    def test_lock_change_without_intent_is_refused(self) -> None:
        self._stage_update("-p", "dep-x", "--precise", "1.0.1")

        problems = self._check(None)
        self.assertEqual(len(problems), 1)
        self.assertIn("Cargo.lock changed with no Cargo.toml beside it", problems[0])
        self.assertIn("TILO_CARGO_UPDATE=", problems[0])

    def test_relock_after_a_merge_is_admitted_without_intent(self) -> None:
        # A rebase took main's lock over a commit's manifest change (.githooks/merge-generated):
        # the committed manifest needs dep-z, which the committed lock lacks.
        manifest = self.root / "app/Cargo.toml"
        manifest.write_text(manifest.read_text(encoding="utf-8") + 'dep-z = "1"\n', encoding="utf-8")
        self._run("git", "commit", "-q", "--no-verify", "-am", "add dep-z, lock taken from main")
        self._cargo("metadata", "--format-version", "1")
        self._run("git", "add", "Cargo.lock")

        lock = (self.root / "Cargo.lock").read_text(encoding="utf-8")
        self.assertIn('name = "dep-z"', lock)
        self.assertEqual(self._check(None), [])

    def test_relock_onto_a_package_already_locked_is_admitted(self) -> None:
        # dep-x 1.0.2 brings dep-z into the lock; a committed manifest then
        # names dep-z directly while the lock still lacks that reference.
        self._cargo("update", "-p", "dep-x", "--precise", "1.0.2")
        self._run("git", "commit", "-q", "--no-verify", "-am", "dep-x 1.0.2")
        manifest = self.root / "app/Cargo.toml"
        manifest.write_text(manifest.read_text(encoding="utf-8") + 'dep-z = "1"\n', encoding="utf-8")
        self._run("git", "commit", "-q", "--no-verify", "-am", "app names dep-z, lock taken from main")
        self._cargo("metadata", "--format-version", "1")
        self._run("git", "add", "Cargo.lock")

        self.assertNotEqual(self._run("git", "diff", "--cached", "--stat").stdout, "")
        self.assertEqual(self._check(None), [])

    def test_broad_intent_is_refused(self) -> None:
        self._stage_update("-p", "dep-x", "--precise", "1.0.1")

        for intent in ("--workspace", "-p dep-x --recursive", "--precise 1.0.1", ""):
            with self.subTest(intent=intent):
                problems = self._check(intent or " ; ")
                self.assertEqual(len(problems), 1)
                self.assertIn("TILO_CARGO_UPDATE", problems[0])

    def test_lock_that_cargo_metadata_rejects_is_refused(self) -> None:
        self._stage_update("-p", "dep-x", "--precise", "1.0.1")
        # A throwaway fixture: claim a dependency no manifest declares, which
        # would otherwise pass as a dependency list change of a declared crate.
        lock = self.root / "Cargo.lock"
        text = lock.read_text(encoding="utf-8")
        entry = 'name = "dep-x"\nversion = "1.0.1"\n'
        self.assertIn(entry, text)
        start = text.index(entry)
        checksum_end = text.index("\n", text.index("checksum", start)) + 1
        lock.write_text(
            text[:checksum_end] + 'dependencies = [\n "dep-y",\n]\n' + text[checksum_end:],
            encoding="utf-8",
        )
        self._run("git", "add", "Cargo.lock")

        problems = self._check("-p dep-x --precise 1.0.1")
        self.assertEqual(len(problems), 1)
        self.assertIn("cargo metadata --locked rejects the staged lock", problems[0])

    def test_unstaged_lock_edits_are_refused(self) -> None:
        self._stage_update("-p", "dep-x", "--precise", "1.0.1")
        self._cargo("update", "-p", "dep-y", "--precise", "1.0.1")

        problems = self._check("-p dep-x --precise 1.0.1")
        self.assertEqual(len(problems), 1)
        self.assertIn("differs from the staged lock", problems[0])

    def test_hook_delegates_lock_only_changes_to_the_checker(self) -> None:
        hook = HOOK.read_text(encoding="utf-8")

        self.assertIn('python3 "$root/tools/ci/check_lock_update.py" --root "$root" || fail=1', hook)


class LockDiffTests(unittest.TestCase):
    OLD = textwrap.dedent(
        """\
        version = 4

        [[package]]
        name = "a"
        version = "1.0.0"
        source = "registry+https://github.com/rust-lang/crates.io-index"
        checksum = "aa"

        [[package]]
        name = "b"
        version = "1.0.0"
        source = "registry+https://github.com/rust-lang/crates.io-index"
        checksum = "bb"
        dependencies = [
         "a",
        ]
        """
    )

    def test_checksum_change_at_the_same_version_is_refused(self) -> None:
        new = self.OLD.replace('checksum = "bb"', 'checksum = "cc"')
        problems = checker.lock_violations(self.OLD, new, checker.parse_intent("-p a"))
        self.assertIn("b 1.0.0 changed beyond its dependencies", problems)

    def test_dependency_reference_to_a_declared_crate_may_change(self) -> None:
        new = self.OLD.replace(
            'version = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "aa"',
            'version = "1.0.1"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "ab"',
        ).replace(' "a",', ' "a 1.0.1",')
        self.assertEqual(
            checker.lock_violations(self.OLD, new, checker.parse_intent("-p a --precise 1.0.1")),
            [],
        )

    def test_unrelated_dependency_list_change_is_refused(self) -> None:
        new = (
            self.OLD.replace(' "a",', ' "a",\n "c",')
            + '\n[[package]]\nname = "c"\nversion = "1.0.0"\nchecksum = "c0"\n'
        )
        problems = checker.lock_violations(self.OLD, new, checker.parse_intent("-p a"))
        self.assertIn("c 1.0.0 was added but no declared update needs it", problems)


if __name__ == "__main__":
    unittest.main()
