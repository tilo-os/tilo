#!/usr/bin/env python3
"""tools/policy/check_image_builder_inputs.py against small fixture repositories."""

from __future__ import annotations

import importlib.util
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("check_image_builder_inputs",
                                              REPO / "tools/policy/check_image_builder_inputs.py")
policy = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = policy
SPEC.loader.exec_module(policy)

# The stage the policy covers in os/Containerfile.
STAGE = dict(policy.STAGES)["os/Containerfile"]

NARROW = f"""\
FROM base AS {STAGE}
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml /build/
COPY .cargo /build/.cargo
COPY crates /build/crates
COPY vendor /build/vendor
COPY os/data.txt /build/os/data.txt
COPY os/scripts/build.sh /build/os/scripts/
RUN --mount=type=cache,target=/build/target sh os/scripts/build.sh && cp target/release/a /out/

FROM base AS rootfs
COPY os/etc/unit.conf /etc/unit.conf
"""

LIB = """\
pub const DATA: &str = include_str!("../../../os/data.txt");

#[cfg(test)]
mod tests {
    const FIXTURE: &str = include_str!("../../../os/tests/fixture.json");
}
"""


class ImageBuilderInputsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="tilo-builder-inputs-"))
        self.addCleanup(shutil.rmtree, self.root)
        (self.root / "tools/ci").mkdir(parents=True)
        shutil.copy(REPO / "tools/ci/suites.py", self.root / "tools/ci/suites.py")
        self.write("Cargo.toml", '[workspace]\nmembers = ["crates/a"]\n')
        self.write("Cargo.lock", "")
        self.write("rust-toolchain.toml", "")
        self.write(".cargo/config.toml", "")
        self.write("crates/a/Cargo.toml", '[package]\nname = "a"\n')
        self.write("crates/a/src/lib.rs", LIB)
        self.write("vendor/.keep", "")
        self.write("os/data.txt", "data\n")
        self.write("os/tests/fixture.json", "{}\n")
        self.write("os/scripts/build.sh", "#!/bin/sh\n")
        self.write("os/etc/unit.conf", "")

    def write(self, path: str, text: str) -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def findings(self, containerfile: str) -> list[str]:
        self.write("os/Containerfile", containerfile)
        tracked = sorted(str(p.relative_to(self.root)) for p in self.root.rglob("*") if p.is_file())
        return [str(f) for f in policy.audit(self.root, tracked)]

    def test_a_complete_narrow_copy_passes(self) -> None:
        self.assertEqual(self.findings(NARROW), [])

    def test_copying_the_whole_context_is_refused(self) -> None:
        found = self.findings(f"FROM base AS {STAGE}\nWORKDIR /build\nCOPY . /build\nRUN cargo build\n")
        self.assertEqual(len(found), 1, found)
        self.assertIn("copies the whole build context", found[0])

    def test_a_missing_release_include_is_named(self) -> None:
        found = self.findings(NARROW.replace("COPY os/data.txt /build/os/data.txt\n", ""))
        self.assertEqual(len(found), 1, found)
        self.assertIn("does not copy os/data.txt to /build/os/data.txt", found[0])
        self.assertIn("crates/a/src/lib.rs:1 include!s it", found[0])

    def test_a_test_only_include_is_not_required(self) -> None:
        self.assertFalse(any("fixture.json" in f for f in self.findings(NARROW)))

    def test_a_script_the_stage_runs_must_be_copied(self) -> None:
        found = self.findings(NARROW.replace("COPY os/scripts/build.sh /build/os/scripts/\n", ""))
        self.assertEqual(len(found), 1, found)
        self.assertIn("os/scripts/build.sh", found[0])

    def test_a_workspace_member_root_must_be_copied(self) -> None:
        found = self.findings(NARROW.replace("COPY crates /build/crates\n", ""))
        self.assertTrue(any("does not copy crates to /build/crates" in f for f in found), found)

    def test_a_copy_to_another_path_does_not_count(self) -> None:
        found = self.findings(NARROW.replace("/build/os/data.txt", "/build/data.txt"))
        self.assertTrue(any("os/data.txt" in f for f in found), found)

    def test_other_stages_may_copy_what_they_like(self) -> None:
        self.assertEqual(self.findings(NARROW + "COPY . /src\n"), [])

    def test_copies_of_a_scratch_source_stage_count(self) -> None:
        head, rest = NARROW.split("WORKDIR /build\n", 1)
        copies = "".join(line + "\n" for line in rest.splitlines() if line.startswith("COPY"))
        body = "\n".join(line for line in rest.splitlines() if not line.startswith("COPY"))
        staged = ("FROM scratch AS sources\n" + copies + "\n" + head + "WORKDIR /build\n"
                  + "COPY --from=sources /build/ /build/\n" + body + "\n")
        self.assertEqual(self.findings(staged), [])
        dropped = staged.replace("COPY os/data.txt /build/os/data.txt\n", "")
        self.assertTrue(any("os/data.txt" in f for f in self.findings(dropped)))
        whole = staged.replace("FROM scratch AS sources\n", "FROM scratch AS sources\nCOPY . /build\n")
        self.assertTrue(any("whole build context" in f for f in self.findings(whole)))

    def test_a_context_bind_mount_needs_no_copy(self) -> None:
        mounted = NARROW.replace(
            "RUN --mount=type=cache,target=/build/target sh os/scripts/build.sh",
            "RUN --mount=type=bind,source=os/scripts/check.sh,target=/run/check.sh bash /run/check.sh\n"
            "RUN --mount=type=cache,target=/build/target sh os/scripts/build.sh")
        self.write("os/scripts/check.sh", "#!/bin/sh\n")
        self.assertEqual(self.findings(mounted), [])

    def test_the_repository_passes(self) -> None:
        self.assertEqual([str(f) for f in policy.audit(REPO)], [])


if __name__ == "__main__":
    unittest.main()
