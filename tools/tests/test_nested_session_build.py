"""The nested-session build refuses to resolve the env-runtime by name."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MESSAGE = (
    "the env-runtime tree comes from the image-env-runtime-dev-tree check; "
    "run through `t test nested-session` or `t exec --needs nested-session`"
)


class NestedSessionBuildEnvRuntimeTests(unittest.TestCase):
    def test_a_t_job_without_trees_names_the_check_that_publishes_them(self):
        # Without the trees the script used to resolve registry.tilo.so by name,
        # which no check's egress pins: a t job stops before any build instead,
        # and never fetches what its needs did not hand it.
        environment = dict(os.environ)
        environment.pop("TILO_ENV_RUNTIME_TREES", None)
        environment["T_RUN_ID"] = "a-run"
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                ["bash", "dist/dev/nested-session-build.sh", directory],
                cwd=ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(MESSAGE, result.stderr)

    def test_a_person_s_default_build_comes_from_t_and_compiles_nothing_here(self):
        # A fake `t` answers `t outputs nested-session --to DIR` the way t lays a check's
        # outputs out; no cargo is on PATH, so a build here would fail.
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            bin_dir = work / "bin"
            bin_dir.mkdir()
            (bin_dir / "t").write_text(
                "#!/bin/bash\n"
                '[[ $1 == outputs && $2 == nested-session && $3 == --to ]] || exit 9\n'
                'mkdir -p "$4/target/nested-session/bin" && echo built > "$4/target/nested-session/bin/tilo-session"\n'
            )
            (bin_dir / "t").chmod(0o755)
            for tool in ("bash", "find", "mkdir", "rm", "mv", "dirname", "realpath", "uname", "cat"):
                found = subprocess.run(["bash", "-c", f"command -v {tool}"], capture_output=True,
                                       text=True, check=True).stdout.strip()
                (bin_dir / tool).symlink_to(found)
            environment = {"PATH": str(bin_dir), "HOME": directory,
                           "CARGO_TARGET_DIR": str(work / "target")}
            out = work / "session"
            result = subprocess.run(
                ["bash", "dist/dev/nested-session-build.sh", str(out)],
                cwd=ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((out / "bin" / "tilo-session").read_text(), "built\n")
            self.assertFalse((work / "target" / "nested-session.fetching").exists())


if __name__ == "__main__":
    unittest.main()
