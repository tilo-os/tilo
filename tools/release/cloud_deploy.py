#!/usr/bin/env python3
"""Deploy the images stable publishes to the hosts a GitOps repository names.

Stable's publishing checks push signed `sha-<commit>` images and write a
receipt for each (tools/ci/publish/oci-publish.sh). Which of them run where is
not decided here: a deploy target, a TOML file in a GitOps repository (by
default TILO_DEPLOY_CONFIG in TILO_DEPLOY_REPO), lists each service, the
image it runs, the manifest that pins that image by digest and a public URL
that reports the revision it runs. The repository's own reconciler (ArgoCD
for the infra repository) applies what its main branch pins.

  plan    choose one commit for every service of the target (from receipts,
          --revision, or the newest commit whose images all exist), verify
          each image in the registry (digest, revision label, platform) and
          write the change: plan.json, the commit message and the diff. It
          reads the target through the forge's API (TOKEN_ENV) or from a
          checkout (--infra), and with --apply edits that checkout.
  deploy  plan, then commit the new pins to the target repository's main
          branch through the forge's API, wait until every moved service
          reports its new revision and stays healthy, and if it does not,
          commit the previous pins back and wait for those. While the target
          says `rollout = false`, or with --dry-run, it stops before the
          commit and prints what it would do.

All services move together, from one commit: the web client and the room
protocol change together, so they are never pinned from different revisions.
A service whose build is the one it runs already keeps its pin. A revision
that cannot be observed is a failure, not a skip.

--qualification names the manifest that says what was qualified against this
release, and the deploy is held to every identity in it: each service's digest,
the guest the release publishes (digest and the commit it was built from), the
AMI, the backend and template revision, and the browser runtime release. A
manifest of any other release is refused, and so is one whose bytes are not the
manifest digest it names. Enabling a gated feature needs an observation of the
running release: a manifest that only observed this commit's source is refused
unless --allow-source says the deploy is not enabling one. The manifest is kept
beside the plan under its digest, so what a deploy was qualified by can be
checked afterwards.
"""

from __future__ import annotations

import argparse
import base64
import dataclasses
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib
from typing import Callable, Iterable, Mapping
import urllib.error
import urllib.parse
import urllib.request


FORGE = "https://code.tilo.so"
SOURCE_REPO = "tilo/tilo"
TOKEN_ENV = "INFRA_PROMOTION_TOKEN"
REPO_ENV = "TILO_DEPLOY_REPO"
CONFIG_ENV = "TILO_DEPLOY_CONFIG"
TARGET_BRANCH = "main"
DEFAULT_PLATFORM = "linux/amd64"
# How far back along main `plan` looks for a commit whose images all exist.
SEARCH_DEPTH = 500
# Changes listed in a deploy commit's message before the rest is a link.
BODY_PULLS = 100
# Seconds between two health probes.
PROBE_INTERVAL = 5
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


class Superseded(Exception):
    """The target runs a newer build already: a later run deployed it."""


@dataclasses.dataclass(frozen=True)
class Service:
    name: str
    image: str
    manifest: str
    health: str
    platform: str = DEFAULT_PLATFORM
    # Fields its health URL must answer with besides the revision (a room service's
    # `relay = "up"`): a service can answer while what it serves is down.
    expect: tuple[tuple[str, str], ...] = ()

    @property
    def registry(self) -> str:
        return self.image.split("/", 1)[0]

    @property
    def repository(self) -> str:
        return self.image.split("/", 1)[1]

    def pin(self) -> re.Pattern[str]:
        return re.compile(rf"(image:\s*){re.escape(self.image)}@(sha256:[0-9a-f]{{64}})")


@dataclasses.dataclass(frozen=True)
class Target:
    services: tuple[Service, ...]
    rollout: bool
    timeout: int
    settle: int
    # The tag each image of a commit is published under: stable's releases, unless a
    # development deploy takes main's `dev-<commit>` images (--tag).
    tag: str = "sha-{commit}"

    def service(self, name: str) -> Service:
        return next(each for each in self.services if each.name == name)


def load_target(text: str, source: str) -> Target:
    """A deploy target: which services run where, and whether to roll them out."""
    try:
        document = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise PromotionError(f"{source}: {error}") from error
    services = []
    for index, entry in enumerate(document.get("service") or []):
        missing = [key for key in ("name", "image", "manifest", "health") if not isinstance(entry.get(key), str)]
        if missing:
            raise PromotionError(f"{source}: service {index + 1} has no {', '.join(missing)}")
        expect = entry.get("expect", {})
        if not isinstance(expect, dict) or not all(isinstance(value, str) for value in expect.values()):
            raise PromotionError(f"{source}: {entry['name']}'s expect must map fields to strings")
        service = Service(entry["name"], entry["image"], entry["manifest"], entry["health"],
                          entry.get("platform", DEFAULT_PLATFORM), tuple(sorted(expect.items())))
        if "/" not in service.image or "@" in service.image or ":" in service.image.split("/", 1)[1]:
            raise PromotionError(f"{source}: {service.name}'s image {service.image!r} is not a repository")
        if urllib.parse.urlsplit(service.health).scheme != "https":
            raise PromotionError(f"{source}: {service.name}'s health URL is not https")
        services.append(service)
    if not services:
        raise PromotionError(f"{source} lists no [[service]]")
    if len({each.name for each in services}) != len(services) or len({each.image for each in services}) != len(services):
        raise PromotionError(f"{source}: each service needs its own name and image")
    rollout, timeout, settle = document.get("rollout"), document.get("timeout_secs", 900), document.get("settle_secs", 120)
    if not isinstance(rollout, bool):
        raise PromotionError(f"{source}: `rollout` must be true or false")
    if not (isinstance(timeout, int) and 60 <= timeout <= 1800 and isinstance(settle, int) and 0 <= settle <= 900):
        raise PromotionError(f"{source}: timeout_secs must be 60-1800 and settle_secs 0-900")
    return Target(tuple(services), rollout, timeout, settle)


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
                                     headers={"User-Agent": "tilo-cloud-deploy/1", **headers})
    try:
        with OPENER.open(request, timeout=30) as reply:
            return Response(reply.status, {k.lower(): v for k, v in reply.headers.items()}, reply.read())
    except urllib.error.HTTPError as error:
        return Response(error.code, {k.lower(): v for k, v in error.headers.items()}, error.read())
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise PromotionError(f"{method} {url}: {error}") from error


