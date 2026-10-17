#!/usr/bin/env python3
"""The desktop package lane builds every userspace of its architecture."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

CI = Path(__file__).resolve().parents[1]


def run_lane(root, fail=""):
    """Run the real lane in `root` with stub jobs; `fail` is shell run first in each job."""
    ci = root / "dist/ci"
    ci.mkdir(parents=True)
    for name in ("package-lane.sh", "package-targets.sh", "package-targets.txt"):
        shutil.copy2(CI / name, ci / name)
    (ci / "fetch-package-env-runtime.sh").write_text("#!/bin/sh\n")
    # A container engine attached to the job drains stdin; the stub does too.
    job = ci / "run-package-job.sh"
    job.write_text('#!/bin/sh\ncat >/dev/null\n' + fail
                   + 'mkdir -p "dist/ci-output/$1-$3" && touch "dist/ci-output/$1-$3/built"\n')
    job.chmod(0o755)
    bin_dir = root / "bin"
    bin_dir.mkdir()
    for tool in ("docker", "skopeo"):
        (bin_dir / tool).write_text("#!/bin/sh\n")
        (bin_dir / tool).chmod(0o755)
    env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", "TMPDIR": str(root)}
    return subprocess.run(["bash", str(ci / "package-lane.sh"), "amd64"], cwd=root, env=env,
                          stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)


def built(root):
    return sorted(p.parent.name.removesuffix("-amd64")
                  for p in (root / "dist/ci-output").glob("*/built"))


class PackageLaneTests(unittest.TestCase):
    def test_every_target_builds_even_when_a_job_reads_stdin(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result = run_lane(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            targets = subprocess.check_output(["bash", str(CI / "package-targets.sh"), "amd64"],
                                              text=True).splitlines()
            distros = sorted(line.split("|")[0] for line in targets)
            self.assertIn("fedora-44", distros, "host-session-e2e consumes the fedora-44 RPMs")
            self.assertEqual(built(root), distros)

    def test_a_failing_userspace_fails_the_lane_after_the_others_build(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result = run_lane(root, 'case "$1" in nix) exit 3;; esac\n')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("failed userspaces: nix", result.stderr)
            self.assertIn("fedora-44", built(root))
            self.assertNotIn("nix", built(root))


if __name__ == "__main__":
    unittest.main()
