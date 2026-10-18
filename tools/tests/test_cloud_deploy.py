"""The cloud deploy tool against a fake registry, fake endpoints, a fake forge and a real Git history."""

import base64
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest import mock

from git_fixture import isolated_git_environment


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("cloud_deploy", ROOT / "tools/release/cloud_deploy.py")
deploy = importlib.util.module_from_spec(SPEC)
# Dataclasses resolve their string annotations through sys.modules.
sys.modules[SPEC.name] = deploy
SPEC.loader.exec_module(deploy)

REGISTRY = "registry.tilo.so"
REPO = "Example/gitops"
CONFIG = "deploy/tilo.toml"
API = f"{deploy.FORGE}/api/v1/repos/{REPO}"


def target_text(rollout=True, timeout=60, settle=10):
    return f"""
rollout = {str(rollout).lower()}
timeout_secs = {timeout}
settle_secs = {settle}

[[service]]
name = "web-entry"
image = "{REGISTRY}/tilo/web-entry"
manifest = "apps/web/deployment.yaml"
health = "https://web.example/version.json"

[[service]]
name = "room-service"
image = "{REGISTRY}/tilo/room-service"
manifest = "apps/rooms/deployment.yaml"
health = "https://rooms.example/mode"
expect = {{ relay = "up" }}
"""


TARGET = deploy.load_target(target_text(), "test")
WEB, ROOM = TARGET.services