@dataclasses.dataclass(frozen=True)
class Image:
    service: str
    digest: str
    # The commit whose publishing run released it: its `sha-<revision>` tag.
    revision: str
    platform: str
    # The commit it was built at (its revision label), which the running service reports. t
    # reuses a build for later commits with the same inputs, so this may be older than
    # `revision`.
    built: str = ""
    signed: bool | None = None

    def reference(self, service: Service) -> str:
        return f"{service.image}@{self.digest}"


class Registry:
    """Image registries, read anonymously.

    From the internet registry.tilo.so reaches a public read-only mirror that
    needs no credentials. Inside the CI network the same name reaches the
    token-authenticated registry, whose token service grants anonymous
    callers `pull`: a 401 with a Bearer challenge is answered by fetching
    such a token and asking again.
    """

    def __init__(self, transport: Transport):
        self.transport = transport
        self.tokens: dict[str, str] = {}

    def get(self, service: Service, path: str, accept: Iterable[str] = ()) -> Response:
        url = f"https://{service.registry}/v2/{service.repository}/{path}"
        headers = {"Accept": ", ".join(accept)} if accept else {}
        if service.image in self.tokens:
            headers["Authorization"] = f"Bearer {self.tokens[service.image]}"
        reply = self.transport("GET", url, headers, None)
        if reply.status == 401 and service.image not in self.tokens:
            token = self.pull_token(service, reply.headers.get("www-authenticate", ""))
            if token:
                self.tokens[service.image] = token
                reply = self.transport("GET", url, {**headers, "Authorization": f"Bearer {token}"}, None)
        return reply

    def pull_token(self, service: Service, challenge: str) -> str | None:
        """An anonymous pull token from the challenge's token service."""
        match = re.match(r"\s*Bearer\s+(.*)", challenge, re.IGNORECASE)
        if not match:
            return None
        params = dict(re.findall(r'(\w+)="([^"]*)"', match.group(1)))
        realm = params.get("realm", "")
        if urllib.parse.urlsplit(realm).scheme != "https":
            raise PromotionError(f"{service.image}: the registry's token service {realm!r} is not HTTPS")
        query = {"scope": params.get("scope") or f"repository:{service.repository}:pull"}
        if params.get("service"):
            query["service"] = params["service"]
        reply = self.transport("GET", f"{realm}?{urllib.parse.urlencode(query)}", {}, None)
        if reply.status != 200:
            raise PromotionError(f"{service.image}: the registry's token service answered HTTP {reply.status}")
        document = reply.json()
        token = document.get("token") or document.get("access_token")
        if not isinstance(token, str) or not token:
            raise PromotionError(f"{service.image}: the registry's token service returned no token")
        return token

    def tags(self, service: Service) -> set[str]:
        tags: set[str] = set()
        path: str | None = "tags/list?n=1000"
        while path:
            reply = self.get(service, path)
            if reply.status != 200:
                raise PromotionError(f"{service.image}: listing tags answered HTTP {reply.status}")
            tags.update(reply.json().get("tags") or [])
            link = re.match(r'\s*<([^>]+)>\s*;\s*rel="?next"?', reply.headers.get("link", ""))
            path = link.group(1).split(f"/v2/{service.repository}/", 1)[-1] if link else None
        return tags

    def image(self, service: Service, reference: str) -> Image:
        """The single-platform image behind a tag or digest, verified by content."""
        reply = self.get(service, f"manifests/{reference}", MANIFEST_TYPES + INDEX_TYPES)
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
                                 "deployments pin a single-platform image")
        config_digest = manifest.get("config", {}).get("digest", "")
        if not DIGEST.fullmatch(config_digest):
            raise PromotionError(f"{service.image}@{digest} has no image config")
        config = self.get(service, f"blobs/{config_digest}")
        if config.status != 200 or "sha256:" + hashlib.sha256(config.body).hexdigest() != config_digest:
            raise PromotionError(f"{service.image}@{digest}: its config blob is missing or altered")
        document = config.json()
        labels = (document.get("config") or {}).get("Labels") or {}
        built = labels.get("org.opencontainers.image.revision") or ""
        if built and not COMMIT.fullmatch(built):
            raise PromotionError(f"{service.image}@{digest} carries a malformed source revision label")
        platform = f"{document.get('os')}/{document.get('architecture')}"
        return Image(service.name, digest, built, platform, built)


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
            raise PromotionError(f"{ref} is not a commit here; fetch the full history first")
        return revision

    def contains(self, ancestor: str, descendant: str) -> bool:
        return self.run("merge-base", "--is-ancestor", ancestor, descendant, check=False).returncode == 0

    def first_parents(self, ref: str, limit: int) -> list[str]:
        return self.run("rev-list", "--first-parent", f"--max-count={limit}", ref).stdout.split()

    def count(self, old: str, new: str) -> int:
        return int(self.run("rev-list", "--count", f"{old}..{new}").stdout.strip())

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
        """The branch's own commits between two revisions, newest first, as (short id, title)."""
        lines = self.run("log", "--first-parent", "--format=%h%x09%s", f"{old}..{new}").stdout.splitlines()
        merged = []
        for line in lines:
            short, _, subject = line.partition("\t")
            match = MERGE_SUBJECT.fullmatch(subject)
            merged.append((short, f"{match.group(1)} ({match.group(2)})" if match else subject))
        return merged


