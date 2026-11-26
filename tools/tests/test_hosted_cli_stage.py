"""Local qualification fixtures never touch a real release, Git host or Keychain."""

import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
from email.message import Message
from urllib.error import HTTPError
import urllib.request
from urllib.response import addinfourl

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tools/ci/publish"))
import hosted_cli_stage as stage
from test_macos_signing_macho import fixture


TAG = "v+cli-0.3.1-assistant.1"
COMMIT = "a" * 40
ARCHES = ("aarch64", "x86_64")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def encode(record):
    return json.dumps(record, sort_keys=True).encode()


def archive(arch, signed):
    members = {}
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w:gz", format=tarfile.USTAR_FORMAT) as tar:
        for name in ("tilo", "tilo-runtime"):
            binary = bytearray(fixture(128 if signed else 0, extra_data=name.encode()))
            if arch == "x86_64":
                binary[4:12] = bytes.fromhex("0700000103000000")
            members[name] = digest(binary)
            entry = tarfile.TarInfo(name)
            entry.mode, entry.size = 0o755, len(binary)
            entry.uname = entry.gname = "root"
            tar.addfile(entry, io.BytesIO(binary))
    return out.getvalue(), members


class HostedCliQualification(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.raw, self.output = self.root / "raw", self.root / "qualified"
        self.raw.mkdir()
        self.env = patch.dict(os.environ, T_REF="tag:" + TAG, T_COMMIT=COMMIT)
        self.env.start()
        self.addCleanup(self.env.stop)
        self.files = {"qualification.cms": b"CMS fixture; authenticity checked by the Apple signer gate"}
        self.receipt = {
            "schema": 1, "tag": TAG, "commit": COMMIT,
            "raw_sha256": {}, "signed_sha256": {}, "account_sha256": {},
        }
        for arch in ARCHES:
            name = f"tilo-macos-{arch}.tar.gz"
            signed, members = archive(arch, True)
            raw, _ = archive(arch, False)
            self.files[name] = signed
            (self.raw / name).write_bytes(raw)
            self.receipt["signed_sha256"][name] = digest(signed)
            self.receipt["raw_sha256"][name] = digest(raw)
            linux = f"tilo-linux-{arch}.tar.gz"
            (self.raw / linux).write_bytes(linux.encode())
            self.receipt["raw_sha256"][linux] = digest(linux.encode())
            first = {
                "cli_device_sha256": digest(b"public device id " + arch.encode()),
                "daemon_device_sha256": digest(b"public device id " + arch.encode()),
                "workspace_sha256": digest(b"disposable workspace"), "session": "connected", "daemon_pid": 250,
            }
            self.files[f"account-{arch}.json"] = encode({
                "schema": 1, "tag": TAG, "commit": COMMIT, "architecture": arch,
                "signed_archive_sha256": digest(signed), "installed_sha256": members, "cli_version": "tilo 0.3.1-assistant.1",
                "macos_version": "15.6.1", "execution": "native", "uid": 501,
                "default_keychain": "login.keychain-db", "initial_device_items": 0, "final_device_items": 1,
                "keychain_prompts": 0, "file_device_keys": 0,
                "elapsed_ms": 54780, "setup_exit": 0, "first": first, "restarted": {**first, "daemon_pid": 720},
            })
        self.seal()

    def seal(self):
        for arch in ARCHES:
            name = f"account-{arch}.json"
            self.receipt["account_sha256"][name] = digest(self.files[name])
        self.files["qualification.json"] = encode(self.receipt)
        self.tag = (
            f"object {COMMIT}\ntype commit\ntag {TAG}\ntagger Operator <operator@example.test> 1 +0000\n\n"
            f"Reviewed qualification.\n\nHosted-CLI-Qualification-SHA256: {digest(self.files['qualification.json'])}\n"
        ).encode()

    def fetch(self):
        def staged(commit, name, dest, limit):
            self.assertEqual(commit, COMMIT)
            dest.write_bytes(self.files[name])
        with patch.object(stage, "upstream_tag", return_value=self.tag), patch.object(stage, "download", staged):
            stage.fetch(self.raw, self.output)

    def test_fetch_then_offline_verify_binds_all_four_ci_builds_and_native_evidence(self):
        self.fetch()
        self.assertEqual((self.output / "qualification.tag").read_bytes(), self.tag)
        result = subprocess.run([sys.executable, stage.__file__, "verify", self.raw, self.output], capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        for path in (self.raw / "tilo-linux-x86_64.tar.gz", self.output / "tilo-macos-aarch64.tar.gz",
                     self.output / "account-x86_64.json", self.output / "qualification.json", self.output / "qualification.tag"):
            with self.subTest(file=path.name):
                original = path.read_bytes()
                path.write_bytes(original.replace(b"type commit", b"type tag") if path.suffix == ".tag" else original + b"changed")
                with self.assertRaises(ValueError):
                    stage.verify(self.raw, self.output)
                path.write_bytes(original)
        (self.output / "qualification.cms").unlink()
        with self.assertRaises((ValueError, OSError)):
            stage.verify(self.raw, self.output)

    def test_prepare_from_actual_archives_and_account_records_is_deterministic_without_a_tag_or_cms(self):
        self.output.mkdir()
        for name, data in self.files.items():
            if name.startswith(("tilo-", "account-")):
                (self.output / name).write_bytes(data)
        with patch.object(stage, "upstream_tag", side_effect=AssertionError("offline preparation fetched a tag")):
            first = stage.prepare(self.raw, self.output)
            receipt = self.output / "qualification.json"
            self.assertEqual(json.loads(receipt.read_bytes()), self.receipt)
            self.assertEqual(digest(receipt.read_bytes()), first)
            self.assertEqual(stage.prepare(self.raw, self.output), first)
            receipt.write_bytes(b"previously reviewed qualification")
            with self.assertRaisesRegex(ValueError, "different qualification.json"):
                stage.prepare(self.raw, self.output)
            self.assertEqual(receipt.read_bytes(), b"previously reviewed qualification")
        self.assertFalse((self.output / "qualification.tag").exists())
        self.assertFalse((self.output / "qualification.cms").exists())

    def test_annotation_must_be_unique_direct_and_for_this_release(self):
        for bad in (self.tag.replace(b"type commit", b"type tag"), self.tag.replace(COMMIT.encode(), b"b" * 40),
                    self.tag.replace(TAG.encode(), b"v0.5.0"), self.tag + self.tag.splitlines()[-1] + b"\n",
                    self.tag.replace(b"SHA256: ", b"SHA256: G"), self.tag.replace(b"SHA256:", b"SHA1:")):
            with self.subTest(tag=bad), self.assertRaises(ValueError):
                stage.qualification_pin(bad, TAG, COMMIT)

    def test_disallows_untrusted_locations_and_non_release_environment(self):
        for ref, commit in (("tag:v0.5.0", COMMIT), ("tag:v+cli-0.3.1", COMMIT),
                            ("tag:" + TAG + "?other=1", COMMIT), ("tag:" + TAG, "short")):
            with patch.dict(os.environ, T_REF=ref, T_COMMIT=commit), self.assertRaises(ValueError):
                stage.fetch(self.raw, self.output)
        start = self.receipt["account_sha256"].pop("account-aarch64.json")
        self.receipt["account_sha256"]["https://unrelated.example/test"] = start
        self.files["qualification.json"] = encode(self.receipt)
        self.tag = self.tag.replace(self.tag.splitlines()[-1].split()[-1], digest(self.files["qualification.json"]).encode())
        with self.assertRaises(ValueError):
            self.fetch()
        self.assertFalse(self.output.exists())

    def test_a_reviewed_account_record_still_has_to_match_the_release_and_two_live_processes(self):
        name = "account-aarch64.json"
        original = json.loads(self.files[name])
        cases = [
            {"architecture": "x86_64"}, {"execution": "rosetta"}, {"uid": 0}, {"setup_exit": False},
            {"file_device_keys": 1}, {"default_keychain": "isolated-test.keychain-db"},
            {"final_device_items": 0}, {"keychain_prompts": 1},
            {"cli_version": "tilo 0.5.0"}, {"cli_version": "tilo 0.3.1-assistant.2"},
            {"cli_version": "tilo v0.3.1-assistant.1"},
            {"restarted": {**original["restarted"], "daemon_pid": 2**40}},
            {"installed_sha256": {**original["installed_sha256"], "tilo-runtime": "f"*64}},
            {"restarted": original["first"]},
            {"restarted": {**original["restarted"], "daemon_device_sha256": "e"*64}},
            {"restarted": {**original["restarted"], "workspace_sha256": "e"*64}},
        ]
        for changes in cases:
            with self.subTest(changes=changes):
                self.files[name] = encode({**original, **changes})
                self.seal()
                with self.assertRaises(ValueError):
                    self.fetch()
                self.assertFalse(self.output.exists())

    def test_git_ref_is_fetched_as_an_annotated_object_and_peeling_a_tag_is_insufficient(self):
        # Exercise real Git objects using only a disposable upstream. Only the transport is substituted.
        upstream = self.root / "upstream"
        upstream.mkdir()
        env = {"PATH": os.environ["PATH"], "HOME": str(self.root), "GIT_CONFIG_NOSYSTEM": "1"}

        def git(*args, data=b""):
            return subprocess.check_output(["git", *args], cwd=upstream, input=data, env=env).strip()

        git("init", "--bare", "--quiet", "--template=")
        git("config", "uploadpack.allowFilter", "true")
        tree = git("mktree")
        commit = git("-c", "user.name=Qualification fixture", "-c", "user.email=test@invalid",
                     "commit-tree", tree, data=b"fixture").decode()
        tag = "v+cli-0.0.0-fixture.1"
        annotation = self.tag.replace(COMMIT.encode(), commit.encode()).replace(TAG.encode(), tag.encode())
        tag_id = git("mktag", data=annotation)
        tag_ref = f"refs/tags/{tag}"
        git("update-ref", tag_ref, tag_id)
        actual = subprocess.check_output

        def local_transport(cmd, **kwargs):
            if "fetch" in cmd:
                self.assertIn("http.followRedirects=false", cmd)
                self.assertIn("protocol.allow=never", cmd)
                self.assertEqual(cmd[-2:], [stage.REPOSITORY, f"{tag_ref}:{tag_ref}"])
                cmd = [*cmd[:1], "-c", "protocol.file.allow=always", *cmd[1:-2], upstream.as_uri(), cmd[-1]]
            return actual(cmd, **kwargs)

        with patch.object(stage.subprocess, "check_output", local_transport):
            self.assertEqual(stage.upstream_tag(tag, commit), annotation)
        git("update-ref", tag_ref, commit)
        with patch.object(stage.subprocess, "check_output", local_transport), self.assertRaisesRegex(ValueError, "annotated tag"):
            stage.upstream_tag(tag, commit)

    def test_staged_download_refuses_a_redirect_and_never_requests_the_other_location(self):
        requests = []
        def redirect(handler, req):
            requests.append(req.full_url)
            headers = Message()
            headers["Location"] = "https://unrelated.invalid/content"
            response = addinfourl(io.BytesIO(), headers, req.full_url, code=302)
            response.msg = "Found"
            return response
        with patch.object(urllib.request.HTTPSHandler, "https_open", redirect), self.assertRaises(HTTPError) as failure:
            stage.download(COMMIT, "qualification.json", self.root / "receipt", stage.META_LIMIT)
        failure.exception.close()
        self.assertEqual(requests, [f"{stage.PACKAGE}/{COMMIT}/qualification.json"])
        self.assertFalse((self.root / "receipt").exists())

    def test_the_two_architectures_need_independent_native_accounts(self):
        intel = "account-x86_64.json"
        account = json.loads(self.files[intel])
        other = json.loads(self.files["account-aarch64.json"])["first"]["cli_device_sha256"]
        for phase in ("first", "restarted"):
            account[phase].update(cli_device_sha256=other, daemon_device_sha256=other)
        self.files[intel] = encode(account)
        self.seal()
        with self.assertRaisesRegex(ValueError, "independent device identities"):
            self.fetch()

    def test_duplicate_json_keys_cannot_override_reviewed_fields(self):
        self.files["account-aarch64.json"] = self.files["account-aarch64.json"].replace(b'"uid": 501', b'"uid": 0, "uid": 501')
        self.seal()
        with self.assertRaises(ValueError):
            self.fetch()


if __name__ == "__main__":
    unittest.main()