def digest(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


class Clock:
    """Time that passes only when the tool sleeps."""

    def __init__(self):
        self.now = 0.0
        self.on_sleep = []

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds
        for hook in self.on_sleep:
            hook(self.now)


class Fake:
    """Registry, public endpoints and the target repository's API, by URL."""

    def __init__(self):
        self.routes = {}
        self.calls = []
        self.digests = {}
        self.blobs = 0
        self.conflicts = 0

    def publish(self, service, revision, *, architecture="amd64", label=None):
        label = revision if label is None else label
        labels = {"org.opencontainers.image.revision": label} if label else {}
        config = json.dumps({"os": "linux", "architecture": architecture, "config": {"Labels": labels}}).encode()
        manifest = json.dumps({"schemaVersion": 2,
                               "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
                               "config": {"digest": digest(config)}, "layers": [], "x": revision}).encode()
        base = f"https://{REGISTRY}/v2/{service.repository}"
        self.routes[f"{base}/blobs/{digest(config)}"] = deploy.Response(200, {}, config)
        for reference in (f"sha-{revision}", digest(manifest)):
            self.routes[f"{base}/manifests/{reference}"] = deploy.Response(
                200, {"content-type": "application/vnd.docker.distribution.manifest.v2+json"}, manifest)
        tags_url = f"{base}/tags/list?n=1000"
        tags = json.loads(self.routes.get(tags_url, deploy.Response(200, {}, b'{"tags": []}')).body)["tags"]
        self.routes[tags_url] = deploy.Response(200, {}, json.dumps({"tags": tags + [f"sha-{revision}"]}).encode())
        self.digests[(service.name, revision)] = digest(manifest)
        return digest(manifest)

    def serve(self, url, document, status=200):
        self.routes[url] = deploy.Response(status, {}, json.dumps(document).encode())

    def serve_file(self, path, text):
        self.blobs += 1
        self.serve(f"{API}/contents/{path}?ref=main",
                   {"content": base64.b64encode(text.encode()).decode(), "sha": f"blob{self.blobs}"})

    def file(self, path):
        document = json.loads(self.routes[f"{API}/contents/{path}?ref=main"].body)
        return base64.b64decode(document["content"]).decode(), document["sha"]

    # The in-region registry: every read needs a token, which anonymous callers get.
    token_required = False
    TOKEN_SERVICE = f"https://{REGISTRY}/token"

    def __call__(self, method, url, headers, body):
        self.calls.append((method, url, json.loads(body) if body else None))
        if self.token_required and url.startswith(self.TOKEN_SERVICE + "?"):
            return deploy.Response(200, {}, b'{"token": "anonymous-pull"}')
        if (self.token_required and url.startswith(f"https://{REGISTRY}/v2/")
                and headers.get("Authorization") != "Bearer anonymous-pull"):
            repository = url.split("/v2/", 1)[1].rsplit("/", 2)[0]
            challenge = (f'Bearer realm="{self.TOKEN_SERVICE}",service="{REGISTRY}",'
                         f'scope="repository:{repository}:pull"')
            return deploy.Response(401, {"www-authenticate": challenge}, b"{}")
        if method == "POST" and url == f"{API}/contents":
            if self.conflicts:
                self.conflicts -= 1
                return deploy.Response(409, {}, b'{"message": "sha does not match"}')
            commit = json.loads(body)
            for each in commit["files"]:
                if self.file(each["path"])[1] != each["sha"]:
                    return deploy.Response(409, {}, b'{"message": "sha does not match"}')
            for each in commit["files"]:
                self.serve_file(each["path"], base64.b64decode(each["content"]).decode())
            return deploy.Response(201, {}, b"{}")
        return self.routes.get(url, deploy.Response(404, {}, b"{}"))


def manifest_text(service, image_digest):
    return (f"spec:\n  template:\n    spec:\n      containers:\n        - name: {service.name}\n"
            f"          image: {service.image}@{image_digest}\n          ports: []\n")


class CloudDeployTests(unittest.TestCase):
    def setUp(self):
        environment = isolated_git_environment(
            GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
            GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
            GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid")
        for name in (deploy.TOKEN_ENV, deploy.REPO_ENV, deploy.CONFIG_ENV):
            environment.pop(name, None)
        patcher = mock.patch.dict(os.environ, environment, clear=True)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "tilo"
        self.git("init", "-q", "-b", "stable", str(self.repo), cwd=self.temp.name)
        subjects = ["init: deployed",
                    "Merge pull request 'fix(web): offer a reload after a deploy' (#11) from a into main",
                    "Merge pull request 'feat(relay): serve governed rooms' (#12) from b into main",
                    "fix(web): newest, web image only"]
        self.revisions = []
        for subject in subjects:
            self.git("commit", "-q", "--allow-empty", "-m", subject)
            self.revisions.append(self.git("rev-parse", "HEAD"))
        self.git("checkout", "-q", "-b", "side", self.revisions[0])
        self.git("commit", "-q", "--allow-empty", "-m", "never released")
        self.side = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", "stable")
        self.deployed, _, self.target, self.newest = self.revisions
        self.fake = Fake()
        for revision in (self.deployed, self.target):
            self.fake.publish(WEB, revision)
            self.fake.publish(ROOM, revision)
        self.fake.publish(WEB, self.newest)
        self.fake.publish(WEB, self.side)
        self.fake.publish(ROOM, self.side)
        self.running = {WEB.name: self.deployed, ROOM.name: self.deployed}
        # The room service of a build whose relay cannot start answers, without a relay.
        self.relay_down_in = set()
        self.serve_running()
        self.clock = Clock()

    def serve_running(self):
        self.fake.serve(WEB.health, {"version": "0123456789abcdef0123", "revision": self.running[WEB.name]})
        room = self.running[ROOM.name]
        self.fake.serve(ROOM.health, {"mode": "normal", "revision": room,
                                      "relay": "restarting" if room in self.relay_down_in else "up"})

    def git(self, *args, cwd=None):
        return subprocess.run(["git", *args], cwd=cwd or self.repo, check=True, text=True,
                              stdout=subprocess.PIPE).stdout.strip()

    def infra(self, revision, rollout=True):
        root = Path(self.temp.name) / "infra"
        (root / CONFIG).parent.mkdir(parents=True, exist_ok=True)
        (root / CONFIG).write_text(target_text(rollout))
        for service in TARGET.services:
            path = root / service.manifest
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(manifest_text(service, self.fake.digests[(service.name, revision)]))
        return root

    def serve_infra(self, revision, rollout=True):
        os.environ[deploy.TOKEN_ENV] = "secret"
        os.environ[deploy.REPO_ENV] = REPO
        os.environ[deploy.CONFIG_ENV] = CONFIG
        self.fake.serve_file(CONFIG, target_text(rollout))
        for service in TARGET.services:
            self.fake.serve_file(service.manifest, manifest_text(service, self.fake.digests[(service.name, revision)]))

    def receipt(self, service, revision, image_digest=None, name=None, signed=True):
        image_digest = image_digest or self.fake.digests[(service.name, revision)]
        path = Path(self.temp.name) / (name or f"{service.name}-{revision[:7]}.json")
        path.write_text(json.dumps({"schemaVersion": 3, "repository": service.image, "digest": image_digest,
                                    "image": f"{service.image}@{image_digest}", "tags": [f"sha-{revision}"],
                                    "sourceRevision": revision, "reference": "stable", "signed": signed}))
        return str(path)

    def run_tool(self, *argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = deploy.main(["--root", str(self.repo), *argv], transport=self.fake,
                               clock=self.clock, sleep=self.clock.sleep)
        return code, out.getvalue(), err.getvalue()

    def plan(self, *argv, infra=None):
        return self.run_tool("plan", "--config", CONFIG, "--infra", str(infra or self.infra(self.deployed)), *argv)

    def writes(self):
        return [call for call in self.fake.calls if call[0] != "GET"]

    def test_plan_moves_every_service_to_the_newest_commit_that_has_all_images(self):
        infra = self.infra(self.deployed)
        out_dir = Path(self.temp.name) / "out"
        code, out, err = self.plan("--out", str(out_dir), infra=infra)
        self.assertEqual(code, 0, err)
        plan = json.loads((out_dir / "plan.json").read_text())
        self.assertEqual(plan["revision"], self.target)
        self.assertEqual(plan["moves"], ["web-entry", "room-service"])
        for service in TARGET.services:
            self.assertEqual(plan["images"][service.name]["digest"], self.fake.digests[(service.name, self.target)])
            self.assertEqual(plan["current"][service.name]["built"], self.deployed)
        diff = (out_dir / "infra.diff").read_text()
        for service in TARGET.services:
            self.assertIn(f"-          image: {service.image}@{self.fake.digests[(service.name, self.deployed)]}", diff)
            self.assertIn(f"+          image: {service.image}@{self.fake.digests[(service.name, self.target)]}", diff)
        message = (out_dir / "message.txt").read_text()
        self.assertTrue(message.startswith(f"rooms, web: deploy tilo/tilo {self.target[:12]}\n"), message)
        self.assertIn("- feat(relay): serve governed rooms (#12)", message)
        self.assertIn("- fix(web): offer a reload after a deploy (#11)", message)
        self.assertNotIn("newest, web image only", message)
        # Planning reads; only --apply edits the checkout.
        self.assertIn(self.fake.digests[("web-entry", self.deployed)], (infra / WEB.manifest).read_text())
        code, _, err = self.plan("--apply", infra=infra)
        self.assertEqual(code, 0, err)
        self.assertIn(self.fake.digests[("web-entry", self.target)], (infra / WEB.manifest).read_text())
        self.assertIn(self.fake.digests[("room-service", self.target)], (infra / ROOM.manifest).read_text())
        code, out, _ = self.plan(infra=infra)
        self.assertEqual(code, 0)
        self.assertIn("already runs", out)

    def test_receipts_must_agree_with_each_other_and_with_the_registry(self):
        infra = self.infra(self.deployed)
        code, _, err = self.plan("--receipt", self.receipt(WEB, self.target),
                                 "--receipt", self.receipt(ROOM, self.target), infra=infra)
        self.assertEqual(code, 0, err)
        code, _, err = self.plan("--receipt", self.receipt(WEB, self.target),
                                 "--receipt", self.receipt(ROOM, self.deployed), infra=infra)
        self.assertEqual(code, 1)
        self.assertIn("different revisions", err)
        # A receipt naming the deployed image under the new revision is caught by the release tag.
        lying = self.receipt(ROOM, self.target, self.fake.digests[("room-service", self.deployed)], name="lying.json")
        code, _, err = self.plan("--receipt", self.receipt(WEB, self.target), "--receipt", lying, infra=infra)
        self.assertEqual(code, 1)
        self.assertIn(f"not the receipt's {self.fake.digests[('room-service', self.deployed)]}", err)
        code, _, err = self.plan("--receipt", self.receipt(WEB, self.target), infra=infra)
        self.assertEqual(code, 1)
        self.assertIn("every service", err)
        # An image the target does not run is ignored; an unsigned one refused where asked.
        other = Path(self.temp.name) / "other.json"
        other.write_text(json.dumps({"repository": f"{REGISTRY}/tilo/other", "digest": "x"}))
        code, out, err = self.plan("--receipt", self.receipt(WEB, self.target), "--receipt",
                                   self.receipt(ROOM, self.target), "--receipt", str(other), infra=infra)
        self.assertEqual(code, 0, err)
        self.assertIn("does not run; ignored", out)
        code, _, err = self.plan("--require-signed", "--receipt", self.receipt(WEB, self.target), "--receipt",
                                 self.receipt(ROOM, self.target, name="unsigned.json", signed=False), infra=infra)
        self.assertEqual(code, 1)
        self.assertIn("unsigned: room-service", err)

    def test_receipts_from_two_revisions_deploy_the_newer_when_the_older_image_is_unchanged(self):
        """Stable republishes an image only when its inputs change, as t caches the build."""
        def commit(files, subject):
            for name, text in files.items():
                path = self.repo / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            self.git("add", "-A")
            self.git("commit", "-q", "-m", subject)
            return self.git("rev-parse", "HEAD")

        checks = ('[[check]]\nname = "image-room-service-context"\ninputs = ["crates/room/**"]\n'
                  '[[check]]\nname = "image-room-service"\nneeds = ["image-room-service-context"]\n'
                  'inputs = ["tools/ci/publish/oci-build.sh"]\n')
        room = commit({"t.toml": checks, "crates/room/lib.rs": "1"}, "room service")
        web = commit({"crates/browser/lib.rs": "2"}, "web only")
        self.fake.publish(ROOM, room)
        self.fake.publish(WEB, web)
        out_dir = Path(self.temp.name) / "two"
        code, _, err = self.plan("--out", str(out_dir), "--receipt", self.receipt(ROOM, room),
                                 "--receipt", self.receipt(WEB, web))
        self.assertEqual(code, 0, err)
        plan = json.loads((out_dir / "plan.json").read_text())
        self.assertEqual(plan["revision"], web)
        self.assertEqual(plan["images"]["room-service"]["digest"], self.fake.digests[("room-service", room)])
        self.assertEqual(plan["images"]["room-service"]["sourceRevision"], room)

        # Once the room service's own files change, its older receipt no longer stands for the newer commit.
        changed = commit({"crates/room/lib.rs": "3"}, "room service changed")
        self.fake.publish(WEB, changed)
        code, _, err = self.plan("--receipt", self.receipt(ROOM, room), "--receipt", self.receipt(WEB, changed))
        self.assertEqual(code, 1)
        self.assertIn("files it is built from changed", err)

    def test_a_service_already_running_the_build_keeps_its_pin(self):
        """A release the web entry's build did not change in (carried, so a new digest) leaves it."""
        carried = self.fake.publish(WEB, self.target, label=self.deployed)
        out_dir = Path(self.temp.name) / "kept"
        code, _, err = self.plan("--out", str(out_dir), "--receipt", self.receipt(WEB, self.target, carried),
                                 "--receipt", self.receipt(ROOM, self.target))
        self.assertEqual(code, 0, err)
        plan = json.loads((out_dir / "plan.json").read_text())
        self.assertEqual(plan["moves"], ["room-service"])
        self.assertNotIn(WEB.manifest, (out_dir / "infra.diff").read_text())
        self.assertIn("Kept, already running this build: web-entry.", (out_dir / "message.txt").read_text())

    def test_only_newer_releases_deploy_unless_a_rollback_is_asked_for(self):
        code, _, err = self.plan("--revision", self.side)
        self.assertEqual(code, 1)
        self.assertIn("is not on HEAD", err)
        code, _, err = self.plan("--revision", self.newest)
        self.assertEqual(code, 1)
        self.assertIn("does not exist", err)
        ahead = self.infra(self.target)
        # An older release than what runs is a run overtaken by a later one: nothing to do.
        code, out, err = self.plan("--revision", self.deployed, infra=ahead)
        self.assertEqual(code, 0, err)
        self.assertIn("Superseded", out)
        code, out, err = self.plan("--revision", self.deployed, "--allow-rollback", "--apply", infra=ahead)
        self.assertEqual(code, 0, err)
        self.assertIn(self.fake.digests[("web-entry", self.deployed)], (ahead / WEB.manifest).read_text())

    def test_registry_content_is_checked_against_the_digest(self):
        pinned = self.fake.digests[("room-service", self.target)]
        url = f"https://{REGISTRY}/v2/{ROOM.repository}/manifests/{pinned}"
        real = self.fake.routes[url]
        self.fake.routes[url] = deploy.Response(real.status, real.headers, real.body + b" ")
        code, _, err = self.plan("--receipt", self.receipt(WEB, self.target), "--receipt", self.receipt(ROOM, self.target))
        self.assertEqual(code, 1)
        self.assertIn("served content hashing to", err)

    def test_the_in_region_registry_is_read_with_an_anonymous_pull_token(self):
        self.fake.token_required = True
        code, out, err = self.plan()
        self.assertEqual(code, 0, err)
        self.assertIn(self.fake.digests[("room-service", self.target)], out)
        token_requests = {url for _, url, _ in self.fake.calls if url.startswith(Fake.TOKEN_SERVICE)}
        self.assertEqual(token_requests, {
            f"{Fake.TOKEN_SERVICE}?scope=repository%3A{service.repository.replace('/', '%2F')}%3Apull"
            f"&service={REGISTRY}" for service in TARGET.services})

    def test_credentials_stay_with_the_host_they_were_sent_to_across_redirects(self):
        handler = deploy.SameHostAuthorization()
        registry = f"https://{REGISTRY}/v2/tilo/web-entry/blobs/sha256:{'0' * 64}"
        for target, kept in ((f"https://{REGISTRY}/v2/tilo/web-entry/blobs/other", True),
                             ("https://bucket.s3.us-west-2.amazonaws.com/blob?X-Amz-Signature=x", False)):
            request = deploy.urllib.request.Request(registry, headers={"Authorization": "Bearer t"})
            follow = handler.redirect_request(request, None, 307, "Temporary Redirect", {}, target)
            self.assertEqual(follow.has_header("Authorization"), kept, target)

    def test_an_image_for_another_platform_is_refused(self):
        self.fake.publish(ROOM, self.newest, architecture="arm64")
        code, _, err = self.plan("--revision", self.newest)
        self.assertEqual(code, 1)
        self.assertIn("linux/arm64", err)

    def test_the_target_is_validated(self):
        for text, why in (("rollout = true\n", "lists no [[service]]"),
                          (target_text().replace("rollout = true", "rollout = 1"), "`rollout`"),
                          (target_text().replace("https://rooms", "http://rooms"), "not https"),
                          (target_text().replace('/tilo/room-service"', '/tilo/web-entry"'), "own name and image"),
                          (target_text().replace("timeout_secs = 60", "timeout_secs = 5"), "timeout_secs")):
            with self.assertRaises(deploy.PromotionError) as raised:
                deploy.load_target(text, "t")
            self.assertIn(why, str(raised.exception))

    def test_deploy_needs_the_target_and_stops_before_writing_while_rollout_is_off(self):
        code, _, err = self.run_tool("deploy")
        self.assertEqual(code, 1)
        self.assertIn(deploy.CONFIG_ENV, err)
        code, _, err = self.run_tool("deploy", "--config", CONFIG)
        self.assertEqual(code, 1)
        self.assertIn(deploy.TOKEN_ENV, err)
        self.serve_infra(self.deployed, rollout=False)
        code, out, err = self.run_tool("deploy")
        self.assertEqual(code, 0, err)
        self.assertIn(f"Dry run (rollout = false in {CONFIG})", out)
        self.assertIn(f"{ROOM.health} to report {self.target[:12]}", out)
        self.assertEqual(self.writes(), [])
        self.serve_infra(self.deployed, rollout=True)
        code, out, err = self.run_tool("deploy", "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertIn("Dry run (--dry-run)", out)
        self.assertEqual(self.writes(), [])

    def reconcile(self, after=10, broken=()):
        """The target's reconciler: `after` seconds after a pin moves, the service runs it, unless
        it is `broken`, whose new pods never become ready."""
        moved = {}

        def hook(now):
            for service in TARGET.services:
                pinned = next(revision for (name, revision), each in self.fake.digests.items()
                              if name == service.name and each in self.fake.file(service.manifest)[0])
                moved.setdefault((service.name, pinned), now)
                if now - moved[(service.name, pinned)] >= after and not (
                        service.name in broken and pinned != self.deployed):
                    self.running[service.name] = pinned
            self.serve_running()
        self.clock.on_sleep.append(hook)

    def test_deploy_commits_the_pins_to_main_and_waits_until_the_services_run_them(self):
        self.serve_infra(self.deployed)
        self.fake.conflicts = 1  # someone else pushed in between: planned again
        self.reconcile(after=30)
        code, out, err = self.run_tool("deploy", "--receipt", self.receipt(WEB, self.target),
                                       "--receipt", self.receipt(ROOM, self.target), "--require-signed")
        self.assertEqual(code, 0, err)
        self.assertIn("moved while this ran; planning again", out)
        self.assertIn(f"Deployed {self.target}", out)
        commits = [body for method, url, body in self.writes() if url == f"{API}/contents"]
        self.assertEqual(len(commits), 2)
        commit = commits[-1]
        self.assertEqual(commit["branch"], "main")
        self.assertNotIn("new_branch", commit)
        self.assertTrue(commit["message"].startswith(f"rooms, web: deploy tilo/tilo {self.target[:12]}"))
        self.assertEqual(sorted(each["path"] for each in commit["files"]), sorted(s.manifest for s in TARGET.services))
        for service in TARGET.services:
            self.assertIn(f"{service.image}@{self.fake.digests[(service.name, self.target)]}",
                          self.fake.file(service.manifest)[0])
        # Healthy for the settle time after it first reported the build, not before.
        self.assertGreaterEqual(self.clock.now, 30 + 10)
        # Run again: nothing left to do.
        self.fake.calls.clear()
        code, out, _ = self.run_tool("deploy")
        self.assertEqual(code, 0)
        self.assertIn("already runs", out)
        self.assertEqual(self.writes(), [])

    def test_a_service_reporting_its_build_without_what_it_serves_is_rolled_back(self):
        """A room service whose relay cannot start still answers with its revision."""
        self.serve_infra(self.deployed)
        self.relay_down_in = {self.target}
        self.reconcile()
        code, _, err = self.run_tool("deploy")
        self.assertEqual(code, 1)
        self.assertIn(f"{ROOM.health} reports relay 'restarting', not 'up'", err)
        self.assertIn("Rolled back: the previous images are healthy again.", err)
        for service in TARGET.services:
            self.assertIn(f"{service.image}@{self.fake.digests[(service.name, self.deployed)]}",
                          self.fake.file(service.manifest)[0])

    def test_a_build_rolled_back_to_may_predate_an_expected_field(self):
        older = deploy.Service("room-service", ROOM.image, ROOM.manifest, "https://old.example/mode",
                               expect=(("relay", "up"),))
        self.fake.serve(older.health, {"mode": "normal", "revision": self.deployed})
        self.assertIn("relay None", deploy.probe(self.fake, older, self.deployed))
        self.assertIsNone(deploy.probe(self.fake, older, self.deployed, strict=False))
        self.fake.serve(older.health, {"revision": self.deployed, "relay": "restarting"})
        self.assertIsNotNone(deploy.probe(self.fake, older, self.deployed, strict=False))

    def test_a_rollout_that_does_not_become_healthy_is_rolled_back(self):
        self.serve_infra(self.deployed)
        # The web entry rolls out; the room service's new pod never answers.
        self.reconcile(broken={ROOM.name})
        code, out, err = self.run_tool("deploy")
        self.assertEqual(code, 1)
        self.assertIn(f"The rollout of {self.target[:12]} failed: {ROOM.health} reports {self.deployed}", err)
        self.assertIn("Rolled back: the previous images are healthy again.", err)
        commits = [body for method, url, body in self.writes() if url == f"{API}/contents"]
        self.assertEqual(len(commits), 2)
        self.assertTrue(commits[1]["message"].startswith(f"rooms, web: roll back tilo/tilo {self.target[:12]}"))
        for service in TARGET.services:
            self.assertIn(f"{service.image}@{self.fake.digests[(service.name, self.deployed)]}",
                          self.fake.file(service.manifest)[0])
        self.assertLessEqual(self.clock.now, 2 * (60 + 10) + 2 * deploy.PROBE_INTERVAL)

    def test_a_rollback_leaves_pins_someone_else_moved(self):
        self.serve_infra(self.deployed)

        def moved(now):
            if now >= 20 and self.fake.file(ROOM.manifest)[0].count(self.fake.digests[(ROOM.name, self.target)]):
                self.fake.serve_file(ROOM.manifest, manifest_text(ROOM, self.fake.digests[(ROOM.name, self.side)]))
        self.clock.on_sleep.append(moved)
        code, _, err = self.run_tool("deploy")
        self.assertEqual(code, 1)
        self.assertIn("Not rolling back", err)
        self.assertEqual(len([1 for method, url, _ in self.writes() if url == f"{API}/contents"]), 1)


class WorkflowContractTests(unittest.TestCase):
    def test_t_deploys_the_receipts_stable_publishes(self):
        """Each deployed image's release is published from stable with a receipt, signed first, and
        the deploy runs after both, on stable, with the only secret it needs."""
        checks = {c["name"]: c for c in tomllib.loads((ROOT / "t.toml").read_text())["check"]}
        cloud = checks["cloud-deploy"]
        self.assertEqual(cloud["on"], ["stable", "dispatch"])
        self.assertEqual(cloud["secrets"], [deploy.TOKEN_ENV])
        self.assertTrue(cloud["git"])
        self.assertFalse(cloud["cache"])
        self.assertEqual(set(cloud["env"]), {deploy.REPO_ENV, deploy.CONFIG_ENV})
        for name in ("room-service", "web-entry"):
            publish = checks[f"publish-{name}"]
            self.assertIn(f"publish-{name}", cloud["needs"])
            receipt = f"/target/publish/{name}/receipt.json"
            self.assertIn(receipt, cloud["run"])
            self.assertIn(receipt, publish["outputs"])
            self.assertIn("stable", publish["on"], name)
            self.assertIn("'release:sha-{commit}'", publish["run"], name)
            self.assertIn(f"sign-{name}", publish["optional_needs"], name)
        # The web entry's release is the image carrying production's version, not the bare build.
        self.assertEqual(checks["sign-web-entry"]["needs"], ["carry-web-entry"])
        self.assertIn("carry-web-entry", checks["publish-web-entry"]["optional_needs"])


if __name__ == "__main__":
    unittest.main()
