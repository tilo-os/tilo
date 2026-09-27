#!/usr/bin/env python3
"""Published OS images keep thin LTO; every other build links without it.

os/Containerfile takes TILO_RUST_LTO (default thin). CI and os/dev/build-image.sh
choose it from whether the build publishes, and a publication refuses an image
whose org.tilo.rust-lto label says otherwise.
"""

from __future__ import annotations

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
WORKFLOW = REPO / ".forgejo/workflows/build-image.yml"
CONTAINERFILE = REPO / "os/Containerfile"
BUILD_IMAGE = REPO / "os/dev/build-image.sh"
COMMON = REPO / "os/dev/_common.sh"


def build_arg_blocks(text: str) -> list[str]:
    return re.findall(r"^ {10}build-args: \|\n((?: {12}.*\n)+)", text, re.M)


class RustLtoModeTests(unittest.TestCase):
    def test_every_image_build_passes_the_mode(self) -> None:
        blocks = build_arg_blocks(WORKFLOW.read_text())
        self.assertGreaterEqual(len(blocks), 5)
        for block in blocks:
            if "file: os/Containerfile" not in block and "BASE_REF" not in block:
                continue
            self.assertIn("TILO_RUST_LTO=${{ env.TILO_RUST_LTO }}", block)

    def test_the_release_mode_step_chooses_thin_only_for_a_publication(self) -> None:
        text = WORKFLOW.read_text()
        step = text[text.index("id: release-mode"):]
        step = step[:step.index("\n      - name:")]
        block = step[step.index('          if [[ "$publish" == true ]]'):]
        publish, other = block.split("\n          else\n", 1)
        self.assertIn("echo 'TILO_RUST_LTO=thin' >> \"$GITHUB_ENV\"", publish)
        self.assertNotIn("TILO_RUST_LTO=off", publish)
        self.assertIn("echo 'TILO_RUST_LTO=off' >> \"$GITHUB_ENV\"", other.split("          fi\n", 1)[0])

    def test_the_containerfile_defaults_to_thin_and_labels_the_image(self) -> None:
        text = CONTAINERFILE.read_text()
        self.assertEqual(re.findall(r"^ARG TILO_RUST_LTO=(\S+)$", text, re.M), ["thin", "thin"])
        self.assertIn('LABEL org.tilo.rust-lto="${TILO_RUST_LTO}"', text)
        self.assertIn('CARGO_PROFILE_RELEASE_LTO="$TILO_RUST_LTO"', text)
        self.assertNotIn("CARGO_PROFILE_RELEASE_LTO=thin", text)

    def mode(self, **env: str) -> str:
        """The mode build-image.sh resolves, from its own resolution block."""
        script = BUILD_IMAGE.read_text()
        start = script.index('if [ -z "${TILO_RUST_LTO:-}" ]; then')
        end = script.index("_tilo_jobs_args=()")
        block = script[start:end]
        with tempfile.TemporaryDirectory() as temporary:
            probe = Path(temporary) / "probe.sh"
            probe.write_text(f"source {COMMON}\nset -euo pipefail\ndie() {{ echo \"die: $*\"; exit 3; }}\n" + block
                             + 'echo "$TILO_RUST_LTO ${TILO_CARGO_BUILD_JOBS:-}"\n')
            clean = {k: v for k, v in os.environ.items() if not k.startswith("TILO_")}
            result = subprocess.run(["bash", str(probe)], env=clean | env, capture_output=True, text=True)
        return result.stdout.strip()

    def test_build_image_uses_thin_only_for_a_publication(self) -> None:
        self.assertTrue(self.mode().startswith("off "))
        self.assertTrue(self.mode(TILO_REQUIRE_PCR_PREDICTION="1").startswith("thin"))
        self.assertTrue(self.mode(TILO_RUST_LTO="off", TILO_REQUIRE_PCR_PREDICTION="1").startswith("die:"))
        self.assertTrue(self.mode(TILO_RUST_LTO="fat").startswith("die:"))
        jobs = int(self.mode().split()[1])
        # A CPU per job, as far as memory holds out (os/dev/_common.sh cargo_build_jobs).
        self.assertTrue(1 <= jobs <= (os.cpu_count() or 1), jobs)
        self.assertEqual(self.mode(TILO_REQUIRE_PCR_PREDICTION="1"), "thin")

    def test_a_publication_checks_the_label(self) -> None:
        script = BUILD_IMAGE.read_text()
        self.assertIn('{{index .Labels "org.tilo.rust-lto"}}', script)
        self.assertRegex(script, r'\[ "\$_tilo_image_lto" = thin \]')


if __name__ == "__main__":
    unittest.main()
