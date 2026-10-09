#!/usr/bin/env python3
"""Exercise the installed selector and updater's actual command/gate boundaries."""
import base64
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("board_successor", ROOT / "os/usr/libexec/tilo-board-successor.py")
selector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(selector)
DIGEST = "sha256:" + "a" * 64
SOURCE = "b" * 40
NOW = 1000


def envelope(predicate, kind):
    statement = {"predicateType": kind, "subject": [{"name": selector.REPOSITORY,
                 "digest": {"sha256": DIGEST[7:]}}], "predicate": predicate}
    return json.dumps([{"payload": base64.b64encode(json.dumps(statement).encode()).decode()}])


def documents():
    provenance = {"buildDefinition": {
        "buildType": "https://tilo.sh/buildtypes/bootc-os/v1",
        "internalParameters": {"expectedReleaseDigest": DIGEST},
        "externalParameters": {"hardwareProfile": "lenovo-83ed", "architecture": "aarch64",
                               "generation": 2, "hardwareDefinitionSha256": "c" * 64,
                               "firmwareManifestSha256": "d" * 64},
        "resolvedDependencies": [{"uri": f"git+ssh://code.tilo.so/tilo/tilo@{SOURCE}"},
                                 {"uri": "code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:" + "e" * 64}]},
        "runDetails": {"builder": {"id": "https://code.tilo.so/tilo/forgejo/actions/bootc-release-v1"},
                       "byproducts": [{"name": "independent-rebuild",
                                       "uri": "urn:tilo:verification:manifest-digest-equal"}]}}
    freshness = {"schema": selector.FRESHNESS, "repository": selector.REPOSITORY,
                 "digest": DIGEST, "sourceRevision": SOURCE, "generation": 2,
                 "issuedAt": NOW - 100, "expiresAt": NOW + 1000}
    signature = json.dumps([{"critical": {"image": {"docker-manifest-digest": DIGEST},
                              "identity": {"docker-reference": selector.REPOSITORY}}}])
    return provenance, freshness, signature


