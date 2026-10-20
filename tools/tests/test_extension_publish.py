"""tools/extensions/publish.sh against a fake package registry and a fake Cosign.

`curl` and `cosign` are replaced on PATH: curl serves a generic-package store
from a scratch directory (PUT refuses an existing file, as Forgejo does), and
cosign "signs" with the blob's SHA-256 so a signature over other bytes fails.
"""

import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
import importlib.util
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools" / "ci"))
import t_matrix  # noqa: E402
PUBLISH = REPO / "tools/extensions/publish.sh"
BASE_URL = "https://code.example/api/packages/tilo/generic"
FINGERPRINT = "cd" * 32
PACKAGE = f"/api/packages/tilo/generic/tilo-extension-media-codecs/x86_64-{FINGERPRINT}"
SPEC = importlib.util.spec_from_file_location("publish_resolver", REPO / "tools/extensions/resolve.py")
resolver = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = resolver
SPEC.loader.exec_module(resolver)

FAKE_CURL = textwrap.dedent("""\
    #!/usr/bin/env python3
    import os, sys
    from pathlib import Path
    store = Path(os.environ["FAKE_STORE"])
    args = sys.argv[1:]
    with_value = {"--retry", "--output", "-o", "--write-out", "-w", "--header", "-H",
                  "--request", "-X", "--upload-file", "-T", "--data"}
    options, urls, i = {}, [], 0
    while i < len(args):
        arg = args[i]
        if arg in with_value:
            options[arg.lstrip("-")[:1] if len(arg) == 2 else arg] = args[i + 1]
            i += 2
            continue
        if arg.startswith("-"):
            options[arg] = True
        else:
            urls.append(arg)
        i += 1
    get = lambda *names: next((options[n] for n in names if n in options), None)
    url = urls[0]
    assert url.startswith("https://code.example/"), url
    path = store / url.split("://", 1)[1].split("/", 1)[1]
    upload = get("--upload-file", "T")
    method = get("--request", "X") or ("PUT" if upload else "GET")
    body = b""
    if method == "GET":
        status = 200 if path.is_file() else 404
        body = path.read_bytes() if status == 200 else b"not found"
    elif method == "PUT":
        if path.exists():
            status = 409
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(Path(upload).read_bytes())
            status = 201
    elif method == "DELETE":
        status = 204 if path.is_file() else 404
        if status == 204:
            path.unlink()
    with open(store.parent / "requests.log", "a") as log:
        log.write(f"{method} {url} {status}\\n")
    output = get("--output", "o")
    if output and output != "/dev/null":
        Path(output).write_bytes(body)
    fmt = get("--write-out", "w")
    if fmt:
        sys.stdout.write(fmt.replace("%{http_code}", str(status)).replace("%{redirect_url}", ""))
    if status >= 400 and ("--fail" in options or "--fail-with-body" in options):
        sys.exit(22)
    """)

FAKE_COSIGN = textwrap.dedent("""\
    #!/usr/bin/env python3
    import hashlib, sys
    from pathlib import Path
    args = sys.argv[1:]
    def value(name):
        for i, arg in enumerate(args):
            if arg == name:
                return args[i + 1]
            if arg.startswith(name + "="):
                return arg.split("=", 1)[1]
    blob = Path(args[-1]).read_bytes()
    expected = "sig:" + hashlib.sha256(blob).hexdigest()
    if args[0] == "sign-blob":
        assert value("--key").startswith("hashivault://"), args
        assert "--tlog-upload=false" in args and "--yes" in args, args
        Path(value("--output-signature")).write_text(expected)
    elif args[0] == "verify-blob" and value("--bundle"):
        # A certified signature: the operational certificate under the root.
        assert value("--certificate-chain").endswith("keys/release/root.crt"), args
        assert value("--certificate-identity") == "extension-manifests@signing.tilo.so", args
        assert value("--certificate-oidc-issuer").startswith("https://signing.tilo.so/"), args
        bundle = "bundle:" + hashlib.sha256(blob).hexdigest()
        sys.exit(0 if Path(value("--bundle")).read_text() == bundle else 1)
    elif args[0] == "verify-blob":
        assert value("--key").endswith("keys/extensions.pub"), args
        sys.exit(0 if Path(value("--signature")).read_text() == expected else 1)
    else:
        sys.exit(2)
    """)


