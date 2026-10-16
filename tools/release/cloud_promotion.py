#!/usr/bin/env python3
"""Promote the Tilo Cloud images from their receipts, and report production drift.

Production runs two images built from this repository: the web entry
(tilo.coffee) and the room service (eu1.tilo.coffee). The infra repository
pins both by digest. A promotion is a reviewed pull request there that moves
both pins to the images built from one main commit; the web client and the
room protocol change together, so the two are never pinned from different
revisions. This tool never deploys, never pushes and never writes to the
infra repository's main branch.

  plan     choose a main commit whose two images exist, verify them in the
           registry (digest, revision label, platform, and any receipts
           given) and write the promotion: the exact manifest change, the
           pull request's body and a machine-readable plan. With an infra
           checkout (--infra) and --apply it edits that checkout's two
           manifests and nothing else.
  propose  open that pull request against the infra repository through the
           Forgejo API, on a new `promote/` branch. Needs INFRA_PROMOTION_TOKEN,
           a token allowed to create branches and pull requests there; without
           one, `plan` prints the same change for an operator to apply.
  drift    report which revision each service runs, how far main is ahead of
           it, and the newest revision ready to promote.

The registry, the web entry's /version.json and the room service's /mode are
public, so everything except reading the infra pins through the API and
`propose` runs without credentials. A revision that cannot be observed is a
failure, not a skip.
"""

from __future__ import annotations

import argparse
import base64
import dataclasses
import datetime as dt
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from typing import Callable, Iterable, Mapping
import urllib.error
import urllib.parse
import urllib.request


REGISTRY = "registry.tilo.so"
FORGE = "https://code.tilo.so"
SOURCE_REPO = "tilo/tilo"
INFRA_REPO = "PodestaAI/infra"
INFRA_BRANCH = "main"
TOKEN_ENV = "INFRA_PROMOTION_TOKEN"
PLATFORM = "linux/amd64"
BRANCH_PREFIX = "promote/tilo-"
# How far back along main `plan` and `drift` look for a promotable revision.
SEARCH_DEPTH = 500
# Pull requests listed in a promotion's body before the rest is a link.
BODY_PULLS = 250
COMMIT = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
MERGE_SUBJECT = re.compile(r"Merge pull request '(.*)' \((#\d+)\) from \S+ into \S+")
MANIFEST_TYPES = (
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
)
INDEX_TYPES = (
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
)


class PromotionError(Exception):
    """Something the operator must look at; the message says what."""


@dataclasses.dataclass(frozen=True)
class Service:
    name: str
    title: str
    repository: str
    manifest: str
    endpoint: str

    @property
    def image(self) -> str:
        return f"{REGISTRY}/{self.repository}"

    def pin(self) -> re.Pattern[str]:
        return re.compile(rf"(image:\s*){re.escape(self.image)}@(sha256:[0-9a-f]{{64}})")


SERVICES = (
    Service("web-entry", "web entry", "tilo/web-entry", "apps/tilo-web/deployment.yaml",
            "https://tilo.coffee/version.json"),
    Service("room-service", "room service", "tilo/room-service", "apps/tilo-eu1/deployment.yaml",
            "https://eu1.tilo.coffee/mode"),
)


# --- HTTP ------------------------------------------------------------------

@dataclasses.dataclass(frozen=True)
class Response:
    status: int
    headers: Mapping[str, str]
    body: bytes

    def json(self):
        return json.loads(self.body.decode("utf-8"))


Transport = Callable[[str, str, Mapping[str, str], bytes | None], Response]


class SameHostAuthorization(urllib.request.HTTPRedirectHandler):
    """Follows redirects, but sends credentials only to the host they were meant for.

    The in-region registry answers blob reads with a redirect to a presigned
    storage URL, which refuses a request carrying a second credential.
    """

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        follow = super().redirect_request(req, fp, code, msg, headers, newurl)
        if follow is not None and urllib.parse.urlsplit(newurl).netloc != urllib.parse.urlsplit(req.full_url).netloc:
            follow.remove_header("Authorization")
        return follow


OPENER = urllib.request.build_opener(SameHostAuthorization)


def urllib_transport(method: str, url: str, headers: Mapping[str, str], body: bytes | None) -> Response:
    # The edge in front of the public endpoints refuses urllib's default agent.
    request = urllib.request.Request(url, data=body, method=method,
                                     headers={"User-Agent": "tilo-cloud-promotion/1", **headers})
    try:
        with OPENER.open(request, timeout=30) as reply:
            return Response(reply.status, {k.lower(): v for k, v in reply.headers.items()}, reply.read())
    except urllib.error.HTTPError as error:
        return Response(error.code, {k.lower(): v for k, v in error.headers.items()}, error.read())
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise PromotionError(f"{method} {url}: {error}") from error


