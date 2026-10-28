import os
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "os/usr/libexec/tilo-update-policy-check"
UNITS = REPO / "os/usr/lib/systemd/user"


class UpdatePolicyCheckTest(unittest.TestCase):
    def run_check(self, *args: str, metered: str) -> subprocess.CompletedProcess:
        with tempfile.TemporaryDirectory() as bin_dir:
            nmcli = Path(bin_dir) / "nmcli"
            nmcli.write_text(
                "#!/bin/sh\n"
                f"printf 'GENERAL.STATE:100 (connected)\\nGENERAL.METERED:{metered}\\n'\n"
            )
            nmcli.chmod(0o755)
            env = {**os.environ, "PATH": f"{bin_dir}:/usr/bin:/bin"}
            return subprocess.run(
                ["/bin/sh", str(SCRIPT), *args],
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )

    def test_background_work_always_spares_a_metered_connection(self) -> None:
        self.assertEqual(self.run_check("--background", metered="yes").returncode, 1)
        self.assertEqual(self.run_check("--background", metered="yes (guessed)").returncode, 1)
        self.assertEqual(self.run_check("--background", metered="no").returncode, 0)

    def test_an_unknown_mode_is_refused_rather_than_read_as_an_update(self) -> None:
        self.assertEqual(self.run_check("--sometimes", metered="no").returncode, 2)

    def test_the_catalogue_refresh_runs_only_when_the_device_may_spend_the_network(self) -> None:
        service = (UNITS / "tilo-public-content-refresh.service").read_text()
        self.assertIn(
            "ExecCondition=/usr/libexec/tilo-update-policy-check --background\n", service
        )
        self.assertIn("ConditionACPower=true\n", service)
        timer = (UNITS / "tilo-public-content-refresh.timer").read_text()
        self.assertIn("Persistent=yes\n", timer)
        for containerfile in ("os/Containerfile", "os/image/system.Containerfile"):
            text = (REPO / containerfile).read_text()
            self.assertIn("os/usr/lib/systemd/user/tilo-public-content-refresh.timer", text)
            self.assertIn("systemctl --global enable tilo-public-content-refresh.timer", text)


if __name__ == "__main__":
    unittest.main()