class SuccessorTests(unittest.TestCase):
    def test_exact_signature_source_generation_and_freshness_select_immutable_digest(self):
        provenance, freshness, signature = documents()
        calls = []
        def runner(*args):
            calls.append(args)
            if args[0] == "/usr/bin/skopeo":
                return DIGEST
            if args[1] == "verify":
                return signature
            kind = args[args.index("--type") + 1]
            return envelope(provenance, "https://slsa.dev/provenance/v1") if kind == "slsaprovenance1" else envelope(freshness, selector.FRESHNESS)
        self.assertEqual(selector.select(1, NOW, runner),
                         (f"{selector.REPOSITORY}@{DIGEST}", 2, SOURCE, NOW + 1000))
        self.assertEqual(calls[0][-1], "docker://code.tilo.so/tilo/tilo:lenovo-83ed-latest")
        self.assertEqual(calls[-2][-1], "code.tilo.so/tilo/tilo:lenovo-83ed-gen2")
        self.assertEqual(calls[-1][-1], f"code.tilo.so/tilo/tilo:lenovo-83ed-{SOURCE}")
        self.assertTrue(all(call[-1] == f"{selector.REPOSITORY}@{DIGEST}"
                            for call in calls[1:4]))

    def test_wrong_profile_architecture_digest_source_and_ambiguous_provenance_refuse(self):
        original, _, _ = documents()
        mutations = [
            lambda p: p["buildDefinition"]["externalParameters"].update(hardwareProfile="foreign"),
            lambda p: p["buildDefinition"]["externalParameters"].update(architecture="x86_64"),
            lambda p: p["buildDefinition"]["internalParameters"].update(expectedReleaseDigest="sha256:" + "f" * 64),
            lambda p: p["buildDefinition"]["externalParameters"].update(generation=True),
            lambda p: p["buildDefinition"]["resolvedDependencies"].append({"uri": "git+ssh://code.tilo.so/tilo/tilo@" + "f" * 40}),
            lambda p: p["runDetails"].update(byproducts=[]),
            lambda p: p["buildDefinition"]["externalParameters"].update(hardwareDefinitionSha256=""),
        ]
        for mutation in mutations:
            predicate = copy.deepcopy(original)
            mutation(predicate)
            with self.subTest(predicate=predicate), self.assertRaises(ValueError):
                selector.provenance(envelope(predicate, "https://slsa.dev/provenance/v1"), DIGEST)
        rows = json.loads(envelope(original, "https://slsa.dev/provenance/v1"))
        with self.assertRaises(ValueError):
            selector.provenance(json.dumps(rows + rows), DIGEST)

    def test_expiry_repository_source_generation_and_oversized_lifetime_refuse(self):
        _, original, _ = documents()
        for key, value in [("repository", "registry.tilo.so/tilo/tilo"),
                           ("sourceRevision", "f" * 40), ("generation", 3),
                           ("expiresAt", NOW), ("issuedAt", NOW + 1),
                           ("expiresAt", NOW + 604801), ("generation", True)]:
            predicate = dict(original, **{key: value})
            with self.subTest(key=key), self.assertRaises(ValueError):
                selector.freshness(envelope(predicate, selector.FRESHNESS), DIGEST, SOURCE, 2, NOW)

    def test_monotonic_release_counter_bound_and_signature_refusal(self):
        provenance, fresh, signature = documents()
        def runner(*args):
            if args[0] == "/usr/bin/skopeo":
                return DIGEST
            if args[1] == "verify":
                return signature
            kind = args[args.index("--type") + 1]
            return envelope(provenance, "https://slsa.dev/provenance/v1") if kind == "slsaprovenance1" else envelope(fresh, selector.FRESHNESS)
        with self.assertRaises(ValueError):
            selector.select(3, NOW, runner)
        provenance["buildDefinition"]["externalParameters"]["generation"] = 66
        with self.assertRaises(ValueError):
            selector.select(1, NOW, runner)
        with self.assertRaises(ValueError):
            selector.signature(signature.replace(DIGEST, "sha256:" + "f" * 64), DIGEST)
        with self.assertRaises(ValueError):
            selector.signature(signature.replace(selector.REPOSITORY, "foreign/repository"), DIGEST)

    def test_child_output_deadline_and_failure_are_real_bounded_refusals(self):
        with patch.object(selector, "MAX_OUTPUT", 1024):
            with self.assertRaises(ValueError):
                selector.run(sys.executable, "-c", "import sys; sys.stdout.write('x'*4096)")
        with self.assertRaises(ValueError):
            selector.run(sys.executable, "-c", "import time; time.sleep(1)", timeout=0.02)
        with self.assertRaises(ValueError):
            selector.run(sys.executable, "-c", "raise SystemExit(7)")
        self.assertEqual(selector.run(sys.executable, "-c", "print('ready')"), "ready\n")

    def test_existing_bootc_auth_precedence_private_snapshot_and_cleanup(self):
        class Inputs:
            @staticmethod
            def trusted_path(root, relative, regular):
                # Fixture root; production uses the image's real trusted_path.
                return root / relative
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = root / "run/ostree"
            persistent = root / "etc/ostree"
            runtime.mkdir(parents=True)
            persistent.mkdir(parents=True)
            for path, fixture in [(runtime / "auth.json", b'{"auths": {"test": {"auth": "fixture-runtime"}}}'),
                                  (persistent / "auth.json", b'{"auths": {"test": {"auth": "fixture-persistent"}}}')]:
                path.write_bytes(fixture)
                path.chmod(0o600)
            with patch.object(selector, "run", return_value="verified") as child:
                with selector.registry_runner(Inputs, root, directory, os.getuid()) as runner:
                    self.assertEqual(runner("/usr/libexec/tilo-cosign", "verify"), "verified")
                    environment = child.call_args.kwargs["environment"]
                    snapshot = Path(environment["REGISTRY_AUTH_FILE"])
                    self.assertEqual(snapshot.read_bytes(), (runtime / "auth.json").read_bytes())
                    self.assertEqual(snapshot.stat().st_mode & 0o777, 0o600)
                    self.assertEqual(snapshot.parent.stat().st_mode & 0o777, 0o700)
                self.assertFalse(snapshot.exists())
            (runtime / "auth.json").chmod(0o644)
            with self.assertRaises(ValueError):
                with selector.registry_runner(Inputs, root, directory, os.getuid()):
                    self.fail("exposed bootc credentials were accepted")

    def test_updater_uses_exact_switch_and_verifies_before_activation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            calls = root / "calls"
            def script(name, body):
                path = root / name
                path.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + body)
                path.chmod(0o755)
                return str(path)
            successor = script("successor", f'''if [ "$1" = applicable ]; then exit 0; fi
printf '%s\\n' '{selector.REPOSITORY}@{DIGEST}' 2 '{SOURCE}' 2000
''')
            bootc = script("bootc", f'''printf 'bootc %s\\n' "$*" >>'{calls}'
''')
            gate = script("gate", f'''if [ "$1" = --trusted-time ]; then printf '1000000000000\\n'; exit 0; fi
[ "$1" = --successor ] && [ "$2" = '{DIGEST}' ] && [ "$3" = 2 ]
[ "$4" = 2000 ] && [ -e "/proc/self/fd/$5" ]
if flock -n "$6" true; then echo 'parent lock lost' >&2; exit 1; fi
printf 'gate\\n' >>'{calls}'
[ "${{REFUSE_GATE:-0}}" != 1 ]
''')
            environment = dict(os.environ, TILO_IMAGE_TRUST_BOOTC=bootc,
                               TILO_IMAGE_TRUST_GATE=gate, TILO_IMAGE_TRUST_SUCCESSOR=successor,
                               TILO_IMAGE_TRUST_UPDATE_LOCK=str(root / "update.lock"),
                               TILO_IMAGE_TRUST_DIR=str(root / "trust"))
            command = ["bash", str(ROOT / "os/usr/libexec/tilo-verified-update"), "--quiet"]
            result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(calls.read_text().splitlines(), [
                f"bootc switch --download-only --quiet {selector.REPOSITORY}@{DIGEST}",
                "gate", "bootc switch --from-downloaded --quiet"])
            calls.unlink()
            environment["REFUSE_GATE"] = "1"
            result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(len(calls.read_text().splitlines()), 2)


if __name__ == "__main__":
    unittest.main()