# --- Registry --------------------------------------------------------------

@dataclasses.dataclass(frozen=True)
class Image:
    service: str
    digest: str
    revision: str
    platform: str
    build: Mapping[str, object] | None = None

    def reference(self, service: Service) -> str:
        return f"{service.image}@{self.digest}"


class Registry:
    """The image registry, read anonymously.

    From the internet the name reaches a public read-only mirror that needs
    no credentials. Inside the CI network the same name reaches the
    token-authenticated registry, whose token service grants anonymous
    callers `pull`: a 401 with a Bearer challenge is answered by fetching
    such a token and asking again.
    """

    def __init__(self, transport: Transport, host: str = REGISTRY):
        self.transport = transport
        self.base = f"https://{host}"
        self.tokens: dict[str, str] = {}

    def get(self, repository: str, path: str, accept: Iterable[str] = ()) -> Response:
        url = urllib.parse.urljoin(self.base, path)
        headers = {"Accept": ", ".join(accept)} if accept else {}
        if repository in self.tokens:
            headers["Authorization"] = f"Bearer {self.tokens[repository]}"
        reply = self.transport("GET", url, headers, None)
        if reply.status == 401 and repository not in self.tokens:
            token = self.pull_token(repository, reply.headers.get("www-authenticate", ""))
            if token:
                self.tokens[repository] = token
                reply = self.transport("GET", url, {**headers, "Authorization": f"Bearer {token}"}, None)
        return reply

    def pull_token(self, repository: str, challenge: str) -> str | None:
        """An anonymous pull token from the challenge's token service."""
        match = re.match(r"\s*Bearer\s+(.*)", challenge, re.IGNORECASE)
        if not match:
            return None
        params = dict(re.findall(r'(\w+)="([^"]*)"', match.group(1)))
        realm = params.get("realm", "")
        if urllib.parse.urlsplit(realm).scheme != "https":
            raise PromotionError(f"{repository}: the registry's token service {realm!r} is not HTTPS")
        query = {"scope": params.get("scope") or f"repository:{repository}:pull"}
        if params.get("service"):
            query["service"] = params["service"]
        reply = self.transport("GET", f"{realm}?{urllib.parse.urlencode(query)}", {}, None)
        if reply.status != 200:
            raise PromotionError(f"{repository}: the registry's token service answered HTTP {reply.status}")
        document = reply.json()
        token = document.get("token") or document.get("access_token")
        if not isinstance(token, str) or not token:
            raise PromotionError(f"{repository}: the registry's token service returned no token")
        return token

    def tags(self, repository: str) -> set[str]:
        tags: set[str] = set()
        path: str | None = f"/v2/{repository}/tags/list?n=1000"
        while path:
            reply = self.get(repository, path)
            if reply.status != 200:
                raise PromotionError(f"{repository}: listing tags answered HTTP {reply.status}")
            tags.update(reply.json().get("tags") or [])
            link = re.match(r'\s*<([^>]+)>\s*;\s*rel="?next"?', reply.headers.get("link", ""))
            path = link.group(1) if link else None
        return tags

    def image(self, service: Service, reference: str) -> Image:
        """The single-platform image behind a tag or digest, verified by content."""
        reply = self.get(service.repository, f"/v2/{service.repository}/manifests/{reference}",
                         MANIFEST_TYPES + INDEX_TYPES)
        if reply.status == 404:
            raise PromotionError(f"{service.image}:{reference} does not exist")
        if reply.status != 200:
            raise PromotionError(f"{service.image}:{reference}: HTTP {reply.status}")
        digest = "sha256:" + hashlib.sha256(reply.body).hexdigest()
        if DIGEST.fullmatch(reference) and digest != reference:
            raise PromotionError(f"{service.image}@{reference} served content hashing to {digest}")
        manifest = reply.json()
        media = manifest.get("mediaType") or reply.headers.get("content-type", "")
        if media in INDEX_TYPES:
            raise PromotionError(f"{service.image}:{reference} is a multi-platform index; "
                                 "production pins a single-platform image")
        config_digest = manifest.get("config", {}).get("digest", "")
        if not DIGEST.fullmatch(config_digest):
            raise PromotionError(f"{service.image}@{digest} has no image config")
        config = self.get(service.repository, f"/v2/{service.repository}/blobs/{config_digest}")
        if config.status != 200 or "sha256:" + hashlib.sha256(config.body).hexdigest() != config_digest:
            raise PromotionError(f"{service.image}@{digest}: its config blob is missing or altered")
        document = config.json()
        labels = (document.get("config") or {}).get("Labels") or {}
        # Images t builds carry no revision label: a build is reused for every later commit with
        # the same inputs, so the publishing run's receipt names the revision instead.
        revision = labels.get("org.opencontainers.image.revision") or ""
        if revision and not COMMIT.fullmatch(revision):
            raise PromotionError(f"{service.image}@{digest} carries a malformed source revision label")
        platform = f"{document.get('os')}/{document.get('architecture')}"
        return Image(service.name, digest, revision, platform)


