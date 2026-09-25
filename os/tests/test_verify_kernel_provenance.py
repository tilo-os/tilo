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
KERNEL = "code.sandbox.example/qos/tilo-kernel@sha256:" + "1" * 64
BASE = "code.sandbox.example/qos/fedora-bootc@sha256:" + "2" * 64
BUILDROOT = "code.sandbox.example/qos/tilo-kernel-buildroot@sha256:" + "3" * 64
EARLIER_RPMS = "code.sandbox.example/qos/rpms@sha256:" + "4" * 64
SOURCE = "git" + "+ssh://code.sandbox.example/qos/qos@" + "5" * 40


def provenance(*environments: str, architecture: str = ARCH) -> str:
    predicate = {
        "buildDefinition": {
            "buildType": "https://tilo.sh/buildtypes/kernel-rpm/v1",
            "externalParameters": {"architecture": architecture},
            "internalParameters": {"expectedReleaseDigest": KERNEL.split("@", 1)[1]},
            "resolvedDependencies": [{"uri": SOURCE}, {"uri": BASE}]
            + [{"uri": uri} for uri in environments],
        }
    }
    payload = base64.b64encode(json.dumps({"predicate": predicate}).encode()).decode()
    return json.dumps([{"payload": payload}])


@unittest.skipUnless(REGISTRY_ARCH, "native kernel verification needs x86_64 or aarch64")
class VerifyKernelProvenanceTests(unittest.TestCase):
    def run_verify(self, bound: tuple[str, ...], signed: tuple[str, ...], *,
                   kernel_signed: bool = True, architecture: str = ARCH,
                   registry_arch: str = REGISTRY_ARCH, arguments: tuple[str, ...] = (KERNEL,)):
        with tempfile.TemporaryDirectory() as temporary:
            bin_dir = Path(temporary)
            (bin_dir / "provenance.json").write_text(provenance(*bound, architecture=architecture))
            (bin_dir / "signed").write_text(
                "".join(ref + "\n" for ref in ((KERNEL,) if kernel_signed else ()) + signed))
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
                f"#!/usr/bin/env bash\nprintf '{{\"Architecture\":\"{registry_arch}\"}}'\n"
            )
            for tool in ("cosign", "skopeo"):
                (bin_dir / tool).chmod(0o755)
            environment = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}")
            return subprocess.run(
                ["bash", str(SCRIPT), *arguments],
                env=environment, capture_output=True, text=True,
            )

    def test_accepts_a_kernel_built_on_the_signed_buildroot(self):
        result = self.run_verify((BUILDROOT,), (BUILDROOT,))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")

    def test_accepts_a_kernel_built_before_the_buildroot_from_a_signed_snapshot(self):
        # Any signed RPM snapshot: there is no equality with the OS build's own.
        result = self.run_verify((EARLIER_RPMS,), (EARLIER_RPMS,))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("before the kernel buildroot", result.stderr)

    def test_rejects_an_unsigned_build_environment(self):
        for bound in ((BUILDROOT,), (EARLIER_RPMS,)):
            with self.subTest(bound=bound):
                result = self.run_verify(bound, ())
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("is not signed", result.stderr)

    def test_rejects_an_unsigned_kernel(self):
        result = self.run_verify((BUILDROOT,), (BUILDROOT,), kernel_signed=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("signature verification failed", result.stderr)

    def test_requires_the_kernel_identity_and_one_environment(self):
        for bound in ((), (BUILDROOT, "code.sandbox.example/qos/tilo-kernel-buildroot@sha256:" + "6" * 64)):
            with self.subTest(bound=bound):
                result = self.run_verify(bound, bound)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("does not bind", result.stderr)
        other = "aarch64" if ARCH == "x86_64" else "x86_64"
        result = self.run_verify((BUILDROOT,), (BUILDROOT,), architecture=other)
        self.assertNotEqual(result.returncode, 0)

    def test_rejects_a_foreign_registry_architecture(self):
        result = self.run_verify((BUILDROOT,), (BUILDROOT,), registry_arch="s390x")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected", result.stderr)

    def test_takes_only_the_kernel_reference(self):
        for arguments in ((), (KERNEL, BASE), ("code.sandbox.example/qos/tilo-kernel:latest",)):
            with self.subTest(arguments=arguments):
                self.assertNotEqual(self.run_verify((BUILDROOT,), (BUILDROOT,),
                                                    arguments=arguments).returncode, 0)


if __name__ == "__main__":
    unittest.main()
