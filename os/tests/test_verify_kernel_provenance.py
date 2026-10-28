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
KERNEL = "registry.tilo.so/tilo/tilo-kernel@sha256:" + "1" * 64
BASE = "registry.tilo.so/tilo/fedora-bootc@sha256:" + "2" * 64
BUILDROOT = "registry.tilo.so/tilo/tilo-kernel-buildroot@sha256:" + "3" * 64
EARLIER_RPMS = "registry.tilo.so/tilo/rpms@sha256:" + "4" * 64
SOURCE = "git" + "+ssh://code.tilo.so/tilo/tilo@" + "5" * 40


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

    def test_accepted_provenance(self):
        with self.subTest(phase="accepts_a_kernel_built_on_the_signed_buildroot"):
            result = self.run_verify((BUILDROOT,), (BUILDROOT,))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stderr, "")
        with self.subTest(phase="takes_only_the_kernel_reference"):
            for arguments in ((), (KERNEL, BASE), ("registry.tilo.so/tilo/tilo-kernel:latest",)):
                with self.subTest(arguments=arguments):
                    self.assertNotEqual(self.run_verify((BUILDROOT,), (BUILDROOT,),
                                                        arguments=arguments).returncode, 0)
        with self.subTest(phase="verifies_a_pinned_other_architecture"):
            other = "aarch64" if ARCH == "x86_64" else "x86_64"
            registry = "arm64" if other == "aarch64" else "amd64"
            result = self.run_verify((BUILDROOT,), (BUILDROOT,), architecture=other,
                                     registry_arch=registry,
                                     arguments=(KERNEL, "--architecture", other))
            self.assertEqual(result.returncode, 0, result.stderr)
            result = self.run_verify((BUILDROOT,), (BUILDROOT,), architecture=other,
                                     registry_arch=registry)
            self.assertNotEqual(result.returncode, 0)
        with self.subTest(phase="requires_the_kernel_identity_and_one_environment"):
            for bound in ((), (BUILDROOT, "registry.tilo.so/tilo/tilo-kernel-buildroot@sha256:" + "6" * 64)):
                with self.subTest(bound=bound):
                    result = self.run_verify(bound, bound)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("does not bind", result.stderr)
            other = "aarch64" if ARCH == "x86_64" else "x86_64"
            result = self.run_verify((BUILDROOT,), (BUILDROOT,), architecture=other)
            self.assertNotEqual(result.returncode, 0)

    def test_rejected_provenance(self):
        with self.subTest(phase="rejects_a_kernel_built_from_a_signed_rpm_snapshot"):
            result = self.run_verify((EARLIER_RPMS,), (EARLIER_RPMS,))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not bind", result.stderr)
        with self.subTest(phase="rejects_an_unsigned_buildroot"):
            result = self.run_verify((BUILDROOT,), ())
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("is not signed", result.stderr)
        with self.subTest(phase="rejects_an_unsigned_kernel"):
            result = self.run_verify((BUILDROOT,), (BUILDROOT,), kernel_signed=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("signature verification failed", result.stderr)
        with self.subTest(phase="rejects_a_foreign_registry_architecture"):
            result = self.run_verify((BUILDROOT,), (BUILDROOT,), registry_arch="s390x")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("expected", result.stderr)


KERNEL_SH = Path(__file__).resolve().parents[2] / "tools/ci/publish/kernel.sh"
PINS = Path(__file__).resolve().parents[1] / "kernel-buildroot-snapshot.env"


def pinned(arch: str) -> str:
    key = f"KERNEL_BUILDROOT_{arch.upper()}_REF="
    return next(line[len(key):] for line in PINS.read_text().splitlines() if line.startswith(key))


class CrossBuiltProvenanceTests(unittest.TestCase):
    """The predicate kernel.sh provenance writes for a cross-compiled aarch64 kernel names the
    x86_64 buildroot it ran FROM and the machine, and verify-kernel-provenance.sh accepts it."""

    def provenance_dir(self, temporary: Path, built: dict[str, tuple[str, str]]) -> Path:
        for arch, (built_on, buildroot) in built.items():
            out = temporary / arch
            out.mkdir()
            (out / "digest").write_text(KERNEL.split("@", 1)[1] + "\n")
            (out / "build-architecture").write_text(built_on + "\n")
            (out / "buildroot").write_text(buildroot + "\n")
        return temporary

    def write(self, temporary: Path, built: dict[str, tuple[str, str]]) -> subprocess.CompletedProcess:
        directory = self.provenance_dir(temporary, built)
        return subprocess.run(["bash", str(KERNEL_SH), "provenance", str(directory)],
                              capture_output=True, text=True)

    def test_cross_built_kernel_names_its_buildroot_and_machine(self):
        x86, arm = pinned("x86_64"), pinned("aarch64")
        with tempfile.TemporaryDirectory() as temporary:
            result = self.write(Path(temporary), {"x86_64": ("x86_64", x86), "aarch64": ("x86_64", x86)})
            self.assertEqual(result.returncode, 0, result.stderr)
            statement = json.loads((Path(temporary) / "aarch64/provenance.json").read_text())
        definition = statement["buildDefinition"]
        self.assertEqual(definition["externalParameters"], {"architecture": "aarch64"})
        self.assertEqual(definition["internalParameters"]["buildArchitecture"], "x86_64")
        self.assertEqual([d["uri"] for d in definition["resolvedDependencies"]][1:], [x86])
        # As the OS build verifies the published kernel: signature, provenance, buildroot.
        definition["resolvedDependencies"][0]["uri"] = SOURCE
        payload = base64.b64encode(json.dumps({"predicate": statement}).encode()).decode()
        with tempfile.TemporaryDirectory() as temporary:
            bin_dir = Path(temporary)
            (bin_dir / "provenance.json").write_text(json.dumps([{"payload": payload}]))
            (bin_dir / "signed").write_text(f"{KERNEL}\n{x86}\n")
            (bin_dir / "cosign").write_text(
                "#!/usr/bin/env bash\n"
                f'dir="{bin_dir}"\n'
                'ref="${@: -1}"\n'
                'case "$1" in\n'
                '  verify) grep -Fxq -- "$ref" "$dir/signed" ;;\n'
                '  verify-attestation) cat "$dir/provenance.json" ;;\n'
                "  *) exit 2 ;;\n"
                "esac\n")
            (bin_dir / "skopeo").write_text("#!/usr/bin/env bash\nprintf '{\"Architecture\":\"arm64\"}'\n")
            for tool in ("cosign", "skopeo"):
                (bin_dir / tool).chmod(0o755)
            result = subprocess.run(["bash", str(SCRIPT), KERNEL, "--architecture", "aarch64"],
                                    env=dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}"),
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
        with self.subTest(phase="native builds keep their own buildroots"):
            with tempfile.TemporaryDirectory() as temporary:
                result = self.write(Path(temporary), {"x86_64": ("x86_64", x86), "aarch64": ("aarch64", arm)})
                self.assertEqual(result.returncode, 0, result.stderr)
                statement = json.loads((Path(temporary) / "aarch64/provenance.json").read_text())
                self.assertEqual(statement["buildDefinition"]["internalParameters"]["buildArchitecture"], "aarch64")
                self.assertEqual(statement["buildDefinition"]["resolvedDependencies"][1]["uri"], arm)

    def test_provenance_refuses_what_the_build_could_not_have_used(self):
        x86, arm = pinned("x86_64"), pinned("aarch64")
        for built in ({"x86_64": ("x86_64", x86), "aarch64": ("x86_64", arm)},
                      {"x86_64": ("x86_64", x86), "aarch64": ("aarch64", x86)},
                      {"x86_64": ("aarch64", arm), "aarch64": ("aarch64", arm)},
                      {"x86_64": ("x86_64", x86),
                       "aarch64": ("x86_64", "registry.tilo.so/tilo/tilo-kernel-buildroot@sha256:" + "6" * 64)}):
            with self.subTest(built=built), tempfile.TemporaryDirectory() as temporary:
                result = self.write(Path(temporary), built)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((Path(temporary) / "aarch64/provenance.json").exists()
                                 and (Path(temporary) / "x86_64/provenance.json").exists())


if __name__ == "__main__":
    unittest.main()