def manifest(revision=20261003120000, sha256="ef" * 32, image_byte="ab"):
    return {
        "type": "tilo.system-extension.sysext-manifest", "schema": 1, "id": "media-codecs",
        "revision": revision, "created": "2026-10-03T12:00:00Z",
        "base": {"os_id": "fedora", "version_id": "44", "architecture": "x86_64",
                 "packages_sha256": FINGERPRINT,
                 "image": "registry.tilo.so/tilo/tilo@sha256:" + image_byte * 32},
        "parts": [{"name": "tilo-codecs", "format": "rpm", "state": "available", "message": "",
                   "requires_base": {"gstreamer1": "0:1.28.7-1.fc44"},
                   "artifacts": [{"package": "x-1-1.fc44.x86_64",
                                  "urls": ["https://download1.rpmfusion.org/x"], "size": 3,
                                  "sha256": sha256, "publisher": "RPM Fusion", "license": "MIT"}]}],
    }


class Publish(unittest.TestCase):
    script = PUBLISH

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.store = self.root / "store"
        self.store.mkdir()
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        for name, text in (("curl", FAKE_CURL), ("cosign", FAKE_COSIGN)):
            path = bin_dir / name
            path.write_text(text.replace("#!/usr/bin/env python3", f"#!{sys.executable}", 1))
            path.chmod(path.stat().st_mode | stat.S_IXUSR)
        (self.root / "runner").mkdir()
        self.env = {
            "PATH": f"{bin_dir}:{os.environ['PATH']}",
            "HOME": str(self.root),
            "FAKE_STORE": str(self.store),
            "RUNNER_TEMP": str(self.root / "runner"),
            "COSIGN_KEY_REF": "hashivault://tilo-test",
            "TILO_PACKAGE_TOKEN": "token",
            "TILO_PACKAGE_PUBLIC_BASE_URL": BASE_URL,
        }

    def publish(self, document, sign_only=False, **env):
        path = self.root / "manifest.json"
        path.write_bytes((json.dumps(document, indent=2) + "\n").encode())
        log = self.root / "requests.log"
        log.unlink(missing_ok=True)
        mode = ["--sign-only"] if sign_only else []
        result = subprocess.run(["bash", str(self.script), *mode, str(path)], capture_output=True, text=True,
                                env={**self.env, **env}, cwd=REPO)
        requests = log.read_text().splitlines() if log.exists() else []
        return result, requests

    def test_sign_only_produces_verified_signature_without_registry_or_activation(self):
        document = manifest()
        self.env.pop("TILO_PACKAGE_TOKEN")
        self.env.pop("TILO_PACKAGE_PUBLIC_BASE_URL")
        result, requests = self.publish(document, sign_only=True,
            TILO_EXPECTED_EXTENSION_CONTENT_SHA256=resolver.qualification_digest(document))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(requests, [])
        source = self.root / "manifest.json"
        self.assertEqual(source.with_suffix(".json.sig").read_text(), self.signature_of(source.read_bytes()))
        self.assertEqual(list(self.store.iterdir()), [])

    def test_changed_or_missing_reviewed_hash_cannot_sign_only(self):
        for expected in ("", "00" * 32):
            result, requests = self.publish(manifest(), sign_only=True,
                TILO_EXPECTED_EXTENSION_CONTENT_SHA256=expected)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(requests, [])
            self.assertFalse((self.root / "manifest.json.sig").exists())

    def stored(self, name):
        return (self.store / PACKAGE.lstrip("/") / name)

    def signature_of(self, data: bytes) -> str:
        return "sig:" + hashlib.sha256(data).hexdigest()

    def test_first_publication_writes_the_revision_and_the_newest_pair(self):
        result, requests = self.publish(manifest())
        self.assertEqual(result.returncode, 0, result.stderr)
        files = sorted(p.name for p in self.stored("").iterdir())
        self.assertEqual(files, ["manifest-r20261003120000.json", "manifest-r20261003120000.json.sig",
                                 "manifest.json", "manifest.json.sig"])
        data = self.stored("manifest.json").read_bytes()
        self.assertEqual(self.stored("manifest-r20261003120000.json").read_bytes(), data)
        self.assertEqual(self.stored("manifest.json.sig").read_text(), self.signature_of(data))
        puts = [r.split()[1].rsplit("/", 1)[1] for r in requests if r.startswith("PUT")]
        # The newest manifest is written before its signature.
        self.assertLess(puts.index("manifest.json"), puts.index("manifest.json.sig"))

    def test_an_unchanged_resolution_publishes_nothing(self):
        self.assertEqual(self.publish(manifest())[0].returncode, 0)
        result, requests = self.publish(manifest(revision=20261004120000, image_byte="01"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("already publishes these artifacts", result.stdout)
        self.assertEqual([r for r in requests if not r.startswith("GET")], [])
        self.assertFalse(self.stored("manifest-r20261004120000.json").exists())

    def test_a_changed_resolution_adds_a_revision_and_replaces_the_newest(self):
        self.assertEqual(self.publish(manifest())[0].returncode, 0)
        result, _ = self.publish(manifest(revision=20261004120000, sha256="aa" * 32))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.stored("manifest-r20261003120000.json").is_file())
        self.assertTrue(self.stored("manifest-r20261004120000.json").is_file())
        newest = json.loads(self.stored("manifest.json").read_bytes())
        self.assertEqual(newest["revision"], 20261004120000)
        self.assertEqual(self.stored("manifest.json.sig").read_text(),
                         self.signature_of(self.stored("manifest.json").read_bytes()))

    def test_a_revision_never_goes_backwards(self):
        self.assertEqual(self.publish(manifest(revision=20261004120000))[0].returncode, 0)
        result, requests = self.publish(manifest(revision=20261003120000, sha256="aa" * 32))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("is not newer than the published 20261004120000", result.stderr)
        self.assertEqual([r for r in requests if not r.startswith("GET")], [])

    def test_a_newest_manifest_without_a_valid_signature_is_replaced(self):
        self.assertEqual(self.publish(manifest())[0].returncode, 0)
        self.stored("manifest.json.sig").write_text("sig:forged")
        result, _ = self.publish(manifest(revision=20261004120000))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("has no valid signature; replacing it", result.stdout)
        self.assertEqual(json.loads(self.stored("manifest.json").read_bytes())["revision"], 20261004120000)

    def test_an_unpublishable_manifest_makes_no_request(self):
        broken = manifest()
        broken["parts"][0]["artifacts"][0]["urls"] = ["http://download1.rpmfusion.org/x"]
        result, requests = self.publish(broken)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(requests, [])

    def test_only_a_public_https_package_base(self):
        for base in ("http://code.example/api/packages/tilo/generic",
                     "http://forgejo.forgejo.svc.cluster.local:3000/api/packages/tilo/generic"):
            result, requests = self.publish(manifest(), TILO_PACKAGE_PUBLIC_BASE_URL=base)
            self.assertNotEqual(result.returncode, 0, base)
            self.assertEqual(requests, [])

    def test_only_a_kms_key_signs(self):
        result, _ = self.publish(manifest(), COSIGN_KEY_REF="cosign.key")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.stored("manifest.json").exists())


