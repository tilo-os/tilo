#!/usr/bin/env python3
"""Exercise the actual vendored build script across unrelated repository commits."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "vendor/librespot-core"


class BuildIdentity(unittest.TestCase):
    def test_unrelated_commits_and_build_dates_preserve_runtime_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            environment = dict(os.environ, CARGO_PKG_VERSION="0.8.0")
            subprocess.run(
                ["rustc", "--edition=2024", str(SOURCE / "build.rs"),
                 "-o", str(scratch / "build-script")],
                env=environment, check=True,
            )
            subprocess.run(["git", "init", "-q", str(scratch)], check=True)
            environment.update(
                GIT_AUTHOR_NAME="Reproducibility fixture",
                GIT_AUTHOR_EMAIL="fixture@example.invalid",
                GIT_COMMITTER_NAME="Reproducibility fixture",
                GIT_COMMITTER_EMAIL="fixture@example.invalid",
            )
            outputs, revisions = [], []
            for epoch in (1_700_000_000, 1_800_000_000):
                environment.update(
                    SOURCE_DATE_EPOCH=str(epoch),
                    GIT_AUTHOR_DATE=f"@{epoch} +0000",
                    GIT_COMMITTER_DATE=f"@{epoch} +0000",
                )
                subprocess.run(
                    ["git", "-C", str(scratch), "commit", "-q", "--allow-empty",
                     "-m", "Unrelated application change"],
                    env=environment, check=True,
                )
                revisions.append(subprocess.check_output(
                    ["git", "-C", str(scratch), "rev-parse", "HEAD"], text=True,
                ))
                outputs.append(subprocess.check_output(
                    [str(scratch / "build-script")], cwd=scratch,
                    env=environment, text=True,
                ))
            self.assertNotEqual(*revisions)
            self.assertEqual(*outputs)
            upstream = json.loads((SOURCE / ".cargo_vcs_info.json").read_text())["git"]["sha1"]
            self.assertIn(f"cargo:rustc-env=VERGEN_GIT_SHA={upstream[:10]}\n", outputs[0])
            self.assertIn(f"cargo:rustc-env=LIBRESPOT_BUILD_ID=0.8.0-{upstream}\n", outputs[0])
            # Compile the production consumer too, including its user-agent fields.
            for line in outputs[0].splitlines():
                if line.startswith("cargo:rustc-env="):
                    name, value = line.removeprefix("cargo:rustc-env=").split("=", 1)
                    environment[name] = value
            consumer = scratch / "version-consumer.rs"
            consumer.write_text(
                'mod config { pub const OS: &str = "linux"; }\n'
                + '#[path = ' + json.dumps(str(SOURCE / "src/version.rs"))
                + '] pub mod version;\n'
            )
            subprocess.run(
                ["rustc", "--edition=2024", "--crate-type=lib",
                 str(consumer), "-o", str(scratch / "version.rlib")],
                env=environment, check=True,
            )


if __name__ == "__main__":
    unittest.main()
