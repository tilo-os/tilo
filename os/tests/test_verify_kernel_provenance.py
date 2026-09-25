"""Executable checks for os/dev/verify-kernel-provenance.sh with stub cosign/skopeo."""
import base64
import json
import os
import platform
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "dev/verify-kernel-provenance.sh"
ARCH = platform.machine()
REGISTRY_ARCH = {"x86_64": "amd64", "aarch64": "arm64"}.get(ARCH)
KERNEL = "code.podesta.ai/qos/tilo-kernel@sha256:" + "1" * 64
BASE = "code.podesta.ai/qos/fedora-bootc@sha256:" + "2" * 64
CURRENT_RPMS = "code.podesta.ai/qos/rpms@sha256:" + "3" * 64
EARLIER_RPMS = "code.podesta.ai/qos/rpms@sha256:" + "4" * 64
SOURCE = "git" + "+ssh://code.podesta.ai/qos/qos@" + "5" * 40


def provenance(*rpms: str) -> str:
    predicate = {
        "buildDefinition": {
            "buildType": "https://tilo.sh/buildtypes/kernel-rpm/v1",
            "externalParameters": {"architecture": ARCH},
            "internalParameters": {"expectedReleaseDigest": KERNEL.split("@", 1)[1]},
            "resolvedDependencies": [{"uri": SOURCE}, {"uri": BASE}]
            + [{"uri": uri} for uri in rpms],
        }
    }
    payload = base64.b64encode(json.dumps({"predicate": predicate}).encode()).decode()
    return json.dumps([{"payload": payload}])


@unittest.skipUnless(REGISTRY_ARCH, "native kernel verification needs x86_64 or aarch64")
class VerifyKernelProvenanceTests(unittest.TestCase):
    def run_verify(self, binding: str, bound: tuple[str, ...], signed: tuple[str, ...]):
        with tempfile.TemporaryDirectory() as temporary:
            bin_dir = Path(temporary)
            (bin_dir / "provenance.json").write_text(provenance(*bound))
            (bin_dir / "signed").write_text("".join(ref + "\n" for ref in (KERNEL, *signed)))
            (bin_dir / "cosign").write_text(
                "#!/usr/bin/env bash\n"
                f'dir="{bin_dir}"\n'
                'ref="${@: -1}"\n'
                'case "$1" in\n'
                '  verify) grep -Fxq -- "$ref" "$dir/signed" ;;\n'
                '  verify-attestation) cat "$dir/provenance.json" ;;\n'
                "  *) exit 2 ;;\n"
                "esac\n"
            )
            (bin_dir / "skopeo").write_text(
                f"#!/usr/bin/env bash\nprintf '{{\"Architecture\":\"{REGISTRY_ARCH}\"}}'\n"
            )
            for tool in ("cosign", "skopeo"):
                (bin_dir / tool).chmod(0o755)
            environment = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}")
            return subprocess.run(
                ["bash", str(SCRIPT), KERNEL, BASE, CURRENT_RPMS, binding],
                env=environment, capture_output=True, text=True,
            )

    def test_exact_accepts_the_current_snapshot(self):
        result = self.run_verify("exact", (CURRENT_RPMS,), ())
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_exact_rejects_an_earlier_signed_snapshot(self):
        result = self.run_verify("exact", (EARLIER_RPMS,), (EARLIER_RPMS,))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("instead of", result.stderr)

    def test_integration_accepts_an_earlier_signed_snapshot(self):
        result = self.run_verify("signed-snapshot", (EARLIER_RPMS,), (EARLIER_RPMS,))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("earlier signed RPM snapshot", result.stderr)

    def test_integration_rejects_an_unsigned_earlier_snapshot(self):
        result = self.run_verify("signed-snapshot", (EARLIER_RPMS,), ())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("is not signed", result.stderr)

    def test_integration_still_requires_the_kernel_identity(self):
        result = self.run_verify("signed-snapshot", (), ())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not bind", result.stderr)

    def test_unknown_binding_is_rejected(self):
        result = self.run_verify("any", (CURRENT_RPMS,), ())
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