# What tools/signing/sign-blob-certified.sh does, minus KMS and Rekor: the bare
# signature the fake cosign accepts, and a bundle naming the same bytes.
FAKE_CERTIFIED = textwrap.dedent("""\
    #!/usr/bin/env bash
    set -euo pipefail
    [[ $1 == extension-manifests && $2 == */keys/operational/extension-manifests.crt ]]
    digest=$(sha256sum "$3" | cut -d' ' -f1)
    printf 'sig:%s' "$digest" > "$4.sig"
    printf 'bundle:%s' "$digest" > "$4.bundle"
    """)


class CertifiedPublish(Publish):
    """After the root ceremony: a scratch copy of the repository with a release
    root and an operational certificate, where certified signing is faked."""

    def setUp(self):
        super().setUp()
        tree = self.root / "repo"
        for relative in ("tools/extensions/publish.sh", "tools/extensions/resolve.py",
                         "tools/upload_immutable_packages.sh", "os/dev/configure-cosign-kms.sh",
                         "keys/extensions.pub", "keys/release/trust.env"):
            (tree / relative).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(REPO / relative, tree / relative)
        (tree / "keys/release/root.crt").write_text("root fixture\n")
        (tree / "keys/operational").mkdir()
        (tree / "keys/operational/extension-manifests.crt").write_text("certificate fixture\n")
        (tree / "tools/signing").mkdir()
        (tree / "tools/signing/sign-blob-certified.sh").write_text(FAKE_CERTIFIED)
        self.tree = tree
        self.script = tree / "tools/extensions/publish.sh"

    def bundle_of(self, data: bytes) -> str:
        return "bundle:" + hashlib.sha256(data).hexdigest()

    def test_first_publication_writes_the_revision_and_the_newest_pair(self):
        result, requests = self.publish(manifest())
        self.assertEqual(result.returncode, 0, result.stderr)
        files = sorted(p.name for p in self.stored("").iterdir())
        self.assertEqual(files, ["manifest-r20261003120000.json", "manifest-r20261003120000.json.bundle",
                                 "manifest-r20261003120000.json.sig", "manifest.json", "manifest.json.bundle",
                                 "manifest.json.sig"])
        data = self.stored("manifest.json").read_bytes()
        self.assertEqual(self.stored("manifest.json.bundle").read_text(), self.bundle_of(data))
        self.assertEqual(self.stored("manifest-r20261003120000.json.bundle").read_text(), self.bundle_of(data))
        puts = [r.split()[1].rsplit("/", 1)[1] for r in requests if r.startswith("PUT")]
        self.assertLess(puts.index("manifest.json"), puts.index("manifest.json.bundle"))

    def test_sign_only_produces_verified_signature_without_registry_or_activation(self):
        document = manifest()
        self.env.pop("TILO_PACKAGE_TOKEN")
        self.env.pop("TILO_PACKAGE_PUBLIC_BASE_URL")
        result, requests = self.publish(document, sign_only=True,
            TILO_EXPECTED_EXTENSION_CONTENT_SHA256=resolver.qualification_digest(document))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(requests, [])
        source = self.root / "manifest.json"
        self.assertEqual(source.with_suffix(".json.bundle").read_text(), self.bundle_of(source.read_bytes()))

    def test_a_publication_from_before_the_root_is_certified(self):
        # Published for key-pinned images only: the bundle images with the root
        # need is missing, so the same content is published again.
        self.assertEqual(self.publish(manifest())[0].returncode, 0)
        self.stored("manifest.json.bundle").unlink()
        result, _ = self.publish(manifest(revision=20261004120000))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("has no valid signature; replacing it", result.stdout)
        self.assertEqual(self.stored("manifest.json.bundle").read_text(),
                         self.bundle_of(self.stored("manifest.json").read_bytes()))

    def test_a_root_without_an_operational_certificate_publishes_nothing(self):
        (self.tree / "keys/operational/extension-manifests.crt").unlink()
        result, requests = self.publish(manifest())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("run the ceremony", result.stderr)
        self.assertEqual(requests, [])


