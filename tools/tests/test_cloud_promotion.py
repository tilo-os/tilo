"""The Cloud promotion tool against a fake registry, fake endpoints and a real Git history."""

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
import unittest
from unittest import mock

from git_fixture import isolated_git_environment


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("cloud_promotion", ROOT / "tools/release/cloud_promotion.py")
promotion = importlib.util.module_from_spec(SPEC)
# Dataclasses resolve their string annotations through sys.modules.
sys.modules[SPEC.name] = promotion
SPEC.loader.exec_module(promotion)
WEB, ROOM = promotion.SERVICES


def digest(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


class Fake:
    """Registry, public endpoints and the infra repository's API, by URL."""

    def __init__(self):
        self.routes = {}
        self.calls = []
        self.digests = {}

    def publish(self, service, revision, *, architecture="amd64"):
        config = json.dumps({"os": "linux", "architecture": architecture,
                             "config": {"Labels": {"org.opencontainers.image.revision": revision}}}).encode()
        manifest = json.dumps({"schemaVersion": 2,
                               "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
                               "config": {"digest": digest(config)}, "layers": []}).encode()
        base = f"https://{promotion.REGISTRY}/v2/{service.repository}"
        self.routes[f"{base}/blobs/{digest(config)}"] = promotion.Response(200, {}, config)
        for reference in (f"sha-{revision}", digest(manifest)):
            self.routes[f"{base}/manifests/{reference}"] = promotion.Response(
                200, {"content-type": "application/vnd.docker.distribution.manifest.v2+json"}, manifest)
        tags_url = f"{base}/tags/list?n=1000"
        tags = json.loads(self.routes.get(tags_url, promotion.Response(200, {}, b'{"tags": []}')).body)["tags"]
        self.routes[tags_url] = promotion.Response(200, {}, json.dumps({"tags": tags + [f"sha-{revision}"]}).encode())
        self.digests[(service.name, revision)] = digest(manifest)
        return digest(manifest)

    def serve(self, url, document, status=200):
        self.routes[url] = promotion.Response(status, {}, json.dumps(document).encode())

    # The in-region registry: every read needs a token, which anonymous callers get.
    token_required = False
    TOKEN_SERVICE = f"https://{promotion.REGISTRY}/token"

    def __call__(self, method, url, headers, body):
        self.calls.append((method, url, json.loads(body) if body else None))
        if self.token_required and url.startswith(self.TOKEN_SERVICE + "?"):
            return promotion.Response(200, {}, b'{"token": "anonymous-pull"}')
        if (self.token_required and url.startswith(f"https://{promotion.REGISTRY}/v2/")
                and headers.get("Authorization") != "Bearer anonymous-pull"):
            repository = url.split("/v2/", 1)[1].rsplit("/", 2)[0]
            challenge = (f'Bearer realm="{self.TOKEN_SERVICE}",service="{promotion.REGISTRY}",'
                         f'scope="repository:{repository}:pull"')
            return promotion.Response(401, {"www-authenticate": challenge}, b"{}")
        if method == "POST" and url.endswith("/contents"):
            return promotion.Response(201, {}, b"{}")
        if method == "POST" and url.endswith("/pulls"):
            return promotion.Response(201, {}, b'{"html_url": "https://code.tilo.so/PodestaAI/infra/pulls/7"}')
        return self.routes.get(url, promotion.Response(404, {}, b"{}"))


def manifest_text(service, image_digest):
    return (f"spec:\n  template:\n    spec:\n      containers:\n        - name: {service.name}\n"
            f"          image: {service.image}@{image_digest}\n          ports: []\n")


class CloudPromotionTests(unittest.TestCase):
    def setUp(self):
        environment = isolated_git_environment(
            GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
            GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
            GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid")
        environment.pop(promotion.TOKEN_ENV, None)
        environment.pop("GITHUB_ACTIONS", None)
        patcher = mock.patch.dict(os.environ, environment, clear=True)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "tilo"
        self.git("init", "-q", "-b", "main", str(self.repo), cwd=self.temp.name)
        subjects = ["init: deployed",
                    "Merge pull request 'fix(web): offer a reload after a deploy' (#11) from a into main",
                    "Merge pull request 'feat(relay): serve governed rooms' (#12) from b into main",
                    "fix(web): newest, web image only"]
        self.revisions = []
        for subject in subjects:
            self.git("commit", "-q", "--allow-empty", "-m", subject)
            self.revisions.append(self.git("rev-parse", "HEAD"))
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.git("checkout", "-q", "-b", "side", self.revisions[0])
        self.git("commit", "-q", "--allow-empty", "-m", "never merged")
        self.side = self.git("rev-parse", "HEAD")
        self.deployed, _, self.target, self.newest = self.revisions
        self.fake = Fake()
        for revision in (self.deployed, self.target):
            self.fake.publish(WEB, revision)
            self.fake.publish(ROOM, revision)
        self.fake.publish(WEB, self.newest)
        self.fake.publish(WEB, self.side)
        self.fake.publish(ROOM, self.side)
        self.fake.serve(WEB.endpoint, {"version": "0123456789abcdef0123", "revision": self.deployed})
        self.fake.serve(ROOM.endpoint, {"mode": "normal"})

    def git(self, *args, cwd=None):
        return subprocess.run(["git", *args], cwd=cwd or self.repo, check=True, text=True,
                              stdout=subprocess.PIPE).stdout.strip()

    def infra(self, revision):
        root = Path(self.temp.name) / "infra"
        for service in promotion.SERVICES:
            path = root / service.manifest
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(manifest_text(service, self.fake.digests[(service.name, revision)]))
        return root

    def run_tool(self, *argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = promotion.main(["--root", str(self.repo), *argv], transport=self.fake)
        return code, out.getvalue(), err.getvalue()

    def test_plan_promotes_both_images_from_the_newest_commit_that_has_both(self):
        infra = self.infra(self.deployed)
        out_dir = Path(self.temp.name) / "out"
        code, out, err = self.run_tool("plan", "--infra", str(infra), "--out", str(out_dir))
        self.assertEqual(code, 0, err)
        plan = json.loads((out_dir / "plan.json").read_text())
        self.assertEqual(plan["revision"], self.target)
        self.assertEqual(plan["branch"], "promote/tilo-" + self.target[:12])
        for service in promotion.SERVICES:
            self.assertEqual(plan["images"][service.name]["digest"], self.fake.digests[(service.name, self.target)])
            self.assertEqual(plan["current"][service.name]["revision"], self.deployed)
        diff = (out_dir / "infra.diff").read_text()
        for service in promotion.SERVICES:
            self.assertIn(f"-          image: {service.image}@{self.fake.digests[(service.name, self.deployed)]}", diff)
            self.assertIn(f"+          image: {service.image}@{self.fake.digests[(service.name, self.target)]}", diff)
        body = (out_dir / "pr-body.md").read_text()
        self.assertIn("- feat(relay): serve governed rooms (#12)", body)
        self.assertIn("- fix(web): offer a reload after a deploy (#11)", body)
        self.assertNotIn("newest, web image only", body)
        self.assertIn(f"compare/{self.deployed}...{self.target}", body)
        # Planning reads; only --apply edits the checkout.
        self.assertIn(self.fake.digests[("web-entry", self.deployed)], (infra / WEB.manifest).read_text())
        code, _, err = self.run_tool("plan", "--infra", str(infra), "--apply")
        self.assertEqual(code, 0, err)
        self.assertIn(self.fake.digests[("web-entry", self.target)], (infra / WEB.manifest).read_text())
        self.assertIn(self.fake.digests[("room-service", self.target)], (infra / ROOM.manifest).read_text())
        code, out, _ = self.run_tool("plan", "--infra", str(infra))
        self.assertEqual(code, 0)
        self.assertIn("already pins", out)

    def test_plan_without_the_manifests_infers_the_pins_from_what_runs(self):
        code, out, err = self.run_tool("plan")
        self.assertEqual(code, 0, err)
        self.assertIn(f"`{self.fake.digests[('web-entry', self.deployed)]}` → "
                      f"`{self.fake.digests[('web-entry', self.target)]}`", out)
        self.assertIn("observed", out)
        self.assertIn("assumed", out)

    def test_receipts_must_agree_with_each_other_and_with_the_registry(self):
        def receipt(service, revision, image_digest=None, name=None):
            image_digest = image_digest or self.fake.digests[(service.name, revision)]
            path = Path(self.temp.name) / (name or f"{service.name}-{revision[:7]}.json")
            path.write_text(json.dumps({"schemaVersion": 2, "repository": service.image, "tag": f"sha-{revision}",
                                        "digest": image_digest, "image": f"{service.image}@{image_digest}",
                                        "sourceRevision": revision, "platform": "linux/amd64",
                                        "build": {"workflow": "w", "runId": 1, "runNumber": 2,
                                                  "url": "https://code.tilo.so/tilo/tilo/actions/runs/2"}}))
            return str(path)

        infra = self.infra(self.deployed)
        out_dir = Path(self.temp.name) / "receipts"
        code, _, err = self.run_tool("plan", "--infra", str(infra), "--out", str(out_dir),
                                     "--receipt", receipt(WEB, self.target), "--receipt", receipt(ROOM, self.target))
        self.assertEqual(code, 0, err)
        self.assertIn("actions/runs/2", (out_dir / "pr-body.md").read_text())
        code, _, err = self.run_tool("plan", "--infra", str(infra), "--receipt", receipt(WEB, self.target),
                                     "--receipt", receipt(ROOM, self.deployed))
        self.assertEqual(code, 1)
        self.assertIn("different revisions", err)
        # A receipt naming the deployed image under the new revision is caught by the release tag.
        lying = receipt(ROOM, self.target, self.fake.digests[("room-service", self.deployed)], name="lying.json")
        code, _, err = self.run_tool("plan", "--infra", str(infra), "--receipt", receipt(WEB, self.target),
                                     "--receipt", lying)
        self.assertEqual(code, 1)
        self.assertIn(f"not the receipt's {self.fake.digests[('room-service', self.deployed)]}", err)
        code, _, err = self.run_tool("plan", "--infra", str(infra), "--receipt", receipt(WEB, self.target))
        self.assertEqual(code, 1)
        self.assertIn("both images", err)

    def test_receipts_from_two_revisions_promote_the_newer_when_the_older_image_is_unchanged(self):
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
        self.git("checkout", "-q", "main")
        room = commit({"t.toml": checks, "crates/room/lib.rs": "1"}, "room service")
        web = commit({"crates/browser/lib.rs": "2"}, "web only")
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.fake.publish(ROOM, room)
        self.fake.publish(WEB, web)

        def receipt(service, revision):
            image_digest = self.fake.digests[(service.name, revision)]
            path = Path(self.temp.name) / f"{service.name}-{revision[:7]}.json"
            path.write_text(json.dumps({"schemaVersion": 3, "repository": service.image, "digest": image_digest,
                                        "image": f"{service.image}@{image_digest}", "tags": [f"sha-{revision}"],
                                        "sourceRevision": revision, "reference": "stable", "signed": True}))
            return str(path)

        infra = self.infra(self.deployed)
        out_dir = Path(self.temp.name) / "two"
        code, _, err = self.run_tool("plan", "--infra", str(infra), "--out", str(out_dir),
                                     "--receipt", receipt(ROOM, room), "--receipt", receipt(WEB, web))
        self.assertEqual(code, 0, err)
        plan = json.loads((out_dir / "plan.json").read_text())
        self.assertEqual(plan["revision"], web)
        self.assertEqual(plan["images"]["room-service"]["digest"], self.fake.digests[("room-service", room)])
        self.assertEqual(plan["images"]["room-service"]["sourceRevision"], room)

        # Once the room service's own files change, its older receipt no longer stands for the newer commit.
        changed = commit({"crates/room/lib.rs": "3"}, "room service changed")
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.fake.publish(WEB, changed)
        code, _, err = self.run_tool("plan", "--infra", str(infra),
                                     "--receipt", receipt(ROOM, room), "--receipt", receipt(WEB, changed))
        self.assertEqual(code, 1)
        self.assertIn("files it is built from changed", err)

    def test_images_without_a_revision_label_are_taken_from_their_receipts(self):
        config = json.dumps({"os": "linux", "architecture": "amd64", "config": {"Labels": {}}}).encode()
        manifest = json.dumps({"schemaVersion": 2,
                               "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
                               "config": {"digest": digest(config)}, "layers": []}).encode()
        base = f"https://{promotion.REGISTRY}/v2/{ROOM.repository}"
        self.fake.routes[f"{base}/blobs/{digest(config)}"] = promotion.Response(200, {}, config)
        for reference in (f"sha-{self.target}", digest(manifest)):
            self.fake.routes[f"{base}/manifests/{reference}"] = promotion.Response(
                200, {"content-type": "application/vnd.docker.distribution.manifest.v2+json"}, manifest)
        receipts = []
        for service in promotion.SERVICES:
            image_digest = digest(manifest) if service is ROOM else self.fake.digests[(service.name, self.target)]
            path = Path(self.temp.name) / f"unlabelled-{service.name}.json"
            path.write_text(json.dumps({"repository": service.image, "digest": image_digest,
                                        "image": f"{service.image}@{image_digest}", "sourceRevision": self.target}))
            receipts += ["--receipt", str(path)]
        out_dir = Path(self.temp.name) / "unlabelled"
        code, _, err = self.run_tool("plan", "--infra", str(self.infra(self.deployed)), "--out", str(out_dir), *receipts)
        self.assertEqual(code, 0, err)
        plan = json.loads((out_dir / "plan.json").read_text())
        self.assertEqual(plan["images"]["room-service"]["sourceRevision"], self.target)

    def test_only_main_moves_forward_unless_a_rollback_is_asked_for(self):
        infra = self.infra(self.deployed)
        code, _, err = self.run_tool("plan", "--infra", str(infra), "--revision", self.side)
        self.assertEqual(code, 1)
        self.assertIn("not on origin/main", err)
        code, _, err = self.run_tool("plan", "--infra", str(infra), "--revision", self.newest)
        self.assertEqual(code, 1)
        self.assertIn("does not exist", err)
        ahead = self.infra(self.target)
        code, _, err = self.run_tool("plan", "--infra", str(ahead), "--revision", self.deployed)
        self.assertEqual(code, 1)
        self.assertIn("rollback", err)
        code, _, err = self.run_tool("plan", "--infra", str(ahead), "--revision", self.deployed, "--allow-rollback")
        self.assertEqual(code, 0, err)

    def test_registry_content_is_checked_against_the_digest(self):
        pinned = self.fake.digests[("room-service", self.target)]
        url = f"https://{promotion.REGISTRY}/v2/{ROOM.repository}/manifests/{pinned}"
        real = self.fake.routes[url]
        self.fake.routes[url] = promotion.Response(real.status, real.headers, real.body + b" ")
        receipts = []
        for service in promotion.SERVICES:
            image_digest = self.fake.digests[(service.name, self.target)]
            path = Path(self.temp.name) / f"{service.name}.json"
            path.write_text(json.dumps({"repository": service.image, "digest": image_digest,
                                        "image": f"{service.image}@{image_digest}", "sourceRevision": self.target}))
            receipts += ["--receipt", str(path)]
        code, _, err = self.run_tool("plan", "--infra", str(self.infra(self.deployed)), *receipts)
        self.assertEqual(code, 1)
        self.assertIn("served content hashing to", err)

    def test_the_in_region_registry_is_read_with_an_anonymous_pull_token(self):
        self.fake.token_required = True
        code, out, err = self.run_tool("plan", "--infra", str(self.infra(self.deployed)))
        self.assertEqual(code, 0, err)
        self.assertIn(self.fake.digests[("room-service", self.target)], out)
        token_requests = [url for _, url, _ in self.fake.calls if url.startswith(Fake.TOKEN_SERVICE)]
        self.assertEqual(sorted(token_requests), sorted(
            f"{Fake.TOKEN_SERVICE}?scope=repository%3A{service.repository.replace('/', '%2F')}%3Apull"
            f"&service={promotion.REGISTRY}" for service in promotion.SERVICES))

    def test_credentials_stay_with_the_host_they_were_sent_to_across_redirects(self):
        handler = promotion.SameHostAuthorization()
        registry = f"https://{promotion.REGISTRY}/v2/tilo/web-entry/blobs/sha256:{'0' * 64}"
        for target, kept in ((f"https://{promotion.REGISTRY}/v2/tilo/web-entry/blobs/other", True),
                             ("https://bucket.s3.us-west-2.amazonaws.com/blob?X-Amz-Signature=x", False)):
            request = promotion.urllib.request.Request(registry, headers={"Authorization": "Bearer t"})
            follow = handler.redirect_request(request, None, 307, "Temporary Redirect", {}, target)
            self.assertEqual(follow.has_header("Authorization"), kept, target)

    def test_an_image_for_another_platform_is_refused(self):
        self.fake.publish(ROOM, self.newest, architecture="arm64")
        code, _, err = self.run_tool("plan", "--infra", str(self.infra(self.deployed)), "--revision", self.newest)
        self.assertEqual(code, 1)
        self.assertIn("linux/arm64", err)

    def serve_infra(self, revision):
        api = f"{promotion.FORGE}/api/v1/repos/{promotion.INFRA_REPO}"
        for index, service in enumerate(promotion.SERVICES):
            text = manifest_text(service, self.fake.digests[(service.name, revision)])
            self.fake.serve(f"{api}/contents/{service.manifest}?ref=main",
                            {"content": base64.b64encode(text.encode()).decode(), "sha": f"blob{index}"})
        self.fake.serve(f"{api}/pulls?state=open&limit=50", [])
        return api

    def test_propose_opens_one_pull_request_on_a_new_branch_and_never_writes_main(self):
        code, _, err = self.run_tool("propose")
        self.assertEqual(code, 1)
        self.assertIn(promotion.TOKEN_ENV, err)
        api = self.serve_infra(self.deployed)
        os.environ[promotion.TOKEN_ENV] = "secret"
        code, out, err = self.run_tool("propose")
        self.assertEqual(code, 0, err)
        self.assertIn("Opened https://code.tilo.so/PodestaAI/infra/pulls/7", out)
        writes = [call for call in self.fake.calls if call[0] != "GET"]
        self.assertEqual([url for _, url, _ in writes], [f"{api}/contents", f"{api}/pulls"])
        commit, pull = writes[0][2], writes[1][2]
        branch = "promote/tilo-" + self.target[:12]
        self.assertEqual((commit["branch"], commit["new_branch"]), ("main", branch))
        self.assertEqual(sorted(each["path"] for each in commit["files"]), sorted(s.manifest for s in promotion.SERVICES))
        for each in commit["files"]:
            service = next(s for s in promotion.SERVICES if s.manifest == each["path"])
            text = base64.b64decode(each["content"]).decode()
            self.assertIn(f"{service.image}@{self.fake.digests[(service.name, self.target)]}", text)
            self.assertTrue(each["sha"].startswith("blob"))
        self.assertEqual((pull["base"], pull["head"]), ("main", branch))
        self.assertIn("(#12)", pull["body"])
        # Dispatched again while that pull request is open: nothing new is written.
        self.fake.serve(f"{api}/pulls?state=open&limit=50",
                        [{"head": {"ref": branch}, "html_url": "https://code.tilo.so/PodestaAI/infra/pulls/7"}])
        self.fake.calls.clear()
        code, out, _ = self.run_tool("propose")
        self.assertEqual(code, 0)
        self.assertIn("already open", out)
        self.assertEqual([call for call in self.fake.calls if call[0] != "GET"], [])
        with self.assertRaises(promotion.PromotionError):
            promotion.Forge(self.fake, "secret").propose("main", {}, "t", "b")

    def test_drift_reports_how_far_main_is_ahead_and_fails_when_it_cannot_see(self):
        summary = Path(self.temp.name) / "summary.md"
        report = Path(self.temp.name) / "drift.json"
        code, out, err = self.run_tool("drift", "--summary", str(summary), "--json", str(report))
        self.assertEqual(code, 0, err)
        document = json.loads(report.read_text())
        web = document["services"]["web-entry"]
        self.assertEqual((web["revision"], web["commitsBehind"], web["changesBehind"]), (self.deployed, 3, 3))
        self.assertIsNone(document["services"]["room-service"]["revision"])
        self.assertEqual(document["promotable"], self.target)
        self.assertIn("3 commits", summary.read_text())
        # A room service that reports its build is compared too.
        self.fake.serve(ROOM.endpoint, {"mode": "normal", "revision": self.target})
        code, _, _ = self.run_tool("drift", "--json", str(report))
        self.assertEqual(json.loads(report.read_text())["services"]["room-service"]["commitsBehind"], 1)
        # Pinned elsewhere than what runs: a rollout that did not happen.
        code, out, _ = self.run_tool("drift", "--infra", str(self.infra(self.target)))
        self.assertEqual(code, 1)
        self.assertIn("the rollout did not happen", out)
        # A revision that is not on main.
        self.fake.serve(WEB.endpoint, {"revision": self.side})
        code, out, _ = self.run_tool("drift")
        self.assertEqual(code, 1)
        self.assertIn("not on main", out)
        # Unobservable is a failure, not a skip.
        self.fake.serve(WEB.endpoint, {"error": "down"}, status=503)
        code, _, err = self.run_tool("drift")
        self.assertEqual(code, 1)
        self.assertIn("HTTP 503", err)
        self.fake.serve(WEB.endpoint, {"version": "x"})
        code, _, err = self.run_tool("drift")
        self.assertEqual(code, 1)
        self.assertIn("no valid revision", err)


class WorkflowContractTests(unittest.TestCase):
    def test_image_receipts_name_their_build_run(self):
        for name, artifact in (("web-entry.yml", "web-entry-image.json"),):
            text = (ROOT / ".forgejo/workflows" / name).read_text()
            self.assertIn("schemaVersion:2", text, name)
            self.assertIn("build:{workflow:$workflow,runId:($runId|tonumber),runNumber:($runNumber|tonumber),url:$runUrl}",
                          text, name)
            self.assertIn(artifact, text, name)

    def test_promotion_runs_only_on_dispatch_and_its_weekly_report(self):
        text = (ROOT / ".forgejo/workflows/cloud-promotion.yml").read_text()
        triggers = text.split("\njobs:", 1)[0]
        self.assertIn("workflow_dispatch:", triggers)
        self.assertIn("schedule:", triggers)
        self.assertNotIn("push:", triggers)
        self.assertNotIn("pull_request", triggers)
        self.assertIn("tools/release/cloud_promotion.py drift", text)
        self.assertIn("tools/release/cloud_promotion.py plan", text)
        self.assertIn("tools/release/cloud_promotion.py propose", text)
        # The infra token only ever reaches the tool, and only on a dispatch.
        self.assertEqual(text.count("secrets.INFRA_PROMOTION_TOKEN"), 2)
        self.assertIn("github.event_name == 'workflow_dispatch'", text)


if __name__ == "__main__":
    unittest.main()