def load_receipt(path: Path, target: Target) -> tuple[Service, Image] | None:
    """A digest receipt written by t's publishing checks, or None for an image the target
    does not run."""
    try:
        receipt = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise PromotionError(f"{path}: {error}") from error
    service = next((each for each in target.services if receipt.get("repository") == each.image), None)
    if service is None:
        return None
    digest, revision = receipt.get("digest", ""), receipt.get("sourceRevision", "")
    if not DIGEST.fullmatch(digest) or not COMMIT.fullmatch(revision):
        raise PromotionError(f"{path}: the receipt has no valid digest and source revision")
    if receipt.get("image") != f"{service.image}@{digest}":
        raise PromotionError(f"{path}: its image reference disagrees with its digest")
    signed = receipt.get("signed") if isinstance(receipt.get("signed"), bool) else None
    return service, Image(service.name, digest, revision, receipt.get("platform", ""), signed=signed)


def receipts_from(paths: Iterable[str], target: Target) -> dict[str, Image]:
    receipts: dict[str, Image] = {}
    for path in paths:
        loaded = load_receipt(Path(path), target)
        if loaded is None:
            print(f"{path}: an image the deploy target does not run; ignored")
            continue
        service, image = loaded
        if service.name in receipts:
            raise PromotionError(f"two receipts for {service.name}")
        receipts[service.name] = image
    return receipts


def deployable(registry: Registry, git: Git, head: str, target: Target, limit: int = SEARCH_DEPTH) -> str | None:
    """The newest first-parent commit with every service's release image published, or None."""
    tags = [registry.tags(service) for service in target.services]
    for revision in git.first_parents(head, limit):
        if all(target.tag.format(commit=revision) in each for each in tags):
            return revision
    return None


def verified_images(registry: Registry, git: Git, revision: str, receipts: Mapping[str, Image],
                    target: Target) -> dict[str, Image]:
    """Every service's image for `revision`, checked against the registry and any receipts.

    A receipt names the revision whose run published it; its `sha-<revision>` tag must hold the
    receipt's digest. A revision label, where the image has one, names the commit it was built
    at, which is that revision or, for a build t reused, an earlier one.
    """
    images = {}
    for service in target.services:
        receipt = receipts.get(service.name)
        published = receipt.revision if receipt else revision
        image = registry.image(service, receipt.digest if receipt else target.tag.format(commit=revision))
        if receipt:
            tagged = registry.image(service, target.tag.format(commit=published)).digest
            if tagged != receipt.digest:
                raise PromotionError(f"{service.image}:{target.tag.format(commit=published)} is {tagged}, "
                                     f"not the receipt's {receipt.digest}")
        if image.built and image.built != published and not git.contains(image.built, published):
            raise PromotionError(f"{image.reference(service)} is labelled {image.built}, not {published}")
        if image.platform != service.platform:
            raise PromotionError(f"{image.reference(service)} is {image.platform}; "
                                 f"{service.name} runs {service.platform}")
        images[service.name] = dataclasses.replace(image, revision=published,
                                                   signed=receipt.signed if receipt else None)
    return images


def receipts_revision(git: Git, receipts: Mapping[str, Image], target: Target) -> str | None:
    """The revision a set of receipts deploys: the newest of theirs.

    Stable publishes an image again only when what it is built from changed, so the receipts of
    stable's latest images often name different commits. They deploy the newest one only if the
    other images' inputs did not change in between, so all are what that commit builds.
    """
    if not receipts:
        return None
    if set(receipts) != {service.name for service in target.services}:
        raise PromotionError("give a receipt for every service of the target, or none: "
                             + ", ".join(each.name for each in target.services))
    revisions = {name: git.commit(image.revision) for name, image in receipts.items()}
    newest = next((each for each in revisions.values()
                   if all(git.contains(other, each) for other in revisions.values())), None)
    if newest is None:
        raise PromotionError("the receipts come from different revisions on diverged histories: "
                             + ", ".join(sorted(set(revisions.values()))))
    for name, revision in revisions.items():
        if revision != newest:
            why = git.image_unchanged(target.service(name), revision, newest)
            if why:
                raise PromotionError(f"the receipts come from different revisions ({revision[:12]}, "
                                     f"{newest[:12]}) and {name} may differ: {why}")
    return newest


@dataclasses.dataclass(frozen=True)
class Pin:
    service: str
    digest: str
    # The pinned image's revision label: what the service reports while it runs it.
    built: str | None


def pins_from_manifests(texts: Mapping[str, str], target: Target, source: str) -> dict[str, str]:
    """Each service's pinned digest in the target's manifests."""
    pins = {}
    for service in target.services:
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