# --- Git -------------------------------------------------------------------

class Git:
    def __init__(self, root: Path):
        self.root = root

    def run(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(["git", "-C", str(self.root), *args], text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if check and result.returncode:
            raise PromotionError(f"git {' '.join(args)}: {result.stderr.strip()}")
        return result

    def commit(self, ref: str) -> str:
        revision = self.run("rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}", check=False).stdout.strip()
        if not COMMIT.fullmatch(revision):
            raise PromotionError(f"{ref} is not a commit here; fetch main's full history first")
        return revision

    def contains(self, ancestor: str, descendant: str) -> bool:
        return self.run("merge-base", "--is-ancestor", ancestor, descendant, check=False).returncode == 0

    def first_parents(self, ref: str, limit: int) -> list[str]:
        return self.run("rev-list", "--first-parent", f"--max-count={limit}", ref).stdout.split()

    def count(self, old: str, new: str, *flags: str) -> int:
        return int(self.run("rev-list", "--count", *flags, f"{old}..{new}").stdout.strip())

    def committed(self, revision: str) -> dt.datetime:
        stamp = self.run("show", "-s", "--format=%cI", revision).stdout.strip()
        return dt.datetime.fromisoformat(stamp)

    def image_unchanged(self, service: Service, old: str, new: str) -> str | None:
        """Why `service`'s image built at `new` may differ from the one built at `old`, or None.

        t builds an image once per set of inputs and reuses it for later commits, so stable
        publishes an image again only when what it is built from changed. The image checks'
        definitions in t.toml and the files they read are what t keys that build by.
        """
        names = (f"image-{service.name}-context", f"image-{service.name}")

        def checks(revision: str) -> dict[str, dict]:
            shown = self.run("show", f"{revision}:t.toml", check=False)
            if shown.returncode:
                return {}
            try:
                document = tomllib.loads(shown.stdout)
            except tomllib.TOMLDecodeError:
                return {}
            return {each["name"]: each for each in document.get("check", []) if each.get("name") in names}

        before, after = checks(old), checks(new)
        if not after:
            return f"t.toml at {new[:12]} declares no image checks for it"
        if before != after:
            return f"its image checks in t.toml changed between {old[:12]} and {new[:12]}"
        inputs = sorted({glob for each in after.values() for glob in each.get("inputs", [])})
        if not inputs:
            return "its image checks list no inputs"
        diff = self.run("diff", "--quiet", old, new, "--", *(f":(glob){glob}" for glob in inputs), check=False)
        if diff.returncode == 1:
            return f"files it is built from changed between {old[:12]} and {new[:12]}"
        if diff.returncode:
            raise PromotionError(f"git diff {old[:12]} {new[:12]}: {diff.stderr.strip()}")
        return None

    def pulls(self, old: str, new: str) -> list[tuple[str, str]]:
        """Main's own commits between two revisions, newest first, as (short id, title)."""
        lines = self.run("log", "--first-parent", "--format=%h%x09%s", f"{old}..{new}").stdout.splitlines()
        merged = []
        for line in lines:
            short, _, subject = line.partition("\t")
            match = MERGE_SUBJECT.fullmatch(subject)
            merged.append((short, f"{match.group(1)} ({match.group(2)})" if match else subject))
        return merged


# --- Choosing and checking the images ------------------------------------

def load_receipt(path: Path) -> tuple[Service, Image]:
    """A digest receipt written by the image workflows."""
    try:
        receipt = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise PromotionError(f"{path}: {error}") from error
    service = next((each for each in SERVICES if receipt.get("repository") == each.image), None)
    if service is None:
        raise PromotionError(f"{path}: {receipt.get('repository')!r} is not a promoted image")
    digest, revision = receipt.get("digest", ""), receipt.get("sourceRevision", "")
    if not DIGEST.fullmatch(digest) or not COMMIT.fullmatch(revision):
        raise PromotionError(f"{path}: the receipt has no valid digest and source revision")
    if receipt.get("image") != f"{service.image}@{digest}":
        raise PromotionError(f"{path}: its image reference disagrees with its digest")
    build = receipt.get("build") if isinstance(receipt.get("build"), dict) else None
    return service, Image(service.name, digest, revision, receipt.get("platform", ""), build)


def receipts_from(paths: Iterable[str]) -> dict[str, Image]:
    receipts: dict[str, Image] = {}
    for path in paths:
        service, image = load_receipt(Path(path))
        if service.name in receipts:
            raise PromotionError(f"two receipts for the {service.title}")
        receipts[service.name] = image
    return receipts


def promotable(registry: Registry, git: Git, main: str, limit: int = SEARCH_DEPTH) -> str | None:
    """The newest commit on main with both images published, or None."""
    tags = [registry.tags(service.repository) for service in SERVICES]
    for revision in git.first_parents(main, limit):
        if all(f"sha-{revision}" in each for each in tags):
            return revision
    return None


def verified_images(registry: Registry, git: Git, revision: str, receipts: Mapping[str, Image]) -> dict[str, Image]:
    """Both images for `revision`, checked against the registry and any receipts.

    A receipt names the revision whose run published it; its `sha-<revision>` tag must hold the
    receipt's digest. A revision label, where the image has one, names the commit it was built
    at, which is that revision or, for a build t reused, an earlier one.
    """
    images = {}
    for service in SERVICES:
        receipt = receipts.get(service.name)
        published = receipt.revision if receipt else revision
        image = registry.image(service, receipt.digest if receipt else f"sha-{revision}")
        if receipt:
            tagged = registry.image(service, f"sha-{published}").digest
            if tagged != receipt.digest:
                raise PromotionError(f"{service.image}:sha-{published} is {tagged}, "
                                     f"not the receipt's {receipt.digest}")
        if image.revision and image.revision != published and not git.contains(image.revision, published):
            raise PromotionError(f"{image.reference(service)} is labelled {image.revision}, not {published}")
        if image.platform != PLATFORM:
            raise PromotionError(f"{image.reference(service)} is {image.platform}; production runs {PLATFORM}")
        images[service.name] = dataclasses.replace(image, revision=published,
                                                   build=receipt.build if receipt else None)
    return images


def receipts_revision(git: Git, receipts: Mapping[str, Image]) -> str | None:
    """The revision a set of receipts promotes: the newest of theirs.

    Stable publishes an image again only when what it is built from changed, so the two
    receipts of stable's latest images often name different commits. They promote the newer one
    only if the other image's inputs did not change in between, so both are what that commit
    builds.
    """
    if not receipts:
        return None
    if set(receipts) != {service.name for service in SERVICES}:
        raise PromotionError("give a receipt for both images, or none")
    revisions = {name: git.commit(image.revision) for name, image in receipts.items()}
    newest = next((each for each in revisions.values()
                   if all(git.contains(other, each) for other in revisions.values())), None)
    if newest is None:
        raise PromotionError("the receipts come from different revisions on diverged histories: "
                             + ", ".join(sorted(set(revisions.values()))))
    for name, revision in revisions.items():
        if revision != newest:
            why = git.image_unchanged(_service(name), revision, newest)
            if why:
                raise PromotionError(f"the receipts come from different revisions ({revision[:12]}, "
                                     f"{newest[:12]}) and the {_service(name).title} may differ: {why}")
    return newest


@dataclasses.dataclass(frozen=True)
class Pin:
    service: str
    digest: str
    revision: str | None
    source: str


def pins_from_manifests(texts: Mapping[str, str], source: str) -> dict[str, tuple[str, str]]:
    """Each service's pinned digest in the infra manifests, as (digest, text)."""
    pins = {}
    for service in SERVICES:
        text = texts.get(service.manifest)
        if text is None:
            raise PromotionError(f"{source}: {service.manifest} is missing")
        found = service.pin().findall(text)
        if len(found) != 1:
            raise PromotionError(f"{source}: {service.manifest} pins {service.image} "
                                 f"{len(found)} times; expected exactly one digest pin")
        pins[service.name] = found[0][1]
    return pins


def repin(text: str, service: Service, digest: str) -> str:
    return service.pin().sub(lambda match: f"{match.group(1)}{service.image}@{digest}", text)


def resolve_current(registry: Registry, digests: Mapping[str, str], source: str) -> dict[str, Pin]:
    current = {}
    for service in SERVICES:
        digest = digests[service.name]
        try:
            revision = registry.image(service, digest).revision
        except PromotionError:
            revision = None
        current[service.name] = Pin(service.name, digest, revision, source)
    return current


# --- Observing production ---------------------------------------------------

@dataclasses.dataclass(frozen=True)
class Observation:
    service: str
    revision: str | None
    note: str


def observe(transport: Transport, service: Service) -> Observation:
    """The revision a service says it runs. Unreachable or malformed is an error."""
    reply = transport("GET", service.endpoint, {"Accept": "application/json", "Cache-Control": "no-store"}, None)
    if reply.status != 200:
        raise PromotionError(f"{service.endpoint} answered HTTP {reply.status}")
    try:
        document = reply.json()
    except ValueError as error:
        raise PromotionError(f"{service.endpoint} did not answer JSON: {error}") from error
    revision = document.get("revision") if isinstance(document, dict) else None
    if revision is None and service.name == "room-service":
        # Images before the room service reported its build answer only {"mode": ...}.
        return Observation(service.name, None, "not reported by the running image (it predates the field)")
    if not isinstance(revision, str) or not COMMIT.fullmatch(revision):
        raise PromotionError(f"{service.endpoint} reports no valid revision: {document!r}")
    return Observation(service.name, revision, service.endpoint)


def observed_pins(registry: Registry, observations: Mapping[str, Observation]) -> dict[str, Pin]:
    """Current pins inferred from what runs, when the infra manifests cannot be read.

    Production pins the `sha-<revision>` images, so the digest behind that tag
    is the pin; a service that does not report its revision is assumed to be
    pinned from the same revision as the other.
    """
    known = [each.revision for each in observations.values() if each.revision]
    if not known:
        raise PromotionError("no service reports its revision; pass --infra or a token")
    pins = {}
    for service in SERVICES:
        revision = observations[service.name].revision or known[0]
        image = registry.image(service, f"sha-{revision}")
        pins[service.name] = Pin(service.name, image.digest, image.revision,
                                 "observed" if observations[service.name].revision else "assumed")
    return pins


# --- The infra repository through the Forgejo API -------------------------

class Forge:
    def __init__(self, transport: Transport, token: str, base: str = FORGE, repo: str = INFRA_REPO):
        self.transport = transport
        self.api = f"{base}/api/v1/repos/{repo}"
        self.headers = {"Authorization": f"token {token}", "Accept": "application/json",
                        "Content-Type": "application/json"}

    def call(self, method: str, path: str, payload: object | None = None, expect: Iterable[int] = (200,)) -> object:
        body = json.dumps(payload).encode() if payload is not None else None
        reply = self.transport(method, self.api + path, self.headers, body)
        if reply.status not in tuple(expect):
            raise PromotionError(f"{method} {self.api}{path}: HTTP {reply.status} {reply.body[:300]!r}")
        return reply.json() if reply.body else None

    def manifests(self, ref: str = INFRA_BRANCH) -> dict[str, tuple[str, str]]:
        """Each manifest's text and blob id on `ref`."""
        files = {}
        for service in SERVICES:
            path = urllib.parse.quote(service.manifest)
            document = self.call("GET", f"/contents/{path}?ref={urllib.parse.quote(ref)}")
            text = base64.b64decode(document["content"]).decode("utf-8")
            files[service.manifest] = (text, document["sha"])
        return files

    def open_pull(self, branch: str) -> str | None:
        pulls = self.call("GET", "/pulls?state=open&limit=50")
        return next((each["html_url"] for each in pulls or [] if each.get("head", {}).get("ref") == branch), None)

    def propose(self, branch: str, changes: Mapping[str, tuple[str, str]], title: str, body: str) -> str:
        """Commit `changes` (path -> (text, blob id)) to a new branch and open a pull request."""
        if branch == INFRA_BRANCH or not branch.startswith(BRANCH_PREFIX):
            raise PromotionError(f"refusing to write {branch}: promotions only create {BRANCH_PREFIX}* branches")
        files = [{"operation": "update", "path": path, "sha": sha,
                  "content": base64.b64encode(text.encode("utf-8")).decode("ascii")}
                 for path, (text, sha) in sorted(changes.items())]
        self.call("POST", "/contents", {"branch": INFRA_BRANCH, "new_branch": branch,
                                        "message": title, "files": files}, expect=(200, 201))
        pull = self.call("POST", "/pulls", {"base": INFRA_BRANCH, "head": branch, "title": title, "body": body},
                         expect=(201,))
        return pull["html_url"]


# --- The promotion ---------------------------------------------------------

@dataclasses.dataclass
class Plan:
    revision: str
    images: dict[str, Image]
    current: dict[str, Pin]
    pulls: list[tuple[str, str]]
    commits: int
    since: str | None
    files: dict[str, tuple[str, str]] | None  # path -> (old text, new text), when the manifests were read

    @property
    def changed(self) -> bool:
        return any(self.current[name].digest != image.digest for name, image in self.images.items())

    @property
    def branch(self) -> str:
        return f"{BRANCH_PREFIX}{self.revision[:12]}"

    @property
    def title(self) -> str:
        return f"tilo-eu1, tilo-web: promote main images from {self.revision[:12]}"

    def diff(self) -> str:
        if self.files is None:
            return ""
        return "".join(
            "".join(difflib.unified_diff(old.splitlines(True), new.splitlines(True), f"a/{path}", f"b/{path}"))
            for path, (old, new) in sorted(self.files.items()))

    def body(self) -> str:
        lines = [
            f"Moves both Tilo Cloud images to the ones built from {SOURCE_REPO} "
            f"[`{self.revision[:12]}`]({FORGE}/{SOURCE_REPO}/commit/{self.revision}), by digest. "
            "Both come from one commit: the web client and the room protocol change together.",
            "",
            "| Service | Manifest | From | To |",
            "|---|---|---|---|",
        ]
        for service in SERVICES:
            pin, image = self.current[service.name], self.images[service.name]
            was = f"`{pin.digest[:19]}…` ({_short(pin.revision)}, {pin.source})"
            lines.append(f"| {service.title} | `{service.manifest}` | {was} | `{image.digest}` |")
        lines += ["", f"Each image is what `{self.revision}` builds, its platform is `{PLATFORM}` (checked "
                      "in the registry by content), and a revision label, where it has one, is that "
                      "commit or one it builds identically from."]
        runs = [f"[{image.build.get('workflow')}]({image.build.get('url')})"
                for image in self.images.values() if image.build and image.build.get("url")]
        if runs:
            lines.append("Build runs: " + ", ".join(runs) + ".")
        lines += ["", "After the rollout, both of these report the new revision:", "",
                  "```sh", *(f"curl -s {service.endpoint}" for service in SERVICES), "```", ""]
        if self.since:
            compare = f"{FORGE}/{SOURCE_REPO}/compare/{self.since}...{self.revision}"
            lines += [f"### What ships: {len(self.pulls)} changes to main ({self.commits} commits)", "",
                      f"[Full comparison]({compare}).", ""]
            lines += [f"- {title} `{short}`" for short, title in self.pulls[:BODY_PULLS]]
            if len(self.pulls) > BODY_PULLS:
                lines.append(f"- … and {len(self.pulls) - BODY_PULLS} more in the comparison above")
        else:
            lines.append("The deployed revision is unknown, so the changes that ship are not listed.")
        if self.files is None:
            lines += ["", "### Applying it", "", "In an infra checkout, replace each pin:", ""]
            lines += [f"- `{service.manifest}`: `{self.current[service.name].digest}` → "
                      f"`{self.images[service.name].digest}`" for service in SERVICES]
            lines += ["", "or let the tool edit the checkout:", "", "```sh",
                      f"python3 tools/release/cloud_promotion.py plan --revision {self.revision} "
                      "--infra <infra checkout> --apply", "```"]
        return "\n".join(lines) + "\n"

    def document(self) -> dict:
        return {
            "schemaVersion": 1,
            "revision": self.revision,
            "branch": self.branch,
            "title": self.title,
            "changed": self.changed,
            "commits": self.commits,
            "since": self.since,
            "images": {name: {"image": f"{REGISTRY}/{_service(name).repository}@{image.digest}",
                              "digest": image.digest, "sourceRevision": image.revision,
                              "platform": image.platform, "manifest": _service(name).manifest,
                              "build": image.build}
                       for name, image in self.images.items()},
            "current": {name: dataclasses.asdict(pin) for name, pin in self.current.items()},
        }


def _service(name: str) -> Service:
    return next(each for each in SERVICES if each.name == name)


def _short(revision: str | None) -> str:
    return f"`{revision[:12]}`" if revision else "revision unknown"


def build_plan(registry: Registry, git: Git, main: str, revision: str | None,
               receipts: Mapping[str, Image], current: Mapping[str, Pin],
               texts: Mapping[str, str] | None, allow_rollback: bool = False) -> Plan:
    head = git.commit(main)
    from_receipts = receipts_revision(git, receipts)
    if from_receipts:
        chosen = from_receipts
        if revision and git.commit(revision) != chosen:
            raise PromotionError(f"--revision {revision} disagrees with the receipts ({chosen})")
    elif revision:
        chosen = git.commit(revision)
    else:
        found = promotable(registry, git, head)
        if found is None:
            raise PromotionError(f"no commit among main's last {SEARCH_DEPTH} has both images; dispatch "
                                 "publish-web-entry and publish-room-service on main")
        chosen = found
    if not git.contains(chosen, head):
        raise PromotionError(f"{chosen} is not on {main}; production only runs main")
    images = verified_images(registry, git, chosen, receipts)
    known = [pin.revision for pin in current.values() if pin.revision]
    oldest = None
    for each in known:
        if not git.contains(each, head):
            raise PromotionError(f"the deployed revision {each} is not on {main}")
        if not git.contains(each, chosen) and not allow_rollback:
            raise PromotionError(f"{chosen[:12]} does not contain the deployed {each[:12]}: that is a rollback "
                                 "(pass --allow-rollback if it is meant)")
        if oldest is None or git.contains(each, oldest):
            oldest = each
    pulls = git.pulls(oldest, chosen) if oldest and git.contains(oldest, chosen) else []
    commits = git.count(oldest, chosen) if oldest and git.contains(oldest, chosen) else 0
    files = None
    if texts is not None:
        files = {}
        for service in SERVICES:
            old = texts[service.manifest]
            files[service.manifest] = (old, repin(old, service, images[service.name].digest))
    return Plan(chosen, images, dict(current), pulls, commits, oldest, files)


# --- Drift -------------------------------------------------------------------

def drift_report(registry: Registry, git: Git, transport: Transport, main: str,
                 pins: Mapping[str, Pin] | None, now: dt.datetime,
                 receipts: Mapping[str, Image] | None = None) -> tuple[str, dict, list[str]]:
    """The markdown report, its JSON form and the problems that fail the run."""
    head = git.commit(main)
    problems: list[str] = []
    services = {}
    lines = [f"## Tilo Cloud drift: main is `{head[:12]}`", "",
             "| Service | Runs | Behind main | Deployed commit age | Pinned |", "|---|---|---|---|---|"]
    for service in SERVICES:
        seen = observe(transport, service)
        entry: dict[str, object] = {"endpoint": service.endpoint, "revision": seen.revision}
        if seen.revision is None:
            runs, behind, age = seen.note, "unknown", "unknown"
        elif not git.contains(seen.revision, head):
            problems.append(f"{service.title} runs {seen.revision}, which is not on main")
            runs, behind, age = f"`{seen.revision[:12]}` (not on main)", "n/a", "n/a"
        else:
            commits = git.count(seen.revision, head)
            changes = git.count(seen.revision, head, "--first-parent")
            days = (now - git.committed(seen.revision)).total_seconds() / 86400
            entry.update(commitsBehind=commits, changesBehind=changes, ageDays=round(days, 1))
            runs, behind, age = f"`{seen.revision[:12]}`", f"{commits} commits ({changes} changes)", f"{days:.1f} days"
        pinned = "not read"
        if pins is not None:
            pin = pins[service.name]
            entry["pinned"] = dataclasses.asdict(pin)
            pinned = f"`{pin.digest[:19]}…` ({_short(pin.revision)})"
            if seen.revision and pin.revision and pin.revision != seen.revision:
                problems.append(f"{service.title} runs {seen.revision[:12]} but the infra pin is "
                                f"{pin.revision[:12]}: the rollout did not happen or failed")
        services[service.name] = entry
        lines.append(f"| {service.title} | {runs} | {behind} | {age} | {pinned} |")
    ready = receipts_revision(git, receipts) if receipts else promotable(registry, git, head)
    lines.append("")
    if ready:
        lines.append(f"Newest revision ready to promote (both images published): `{ready[:12]}`, "
                     f"{git.count(ready, head)} commits behind main. Promote it with "
                     "`t dispatch cloud-promotion --ref stable`.")
    else:
        lines.append(f"No commit among main's last {SEARCH_DEPTH} has both images published.")
    for problem in problems:
        lines.append(f"\n**Problem:** {problem}")
    return "\n".join(lines) + "\n", {"main": head, "services": services, "promotable": ready,
                                      "problems": problems}, problems


# --- Command line --------------------------------------------------------------

def read_infra(args, transport: Transport) -> tuple[dict[str, str] | None, dict[str, tuple[str, str]] | None, str]:
    """The manifests' texts (and blob ids when read through the API), and where from."""
    if args.infra:
        root = Path(args.infra)
        texts = {}
        for service in SERVICES:
            try:
                texts[service.manifest] = (root / service.manifest).read_text(encoding="utf-8")
            except OSError as error:
                raise PromotionError(f"{root}: {error}") from error
        return texts, None, f"infra checkout {root}"
    token = os.environ.get(TOKEN_ENV, "")
    if token:
        files = Forge(transport, token).manifests()
        return {path: text for path, (text, _) in files.items()}, files, f"{INFRA_REPO}@{INFRA_BRANCH}"
    return None, None, "observed"


def current_pins(args, registry: Registry, transport: Transport):
    texts, files, source = read_infra(args, transport)
    if texts is not None:
        digests = pins_from_manifests(texts, source)
        return resolve_current(registry, digests, "pinned"), texts, files
    observations = {service.name: observe(transport, service) for service in SERVICES}
    return observed_pins(registry, observations), None, None


def command_plan(args, registry: Registry, git: Git, transport: Transport) -> int:
    receipts = receipts_from(args.receipt)
    current, texts, _ = current_pins(args, registry, transport)
    plan = build_plan(registry, git, args.main, args.revision, receipts, current, texts, args.allow_rollback)
    write_outputs(plan, args.out)
    if not plan.changed:
        print(f"Production already pins the images from {plan.revision}.")
        return 0
    if args.apply:
        if not args.infra or plan.files is None:
            raise PromotionError("--apply edits an infra checkout: pass --infra")
        for path, (_, new) in plan.files.items():
            (Path(args.infra) / path).write_text(new, encoding="utf-8")
        print(f"Edited {', '.join(sorted(plan.files))} in {args.infra}; review, commit and open the pull request.")
    return 0


def command_propose(args, registry: Registry, git: Git, transport: Transport) -> int:
    token = os.environ.get(TOKEN_ENV, "")
    if not token:
        raise PromotionError(f"{TOKEN_ENV} is not set; run `plan` and apply its change by hand")
    forge = Forge(transport, token)
    files = forge.manifests()
    texts = {path: text for path, (text, _) in files.items()}
    current = resolve_current(registry, pins_from_manifests(texts, INFRA_REPO), "pinned")
    receipts = receipts_from(args.receipt)
    plan = build_plan(registry, git, args.main, args.revision, receipts, current, texts, args.allow_rollback)
    write_outputs(plan, args.out)
    if not plan.changed:
        print(f"Production already pins the images from {plan.revision}; nothing to propose.")
        return 0
    existing = forge.open_pull(plan.branch)
    if existing:
        print(f"The promotion is already open: {existing}")
        return 0
    changes = {path: (new, files[path][1]) for path, (old, new) in (plan.files or {}).items() if old != new}
    url = forge.propose(plan.branch, changes, plan.title, plan.body())
    print(f"Opened {url}")
    return 0


def command_drift(args, registry: Registry, git: Git, transport: Transport) -> int:
    pins = None
    texts, _, source = read_infra(args, transport)
    if texts is not None:
        pins = resolve_current(registry, pins_from_manifests(texts, source), "pinned")
    report, document, problems = drift_report(registry, git, transport, args.main, pins,
                                              dt.datetime.now(dt.timezone.utc), receipts_from(args.receipt))
    print(report, end="")
    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as summary:
            summary.write(report)
    if args.json:
        Path(args.json).write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    if os.environ.get("GITHUB_ACTIONS") == "true":
        for name, entry in document["services"].items():
            if entry.get("commitsBehind"):
                print(f"::warning title=Production drift::{_service(name).title} runs "
                      f"{str(entry['revision'])[:12]}, {entry['commitsBehind']} commits behind main")
    return 1 if problems else 0


def write_outputs(plan: Plan, out: str | None) -> None:
    body = plan.body()
    if out:
        directory = Path(out)
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "plan.json").write_text(json.dumps(plan.document(), indent=2) + "\n", encoding="utf-8")
        (directory / "pr-body.md").write_text(f"# {plan.title}\n\n{body}", encoding="utf-8")
        if plan.files is not None:
            (directory / "infra.diff").write_text(plan.diff(), encoding="utf-8")
    print(f"# {plan.title}\n\n{body}")
    if plan.files is not None:
        print(plan.diff())


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    root.add_argument("--root", default=".", help="this repository's checkout, with main's history")
    root.add_argument("--main", default="origin/main", help="the main branch ref (default origin/main)")
    commands = root.add_subparsers(dest="command", required=True)
    for name in ("plan", "propose"):
        each = commands.add_parser(name)
        each.add_argument("--revision", help="the main commit to promote (default: the newest with both images)")
        each.add_argument("--receipt", action="append", default=[],
                          help="a digest receipt artifact; give both or none")
        each.add_argument("--allow-rollback", action="store_true", help="allow promoting an older revision")
        each.add_argument("--out", help="directory for plan.json, pr-body.md and infra.diff")
    plan = commands.choices["plan"]
    plan.add_argument("--infra", help="an infra checkout to read the pins from")
    plan.add_argument("--apply", action="store_true", help="edit the --infra checkout's two manifests")
    drift = commands.add_parser("drift")
    drift.add_argument("--infra", help="an infra checkout to compare the running revisions with")
    drift.add_argument("--summary", help="append the report to this file (the job summary)")
    drift.add_argument("--json", help="write the report as JSON here")
    drift.add_argument("--receipt", action="append", default=[],
                       help="the receipt of an image ready to promote; give both or none")
    return root


def main(argv: list[str] | None = None, transport: Transport = urllib_transport) -> int:
    args = parser().parse_args(argv)
    if args.command == "propose":
        args.infra = None
    git = Git(Path(args.root))
    registry = Registry(transport)
    command = {"plan": command_plan, "propose": command_propose, "drift": command_drift}[args.command]
    try:
        return command(args, registry, git, transport)
    except PromotionError as error:
        print(f"cloud_promotion: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