class SigningAuthority(unittest.TestCase):
    """The extension-manifest role trusts tilo/tilo main with one OIDC audience;
    AWS cannot tell checks apart, so only the manifest publisher may request it."""

    def test_sign_only_planner_rejects_ambiguous_or_unpinned_inputs(self):
        script = REPO / "tools/ci/publish/extension-manifests.sh"
        arch = os.uname().machine
        digest = "registry.tilo.so/tilo/tilo-ci@sha256:" + "ab" * 32
        for images, expected, publish in [("", "ef" * 32, "true"),
                                          ("registry.tilo.so/tilo/tilo-ci:latest", "ef" * 32, "true"),
                                          (f"{digest} {digest}", "ef" * 32, "true"), (digest, "malformed", "true"),
                                          (digest, "", "false")]:
            with self.subTest(images=images, expected=expected, publish=publish):
                with tempfile.TemporaryDirectory() as root:
                    sentinel = Path(root) / "skopeo-called"
                    for tool, body in (("skopeo", f"touch '{sentinel}'\nexit 1\n"), ("dnf", "exit 0\n")):
                        fake = Path(root) / tool
                        fake.write_text(f"#!/bin/sh\n{body}")
                        fake.chmod(0o755)
                    out = Path(root) / "out"
                    result = subprocess.run(["bash", str(script), "resolve", arch, str(out)],
                        capture_output=True, text=True,
                        env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}", "TMPDIR": root,
                             "T_INPUT_IMAGES": images, "T_INPUT_EXPECTED_CONTENT_SHA256": expected,
                             "T_INPUT_PUBLISH": publish})
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("qualification requires one immutable image", result.stderr)
                    self.assertFalse(sentinel.exists(), "invalid qualification must not pull an image")
                    self.assertFalse(out.exists())

    def test_only_the_manifest_publisher_requests_the_extension_audience(self):
        checks = t_matrix.load(REPO)["check"]
        users = sorted(c["name"] for c in checks if "tilo-extension-manifests" in c.get("id_token", []))
        self.assertEqual(users, ["publish-extension-manifests"])
        (publisher,) = (c for c in checks if c["name"] == "publish-extension-manifests")
        env = publisher["env"]
        self.assertEqual(env["TILO_SIGNING_AUDIENCE"], "tilo-extension-manifests")
        self.assertTrue(env["COSIGN_KEY_REF"].endswith(":alias/tilo-extension-manifests"), env)
        self.assertTrue(env["AWS_SIGNING_ROLE_ARN"].endswith(":role/TiloTExtensionManifestSigner"), env)
        text = repr(publisher)
        self.assertNotIn("alias/tilo-release-signing", text, "the OS release key never signs a manifest")
        self.assertNotIn("TiloTReleaseSigner", text)

if __name__ == "__main__":
    unittest.main()