def resolve_current(registry: Registry, digests: Mapping[str, str], target: Target) -> dict[str, Pin]:
    current = {}
    for service in target.services:
        digest = digests[service.name]
        try:
            built = registry.image(service, digest).built or None
        except PromotionError:
            built = None
        current[service.name] = Pin(service.name, digest, built)
    return current


@dataclasses.dataclass
class Plan:
    revision: str
    images: dict[str, Image]
    current: dict[str, Pin]
    # The services whose pin moves; the others run this build already.
    moves: list[str]
    pulls: list[tuple[str, str]]
    commits: int
    since: str | None
    files: dict[str, tuple[str, str]]  # path -> (old text, new text)
    target: Target
    # The manifest the release was qualified against, when the deploy was held to one.
    qualification: Qualification | None = None

    @property
    def changed(self) -> bool:
        return bool(self.moves)

    @property
    def title(self) -> str:
        apps = sorted({Path(self.target.service(name).manifest).parent.name for name in self.moves})
        return f"{', '.join(apps) or 'tilo'}: deploy {SOURCE_REPO} {self.revision[:12]}"

    def diff(self) -> str:
        return "".join(
            "".join(difflib.unified_diff(old.splitlines(True), new.splitlines(True), f"a/{path}", f"b/{path}"))
            for path, (old, new) in sorted(self.files.items()) if old != new)

    def message(self) -> str:
        lines = [self.title, "",
                 f"{FORGE}/{SOURCE_REPO}/commit/{self.revision}, its images tagged "
                 f"{self.target.tag.format(commit='<commit>')}.", ""]
        for name in self.moves:
            pin, image = self.current[name], self.images[name]
            lines.append(f"{name}: {pin.digest[:19]}… ({_short(pin.built)}) -> {image.digest} "
                         f"(built at {_short(image.built)})")
        kept = [name for name in self.images if name not in self.moves]
        if kept:
            lines.append(f"Kept, already running this build: {', '.join(kept)}.")
        if self.since:
            lines += ["", f"{len(self.pulls)} changes ({self.commits} commits) since {self.since[:12]}: "
                          f"{FORGE}/{SOURCE_REPO}/compare/{self.since}...{self.revision}"]
            lines += [f"- {title} {short}" for short, title in self.pulls[:BODY_PULLS]]
            if len(self.pulls) > BODY_PULLS:
                lines.append(f"- and {len(self.pulls) - BODY_PULLS} more")
        return "\n".join(lines) + "\n"

    def expected(self) -> dict[str, str | None]:
        """What each moved service reports once it runs its new image."""
        return {name: self.images[name].built or None for name in self.moves}

    def document(self) -> dict:
        return {
            "schemaVersion": 2,
            "revision": self.revision,
            "title": self.title,
            "changed": self.changed,
            "moves": self.moves,
            "commits": self.commits,
            "since": self.since,
            "images": {name: {"image": image.reference(self.target.service(name)), "digest": image.digest,
                              "sourceRevision": image.revision, "built": image.built,
                              "platform": image.platform, "manifest": self.target.service(name).manifest,
                              "health": self.target.service(name).health}
                       for name, image in self.images.items()},
            "current": {name: dataclasses.asdict(pin) for name, pin in self.current.items()},
            "rollout": {"enabled": self.target.rollout, "timeoutSecs": self.target.timeout,
                        "settleSecs": self.target.settle},
            "qualification": (None if self.qualification is None else
                              {"manifest": self.qualification.manifest,
                               "path": str(self.qualification.path),
                               "sourceRevision": self.qualification.source_revision,
                               "observation": self.qualification.observation}),
        }


def _short(revision: str | None) -> str:
    return revision[:12] if revision else "revision unknown"


# What a qualification can rest on, strongest first. Turning on a gated feature needs an
# observation of the running release; a source-only fixture says what the code should do,
# never what the release about to be deployed does.
QUALIFICATION_OBSERVATIONS = ("native", "live", "source")
QUALIFICATION_EVIDENCE = ("native", "live")

# The fields each identity in a qualification manifest carries. An identity is normative
# evidence, so an unknown field is refused rather than ignored: a manifest that names a part
# of the release nobody checks has not qualified it.
QUALIFICATION_IDENTITIES = {
    "guest": {"digest": DIGEST, "sourceRevision": COMMIT},
    "ami": {"id": re.compile(r"ami-[0-9a-f]{8,17}"), "type": re.compile(r"[a-z0-9][a-z0-9.-]{0,63}")},
    "backend": {"revision": COMMIT},
    "template": {"revision": COMMIT},
    "browserRuntime": {"version": re.compile(r"[0-9A-Za-z][0-9A-Za-z._+-]{0,31}")},
}
QUALIFICATION_SERVICES = ("room-service", "web-entry")


