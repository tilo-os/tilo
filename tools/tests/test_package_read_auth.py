"""Exercise the workflow's actual auth selection without network or real secrets."""
import os
from pathlib import Path
import subprocess
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / ".forgejo/workflows/build-image.yml").read_text()


def auth_script(step):
    region = SOURCE.split("      - name: " + step + "\n", 1)[1]
    region = region.split("\n      - name:", 1)[0]
    start = region.index('          if [[ -n ${ACTIONS_ID_TOKEN_REQUEST_URL:-} ]]')
    end = region.index('          case "$', start)
    return textwrap.dedent(region[start:end])


class PackageReadAuth(unittest.TestCase):
    def run_selection(self, step, allowed, token="fixture-package-token", oidc=False):
        environment = os.environ.copy()
        for name in ("ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN"):
            environment.pop(name, None)
        environment.update(DEV_BUILD=allowed, PACKAGE_FALLBACK_ALLOWED=allowed, PACKAGE_TOKEN=token)
        if oidc:
            environment.update(ACTIONS_ID_TOKEN_REQUEST_URL="https://code.tilo.so/fixture?x=1",
                               ACTIONS_ID_TOKEN_REQUEST_TOKEN="fixture-broker-token",
                               COLLECTION_AUDIENCE="fixture", PAYLOAD_AUDIENCE="fixture")
        # The broker fixture exercises branch selection only. JWT issuance and
        # package authorization are proved by the focused native CI receipt.
        script = "set -euo pipefail\ncurl() { printf '%s' '{\"value\":\"fixture-oidc-token\"}'; }\n"
        script += auth_script(step) + '\nprintf "selected=%s\\n" "$auth"\n'
        return subprocess.run(["bash", "-c", script], env=environment, text=True, capture_output=True)

    def test_direct_secret_reaches_only_the_allowed_fallback(self):
        for step in ("Materialize the pinned app collection", "Materialize the pinned desktop payload"):
            with self.subTest(step=step):
                result = self.run_selection(step, "true")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("selected=token fixture-package-token", result.stdout)
                for allowed, token in (("false", "fixture-package-token"), ("true", "")):
                    denied = self.run_selection(step, allowed, token)
                    self.assertNotEqual(denied.returncode, 0)
                    self.assertNotIn("selected=", denied.stdout)

    def test_oidc_still_takes_precedence_outside_the_fallback(self):
        for step in ("Materialize the pinned app collection", "Materialize the pinned desktop payload"):
            with self.subTest(step=step):
                result = self.run_selection(step, "false", oidc=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("selected=Bearer fixture-oidc-token", result.stdout)
                self.assertNotIn("selected=token", result.stdout)


if __name__ == "__main__":
    unittest.main()
