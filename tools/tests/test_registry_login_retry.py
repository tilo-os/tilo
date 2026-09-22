from pathlib import Path
import os
import stat
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
LOGIN = ROOT / "os/dev/login-registry-with-retry.sh"
WORKFLOW = ROOT / ".forgejo/workflows/build-kernel.yml"


class RegistryLoginRetryTests(unittest.TestCase):
    def run_login(self, mode: str, password: bytes = b"token"):
        with tempfile.TemporaryDirectory() as temporary:
            temp = Path(temporary)
            state = temp / "attempts"
            state.write_text("0")
            fake = temp / "docker"
            fake.write_text(
                """#!/usr/bin/env bash
set -euo pipefail
[ "$1" = login ] && [ "$2" = code.sandbox.example ]
[ "$3" = --username ] && [ "$4" = ci-user ] && [ "$5" = --password-stdin ]
secret=$(cat)
[ "$secret" = token ]
attempt=$(( $(cat "$FAKE_LOGIN_STATE") + 1 ))
printf '%s' "$attempt" > "$FAKE_LOGIN_STATE"
case "$FAKE_LOGIN_MODE" in
  transient-then-ok)
    if [ "$attempt" -lt 3 ]; then
      echo 'login failed with status: 503 Service Unavailable' >&2
      exit 1
    fi
    echo 'Login Succeeded'
    ;;
  auth)
    echo 'login failed with status: 401 Unauthorized' >&2
    exit 1
    ;;
  transient)
    echo 'connection reset by peer' >&2
    exit 1
    ;;
  *) exit 90 ;;
esac
"""
            )
            fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{temp}:{environment['PATH']}",
                    "FAKE_LOGIN_STATE": str(state),
                    "FAKE_LOGIN_MODE": mode,
                    "TILO_REGISTRY_LOGIN_RETRY_DELAYS": "0 0",
                }
            )
            result = subprocess.run(
                [str(LOGIN), "docker", "code.sandbox.example", "ci-user"],
                input=password,
                capture_output=True,
                env=environment,
            )
            return result, int(state.read_text())

    def test_transient_registry_outage_retries_then_succeeds(self):
        result, attempts = self.run_login("transient-then-ok")
        self.assertEqual(0, result.returncode, result.stderr.decode())
        self.assertEqual(3, attempts)
        self.assertEqual(b"Login Succeeded\n", result.stdout)
        self.assertEqual(2, result.stderr.count(b"transiently unavailable"))

    def test_authentication_failure_never_retries(self):
        result, attempts = self.run_login("auth")
        self.assertNotEqual(0, result.returncode)
        self.assertEqual(1, attempts)
        self.assertIn(b"401 Unauthorized", result.stderr)
        self.assertNotIn(b"retrying", result.stderr)

    def test_retry_budget_is_bounded(self):
        result, attempts = self.run_login("transient")
        self.assertNotEqual(0, result.returncode)
        self.assertEqual(3, attempts)
        self.assertIn(b"still unavailable after 3 attempts", result.stderr)

    def test_empty_password_fails_before_invoking_cli(self):
        result, attempts = self.run_login("transient-then-ok", b"")
        self.assertEqual(2, result.returncode)
        self.assertEqual(0, attempts)
        self.assertIn(b"password is required", result.stderr)

    def test_kernel_job_uses_exact_shallow_checkout_and_bounded_login(self):
        workflow = WORKFLOW.read_text()
        start = workflow.index("  build-and-push-kernel:")
        end = workflow.index("  promote-native-kernels:")
        build = workflow[start:end]
        self.assertIn("ref: ${{ github.event.pull_request.head.sha || github.sha }}", build)
        self.assertIn("fetch-depth: 1", build)
        self.assertIn("fetch-tags: false", build)
        self.assertNotIn("fetch-depth: 0", build)
        self.assertIn("os/dev/login-registry-with-retry.sh", build)
        self.assertNotIn("docker/login-action", build)
        self.assertIn("if: ${{ always() }}", build)
        self.assertIn("docker logout code.sandbox.example", build)


if __name__ == "__main__":
    unittest.main()