class Qualification:
    """What was qualified against one release, and how.

    A deploy moves a target's services together, so the evidence that says the release works
    has to be about the same parts of it: the guest a deployment boots -- its digest and the
    commit it was built from -- the AMI, the backend and template revision, the browser
    runtime release, and the limits the run was accepted for. `sourceRevision` is the
    repository commit the evidence is about. `services` names, per service, the image digest
    the evidence covered, so a deploy of any other image is not qualified by it.

    `manifest` is the digest of the manifest's own bytes, every field above `manifest` aside,
    so editing one field cannot go unnoticed and the file can be kept and compared under the
    name the deploy refers to.
    """

    def __init__(self, path: Path, document: Mapping):
        self.path = path
        self.document = document

    @property
    def source_revision(self) -> str:
        return self.document["sourceRevision"]

    @property
    def observation(self) -> str:
        return self.document["observation"]

    @property
    def manifest(self) -> str:
        return self.document["manifest"]

    def check(self, service: Service, image: Image) -> None:
        """The manifest qualified this service's exact release, or this is an error.

        Three identities have to agree, and they are three different claims: the service's own
        digest is the image being deployed; the guest digest and the commit the guest was
        built from are the run the evidence came from. A manifest that names this build but
        another guest has measured something else.
        """
        qualified = self.document["services"].get(service.name, "")
        if qualified != image.digest:
            raise PromotionError(
                f"{self.path}: qualified {service.name} at {qualified or 'no digest'}, not at "
                f"{image.digest}; a release deploys with the evidence of that release")
        guest = self.document["identities"]["guest"]
        if guest["sourceRevision"] != image.revision:
            raise PromotionError(
                f"{self.path}: qualified {image.reference(service)} from {guest['sourceRevision']} "
                f"(guest {guest['digest']}), not from {image.revision}")


def manifest_digest(document: Mapping) -> str:
    """The digest a qualification manifest is kept and referred to under."""
    body = {key: value for key, value in document.items() if key != "manifest"}
    encoded = json.dumps(body, sort_keys=True, separators=(",", ":"))
    return "sha256:" + hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _check_identity(path: Path, name: str, identity, fields: Mapping) -> None:
    if not isinstance(identity, dict) or set(identity) != set(fields):
        raise PromotionError(f"{path}: the {name} identity names "
                             f"{sorted(identity) if isinstance(identity, dict) else repr(identity)}, "
                             f"not {sorted(fields)}")
    for field, pattern in fields.items():
        if not isinstance(identity[field], str) or not pattern.fullmatch(identity[field]):
            raise PromotionError(f"{path}: the {name} identity's {field} {identity[field]!r} is not one")


def load_qualification(path: Path) -> Qualification:
    """A qualification manifest, read and held to the shape every deploy compares against."""
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise PromotionError(f"{path}: {error}") from error
    if not isinstance(document, dict):
        raise PromotionError(f"{path}: a qualification manifest is a JSON object")
    needed = ("identities", "services", "limits", "sourceRevision", "observation", "manifest")
    missing = [key for key in needed if key not in document]
    if missing:
        raise PromotionError(f"{path}: the qualification manifest has no {', '.join(missing)}")
    if set(document) - set(needed):
        raise PromotionError(f"{path}: the qualification manifest names "
                             f"{sorted(set(document) - set(needed))}, which no deploy reads")
    if not COMMIT.fullmatch(document["sourceRevision"]):
        raise PromotionError(f"{path}: sourceRevision is not a full commit")
    if document["observation"] not in QUALIFICATION_OBSERVATIONS:
        raise PromotionError(f"{path}: observation is {document['observation']!r}, not one of "
                             f"{', '.join(QUALIFICATION_OBSERVATIONS)}")
    identities = document["identities"]
    if not isinstance(identities, dict) or set(identities) != set(QUALIFICATION_IDENTITIES):
        raise PromotionError(f"{path}: the manifest names "
                             f"{sorted(identities) if isinstance(identities, dict) else repr(identities)}, "
                             f"not {sorted(QUALIFICATION_IDENTITIES)}")
    for name, fields in QUALIFICATION_IDENTITIES.items():
        _check_identity(path, name, identities[name], fields)
    services = document["services"]
    if not isinstance(services, dict) or not services:
        raise PromotionError(f"{path}: the qualification manifest qualified no service")
    for name, digest in services.items():
        if name not in QUALIFICATION_SERVICES:
            raise PromotionError(f"{path}: {name!r} is not a service a deploy runs")
        if not isinstance(digest, str) or not DIGEST.fullmatch(digest):
            raise PromotionError(f"{path}: the qualified {name} digest {digest!r} is not one")
    limits = document["limits"]
    if not isinstance(limits, dict) or not limits:
        raise PromotionError(f"{path}: the qualification manifest names no limit")
    for name, limit in limits.items():
        if not isinstance(limit, dict) or set(limit) != {"value", "unit"}:
            raise PromotionError(f"{path}: the {name} limit is not a value and its unit")
        if not isinstance(limit["value"], int) or isinstance(limit["value"], bool) or limit["value"] < 0:
            raise PromotionError(f"{path}: the {name} limit is not a count")
        if not isinstance(limit["unit"], str) or not limit["unit"].isidentifier():
            raise PromotionError(f"{path}: the {name} limit's unit {limit['unit']!r} is not a unit")
    if document["manifest"] != manifest_digest(document):
        raise PromotionError(f"{path}: its bytes hash to {manifest_digest(document)}, "
                             f"not the {document['manifest']} it names")
    return Qualification(path, document)


# The guest a deployment boots, published and pinned like any other product: the release's
# `sha-<commit>` tag holds the guest that release builds, so the evidence can name the exact
# one without trusting the manifest to say which release it belongs to.
GUEST_IMAGE = "registry.tilo.so/tilo/legacy-guest"


def guest_of(registry: Registry, revision: str, tag: str) -> str:
    """The digest of the guest this release publishes."""
    return registry.image(Service("legacy-guest", GUEST_IMAGE, "", ""), tag.format(commit=revision)).digest


