#!/usr/bin/env python3
"""The desktop package lane builds every userspace of its architecture."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

CI = Path(__file__).resolve().parents[1]


class PackageLaneTests(unittest.TestCase):
    def test_every_target_builds_even_when_a_job_reads_stdin(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ci = root / "dist/ci"
            ci.mkdir(parents=True)
            for name in ("package-lane.sh", "package-targets.sh", "package-targets.txt"):
                shutil.copy2(CI / name, ci / name)
            (ci / "fetch-package-env-runtime.sh").write_text("#!/bin/sh\n")
            # A container engine attached to the job drains stdin; the stub does too.
            job = ci / "run-package-job.sh"
            job.write_text('#!/bin/sh\ncat >/dev/null\n'
                           'mkdir -p "dist/ci-output/$1-$3" && touch "dist/ci-output/$1-$3/built"\n')
            job.chmod(0o755)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            for tool in ("docker", "skopeo"):
                (bin_dir / tool).write_text("#!/bin/sh\n")
                (bin_dir / tool).chmod(0o755)
            env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", "TMPDIR": temporary}
            subprocess.run(["bash", str(ci / "package-lane.sh"), "amd64"], cwd=root, env=env,
                           check=True, stdout=subprocess.DEVNULL)
            targets = subprocess.check_output(["bash", str(ci / "package-targets.sh"), "amd64"],
                                              text=True).splitlines()
            distros = sorted(line.split("|")[0] for line in targets)
            self.assertIn("fedora-44", distros, "host-session-e2e consumes the fedora-44 RPMs")
            built = sorted(p.parent.name.removesuffix("-amd64")
                           for p in (root / "dist/ci-output").glob("*/built"))
            self.assertEqual(built, distros)


if __name__ == "__main__":
    unittest.main()