def gate_qualification(qualification: Qualification, images: Mapping[str, Image],
                       target: Target, guest: str, allow_source: bool) -> None:
    """Hold the evidence to the release about to be deployed, and to what it rests on.

    A manifest qualifies the release it names and no other: every service's exact digest and
    the commit the guest was built from are compared before anything is deployed. Turning on
    a gated feature needs an observation of the running release, so a source-only fixture
    cannot get there whatever else it names.
    """
    for name, image in images.items():
        qualification.check(target.service(name), image)
    qualified = qualification.document["identities"]["guest"]
    if qualified["digest"] != guest:
        raise PromotionError(
            f"{qualification.path}: qualified guest {qualified['digest']}, but "
            f"{next(iter(images.values())).revision} publishes {guest}; the evidence "
            "is for a guest this release does not boot")
    if qualification.observation in QUALIFICATION_EVIDENCE or allow_source:
        return
    raise PromotionError(
        f"{qualification.path}: it observed {qualification.source_revision} from source only; "
        f"enabling a gated feature needs a {', '.join(QUALIFICATION_EVIDENCE)} observation of the release")


def write_qualification(qualification: Qualification, out: Path) -> Path:
    """Keep the manifest under the digest the deploy names it by, beside the plan."""
    kept = out / f"{qualification.manifest.split(':', 1)[1]}.qualification.json"
    kept.write_text(json.dumps(qualification.document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return kept


def build_plan(registry: Registry, git: Git, head_ref: str, revision: str | None,
               receipts: Mapping[str, Image], current: Mapping[str, Pin],
               texts: Mapping[str, str], target: Target, allow_rollback: bool = False) -> Plan:
    head = git.commit(head_ref)
    from_receipts = receipts_revision(git, receipts, target)
    if from_receipts:
        chosen = from_receipts
        if revision and git.commit(revision) != chosen:
            raise PromotionError(f"--revision {revision} disagrees with the receipts ({chosen})")
    elif revision:
        chosen = git.commit(revision)
    else:
        found = deployable(registry, git, head, target)
        if found is None:
            raise PromotionError(f"no commit among {head_ref}'s last {SEARCH_DEPTH} has every service's "
                                 "sha-<commit> image; stable publishes them")
        chosen = found
    if not git.contains(chosen, head):
        raise PromotionError(f"{chosen} is not on {head_ref}")
    images = verified_images(registry, git, chosen, receipts, target)
    oldest = None
    for pin in current.values():
        if not pin.built:
            continue
        if not git.contains(pin.built, head):
            raise PromotionError(f"the deployed build {pin.built} is not on {head_ref}")
        if not git.contains(pin.built, chosen) and not allow_rollback:
            if git.contains(chosen, pin.built):
                raise Superseded(f"{pin.service} runs {pin.built[:12]}, newer than {chosen[:12]}")
            raise PromotionError(f"{chosen[:12]} does not contain the deployed {pin.built[:12]}: that is a "
                                 "rollback (pass --allow-rollback if it is meant)")
        if oldest is None or git.contains(pin.built, oldest):
            oldest = pin.built
    moves = [service.name for service in target.services
             if current[service.name].digest != images[service.name].digest
             and not (images[service.name].built and current[service.name].built == images[service.name].built)]
    ahead = bool(oldest) and git.contains(oldest, chosen)
    pulls = git.pulls(oldest, chosen) if ahead else []
    commits = git.count(oldest, chosen) if ahead else 0
    files: dict[str, tuple[str, str]] = {}
    for service in target.services:
        old = files.get(service.manifest, (texts[service.manifest],) * 2)
        new = repin(old[1], service, images[service.name].digest) if service.name in moves else old[1]
        files[service.manifest] = (old[0], new)
    return Plan(chosen, images, dict(current), moves, pulls, commits, oldest if ahead else None, files, target)


class Forge:
    def __init__(self, transport: Transport, token: str, repo: str, base: str = FORGE):
        self.transport = transport
        self.repo = repo
        self.api = f"{base}/api/v1/repos/{repo}"
        self.headers = {"Authorization": f"token {token}", "Accept": "application/json",
                        "Content-Type": "application/json"}

    def call(self, method: str, path: str, payload: object | None = None, expect: Iterable[int] = (200,)) -> object:
        body = json.dumps(payload).encode() if payload is not None else None
        reply = self.transport(method, self.api + path, self.headers, body)
        if reply.status not in tuple(expect):
            raise PromotionError(f"{method} {self.api}{path}: HTTP {reply.status} {reply.body[:300]!r}")
        return reply.json() if reply.body else None

    def file(self, path: str) -> tuple[str, str]:
        """A file's text and blob id on the target branch."""
        document = self.call("GET", f"/contents/{urllib.parse.quote(path)}?ref={TARGET_BRANCH}")
        return base64.b64decode(document["content"]).decode("utf-8"), document["sha"]

    def commit(self, changes: Mapping[str, tuple[str, str]], message: str) -> bool:
        """Commit `changes` (path -> (text, blob id it replaces)) to the target branch in one
        commit. False when one of the files moved on since it was read."""
        files = [{"operation": "update", "path": path, "sha": sha,
                  "content": base64.b64encode(text.encode("utf-8")).decode("ascii")}
                 for path, (text, sha) in sorted(changes.items())]
        body = json.dumps({"branch": TARGET_BRANCH, "message": message, "files": files}).encode()
        reply = self.transport("POST", f"{self.api}/contents", self.headers, body)
        if reply.status in (200, 201):
            return True
        if reply.status in (409, 422):
            return False
        raise PromotionError(f"POST {self.api}/contents: HTTP {reply.status} {reply.body[:300]!r}")


@dataclasses.dataclass
class Source:
    """Where the target and its manifests were read from."""
    name: str
    target: Target
    texts: dict[str, str]
    blobs: dict[str, str] | None  # path -> blob id, when read through the forge
    forge: Forge | None


def read_source(args, transport: Transport) -> Source:
    if not args.config:
        raise PromotionError(f"name the deploy target's path with --config or {CONFIG_ENV}")
    if args.infra:
        root = Path(args.infra)
        try:
            target = dataclasses.replace(load_target((root / args.config).read_text(encoding="utf-8"),
                                                     f"{root}/{args.config}"), tag=args.tag)
            texts = {service.manifest: (root / service.manifest).read_text(encoding="utf-8")
                     for service in target.services}
        except OSError as error:
            raise PromotionError(f"{root}: {error}") from error
        return Source(f"checkout {root}", target, texts, None, None)
    token = os.environ.get(TOKEN_ENV, "")
    if not token or not args.repo:
        raise PromotionError(f"reading the deploy target needs {TOKEN_ENV} and {REPO_ENV} (or --repo), "
                             "or an --infra checkout")
    forge = Forge(transport, token, args.repo)
    target = dataclasses.replace(load_target(forge.file(args.config)[0], f"{args.repo}:{args.config}"),
                                 tag=args.tag)
    texts, blobs = {}, {}
    for service in target.services:
        texts[service.manifest], blobs[service.manifest] = forge.file(service.manifest)
    return Source(f"{args.repo}@{TARGET_BRANCH}", target, texts, blobs, forge)


def plan_from(args, source: Source, registry: Registry, git: Git) -> Plan:
    target = source.target
    current = resolve_current(registry, pins_from_manifests(source.texts, target, source.name), target)
    receipts = receipts_from(args.receipt, target)
    unsigned = [name for name, image in receipts.items() if image.signed is not True]
    if args.require_signed and unsigned:
        raise PromotionError("deploys run signed stable releases; unsigned: " + ", ".join(unsigned))
    plan = build_plan(registry, git, args.head, args.revision, receipts, current, source.texts, target,
                      args.allow_rollback)
    if args.qualification:
        qualification = load_qualification(Path(args.qualification))
        guest = guest_of(registry, plan.revision, target.tag)
        gate_qualification(qualification, plan.images, target, guest, args.allow_source)
        plan = dataclasses.replace(plan, qualification=qualification)
    return plan


def probe(transport: Transport, service: Service, expected: str | None, strict: bool = True) -> str | None:
    """None when the service answers healthy with the expected build, else what is wrong.

    `strict` requires every field of the service's `expect`. Otherwise a field the build does
    not report passes (an older build rolled back to may predate it), and only a different
    value fails."""
    try:
        reply = transport("GET", service.health, {"Accept": "application/json", "Cache-Control": "no-store"}, None)
    except PromotionError as error:
        return str(error)
    if reply.status != 200:
        return f"{service.health} answered HTTP {reply.status}"
    try:
        document = reply.json()
    except ValueError:
        return f"{service.health} did not answer JSON"
    revision = document.get("revision") if isinstance(document, dict) else None
    if expected and revision != expected:
        return f"{service.health} reports {revision or 'no revision'}, not {expected[:12]}"
    for field, value in service.expect:
        reported = document.get(field) if isinstance(document, dict) else None
        if reported != value and (strict or reported is not None):
            return f"{service.health} reports {field} {reported!r}, not {value!r}"
    return None


def watch(transport: Transport, target: Target, expected: Mapping[str, str | None],
          clock: Callable[[], float], sleep: Callable[[float], None], strict: bool = True) -> str | None:
    """Wait until every service in `expected` reports its build, then stays healthy for the
    target's settle time. None when it did, else why not."""
    start = clock()
    healthy_since = None
    while True:
        problems = [problem for name, revision in expected.items()
                    if (problem := probe(transport, target.service(name), revision, strict))]
        now = clock()
        if not problems:
            if healthy_since is None:
                healthy_since = now
                print(f"all of {', '.join(expected)} report their build, {now - start:.0f} s in")
            if now - healthy_since >= target.settle:
                return None
        elif healthy_since is not None:
            print(f"unhealthy again after {now - healthy_since:.0f} s: {'; '.join(problems)}")
            healthy_since = None
        if now - start >= target.timeout + target.settle:
            return "; ".join(problems) or "never healthy for long enough"
        sleep(PROBE_INTERVAL)


def command_plan(args, registry: Registry, git: Git, transport: Transport, **_) -> int:
    source = read_source(args, transport)
    plan = plan_from(args, source, registry, git)
    write_outputs(plan, args.out)
    if not plan.changed:
        print(f"{source.name} already runs the builds of {plan.revision}.")
        return 0
    if args.apply:
        if not args.infra:
            raise PromotionError("--apply edits an infra checkout: pass --infra")
        for path, (old, new) in plan.files.items():
            if old != new:
                (Path(args.infra) / path).write_text(new, encoding="utf-8")
        print(f"Edited {', '.join(sorted(p for p, (o, n) in plan.files.items() if o != n))} in {args.infra}.")
    return 0


def command_deploy(args, registry: Registry, git: Git, transport: Transport,
                   clock: Callable[[], float], sleep: Callable[[float], None]) -> int:
    for attempt in range(3):
        source = read_source(args, transport)
        target = source.target
        plan = plan_from(args, source, registry, git)
        write_outputs(plan, args.out)
        if not plan.changed:
            print(f"{source.name} already runs the builds of {plan.revision}; nothing to deploy.")
            return 0
        why = "--dry-run" if args.dry_run else None if target.rollout else f"rollout = false in {args.config}"
        if why or source.forge is None:
            print(f"Dry run ({why or 'read from a checkout'}): would commit the change above to {source.name}, "
                  f"then wait up to {target.timeout} s for "
                  + ", ".join(f"{target.service(n).health} to report {_short(r)}" for n, r in plan.expected().items())
                  + f" and stay healthy for {target.settle} s; if it does not, commit the previous pins back.")
            return 0
        changes = {path: (new, source.blobs[path]) for path, (old, new) in plan.files.items() if old != new}
        if source.forge.commit(changes, plan.message()):
            break
        print(f"{source.name} moved while this ran; planning again")
    else:
        raise PromotionError(f"{source.name} kept moving; nothing was committed")
    print(f"Committed {plan.title} to {source.name}; waiting for the rollout")
    problem = watch(transport, target, plan.expected(), clock, sleep)
    if problem is None:
        print(f"Deployed {plan.revision}: {', '.join(plan.moves)} healthy for {target.settle} s.")
        return 0
    print(f"The rollout of {plan.revision[:12]} failed: {problem}", file=sys.stderr)
    return roll_back(args, plan, transport, clock, sleep, problem)


def roll_back(args, plan: Plan, transport: Transport, clock, sleep, problem: str) -> int:
    """Commit the pins the deploy replaced, if nothing moved them since, and wait for them."""
    source = read_source(args, transport)
    pinned = pins_from_manifests(source.texts, source.target, source.name)
    ours = {name: plan.images[name].digest for name in plan.moves}
    if any(pinned[name] != digest for name, digest in ours.items()):
        print(f"Not rolling back: {source.name} no longer pins this deploy's images", file=sys.stderr)
        return 1
    changes = {}
    for name in plan.moves:
        service = source.target.service(name)
        text = changes.get(service.manifest, (source.texts[service.manifest],))[0]
        changes[service.manifest] = (repin(text, service, plan.current[name].digest), source.blobs[service.manifest])
    message = (f"{plan.title.split(':')[0]}: roll back {SOURCE_REPO} {plan.revision[:12]}\n\n"
               f"Its rollout failed: {problem}\n")
    if not source.forge.commit(changes, message):
        print(f"Could not roll back: {source.name} moved while this ran", file=sys.stderr)
        return 1
    print(f"Committed the previous pins to {source.name}; waiting for them")
    back = watch(transport, source.target, {name: plan.current[name].built for name in plan.moves}, clock, sleep,
                 strict=False)
    if back is None:
        print("Rolled back: the previous images are healthy again.", file=sys.stderr)
    else:
        print(f"Rolled back the pins, but the previous images are not healthy either: {back}", file=sys.stderr)
    return 1


def write_outputs(plan: Plan, out: str | None) -> None:
    if out:
        directory = Path(out)
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "plan.json").write_text(json.dumps(plan.document(), indent=2) + "\n", encoding="utf-8")
        (directory / "message.txt").write_text(plan.message(), encoding="utf-8")
        (directory / "infra.diff").write_text(plan.diff(), encoding="utf-8")
        if plan.qualification is not None:
            write_qualification(plan.qualification, directory)
    print(plan.message())
    print(plan.diff())


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    root.add_argument("--root", default=".", help="this repository's checkout, with its history")
    root.add_argument("--head", default="HEAD", help="the released commit: images come from it or its history")
    commands = root.add_subparsers(dest="command", required=True)
    for name in ("plan", "deploy"):
        each = commands.add_parser(name)
        each.add_argument("--repo", default=os.environ.get(REPO_ENV, ""),
                          help=f"the target repository on {FORGE} (default ${REPO_ENV})")
        each.add_argument("--config", default=os.environ.get(CONFIG_ENV, ""),
                          help=f"the deploy target's path in it (default ${CONFIG_ENV})")
        each.add_argument("--infra", help="a checkout of the target repository to read instead")
        each.add_argument("--revision", help="the commit to deploy (default: the receipts', or the newest "
                                             "with every image)")
        each.add_argument("--receipt", action="append", default=[], help="a publishing check's receipt")
        each.add_argument("--require-signed", action="store_true", help="refuse receipts of unsigned images")
        each.add_argument("--allow-rollback", action="store_true", help="allow deploying an older revision")
        each.add_argument("--tag", default="sha-{commit}",
                          help="the tag a commit's images are published under (main's: dev-{commit})")
        each.add_argument("--qualification", help="the qualification manifest of the release this deploys "
                                                  "(required to enable a gated feature)")
        each.add_argument("--allow-source", action="store_true",
                          help="accept a manifest that only observed this commit's source, not the release")
        each.add_argument("--out", help="directory for plan.json, message.txt and infra.diff")
    commands.choices["plan"].add_argument("--apply", action="store_true", help="edit the --infra checkout")
    commands.choices["deploy"].add_argument("--dry-run", action="store_true", help="stop before committing")
    return root


def main(argv: list[str] | None = None, transport: Transport = urllib_transport,
         clock: Callable[[], float] = time.monotonic, sleep: Callable[[float], None] = time.sleep) -> int:
    args = parser().parse_args(argv)
    git = Git(Path(args.root))
    registry = Registry(transport)
    command = {"plan": command_plan, "deploy": command_deploy}[args.command]
    try:
        return command(args, registry, git, transport, clock=clock, sleep=sleep)
    except Superseded as why:
        print(f"Superseded: {why}; nothing to do.")
        return 0
    except PromotionError as error:
        print(f"cloud_deploy: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
