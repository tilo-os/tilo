#!/usr/bin/env python3
"""Select an authenticated board successor; tags are discovery hints only.

All cryptographic verification uses the image's pinned Cosign verifier/key.
This helper never changes deployment, owner trust, or rollback state.
"""
import base64
from contextlib import contextmanager
import binascii
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import selectors
import sys
import stat
import tempfile
import signal
import time

REPOSITORY = "code.tilo.so/tilo/tilo"
PROFILE = "lenovo-83ed"
FRESHNESS = "https://tilo.sh/security/release-freshness/v1"
HEX = re.compile(r"[0-9a-f]{64}\Z")
SOURCE = re.compile(r"git\+ssh://code\.tilo\.so/tilo/tilo@([0-9a-f]{40})\Z")
MAX_OUTPUT = 8 * 1024 * 1024
COSIGN = "/usr/libexec/tilo-cosign"
KEY = "/usr/share/tilo/cosign.pub"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def run(*arguments, environment=None, timeout=120):
    # Read both pipes incrementally and stop before retaining more than the
    # bound. communicate() would allocate unbounded registry-controlled bytes.
    with subprocess.Popen(arguments, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, env=environment) as child:
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ, "output")
            selector.register(child.stderr, selectors.EVENT_READ, "errors")
            output = bytearray()
            totals = {"output": 0, "errors": 0}
            deadline = time.monotonic() + timeout
            try:
                while selector.get_map():
                    require(time.monotonic() < deadline, "release verification timed out")
                    for key, _ in selector.select(timeout=0.02):
                        chunk = os.read(key.fileobj.fileno(), 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        totals[key.data] += len(chunk)
                        require(totals[key.data] <= MAX_OUTPUT,
                                "release verification output exceeds its bound")
                        if key.data == "output":
                            output.extend(chunk)
                child.wait(timeout=max(0.001, deadline - time.monotonic()))
                require(child.returncode == 0,
                        f"{Path(arguments[0]).name} refused release verification (exit {child.returncode})")
                return output.decode("utf-8")
            except BaseException:
                child.kill()
                child.wait()
                raise


def wrappers(output):
    require(0 < len(output.encode()) <= MAX_OUTPUT, "invalid verification output size")
    try:
        values = [json.loads(output)]
    except json.JSONDecodeError:
        values = [json.loads(line) for line in output.splitlines() if line.strip()]
    rows = [row for value in values for row in (value if type(value) is list else [value])]
    require(rows and len(rows) <= 128 and all(type(row) is dict for row in rows),
            "malformed or excessive verification statements")
    return rows


def statements(output, predicate_type, digest):
    result = []
    for row in wrappers(output):
        try:
            statement = json.loads(base64.b64decode(row["payload"], validate=True))
        except (KeyError, TypeError, ValueError, binascii.Error):
            continue
        if type(statement) is not dict or statement.get("predicateType") != predicate_type:
            continue
        subjects = statement.get("subject")
        if (type(subjects) is list and len(subjects) == 1 and type(subjects[0]) is dict
                and subjects[0].get("digest") == {"sha256": digest[7:]}):
            result.append(statement.get("predicate"))
    require(len(result) == 1 and type(result[0]) is dict,
            "release has no unique exact-digest attestation")
    return result[0]


def signature(output, digest):
    rows = wrappers(output)
    require(all(row.get("critical", {}).get("image", {}).get("docker-manifest-digest") == digest
                and row.get("critical", {}).get("identity", {}).get("docker-reference") == REPOSITORY
                for row in rows), "release signature identity or digest differs")


def provenance(output, digest):
    predicate = statements(output, "https://slsa.dev/provenance/v1", digest)
    definition = predicate.get("buildDefinition", {})
    details = predicate.get("runDetails", {})
    require(type(definition) is dict and type(details) is dict, "invalid release provenance")
    external = definition.get("externalParameters", {})
    require(type(external) is dict
            and definition.get("buildType") == "https://tilo.sh/buildtypes/bootc-os/v1"
            and details.get("builder") == {"id": "https://code.tilo.so/tilo/forgejo/actions/bootc-release-v1"}
            and definition.get("internalParameters", {}).get("expectedReleaseDigest") == digest
            and external.get("hardwareProfile") == PROFILE
            and external.get("architecture") == "aarch64"
            and type(external.get("generation")) is int and 0 < external["generation"] < 2**64
            and "children" not in external
            and type(external.get("hardwareDefinitionSha256")) is str
            and HEX.fullmatch(external["hardwareDefinitionSha256"])
            and type(external.get("firmwareManifestSha256")) is str
            and HEX.fullmatch(external["firmwareManifestSha256"]),
            "release provenance does not bind the board, architecture, digest and generation")
    dependencies = definition.get("resolvedDependencies")
    require(type(dependencies) is list and len(dependencies) <= 64,
            "invalid release dependencies")
    sources = [SOURCE.fullmatch(item.get("uri", "")) for item in dependencies
               if type(item) is dict and type(item.get("uri")) is str]
    sources = [item[1] for item in sources if item]
    firmware = [item["uri"] for item in dependencies if type(item) is dict
                and type(item.get("uri")) is str and re.fullmatch(
                    r"code\.tilo\.so/tilo/tilo-lenovo-83ed-firmware@sha256:[0-9a-f]{64}", item["uri"])]
    require(len(sources) == 1 and len(firmware) == 1,
            "release has no unique source and immutable board firmware")
    byproducts = details.get("byproducts")
    require(type(byproducts) is list and any(item == {
        "name": "independent-rebuild", "uri": "urn:tilo:verification:manifest-digest-equal"
    } for item in byproducts), "release has no independent rebuild witness")
    return sources[0], external["generation"]


def freshness(output, digest, source, generation, now):
    predicate = statements(output, FRESHNESS, digest)
    require(set(predicate) == {"schema", "repository", "digest", "sourceRevision",
                               "generation", "issuedAt", "expiresAt"}
            and predicate["schema"] == FRESHNESS and predicate["repository"] == REPOSITORY
            and predicate["digest"] == digest and predicate["sourceRevision"] == source
            and type(predicate["generation"]) is int and predicate["generation"] == generation
            and type(predicate["issuedAt"]) is int and type(predicate["expiresAt"]) is int
            and 0 < predicate["issuedAt"] <= now < predicate["expiresAt"]
            and 300 <= predicate["expiresAt"] - predicate["issuedAt"] <= 604800,
            "release freshness is expired, ambiguous, or belongs to another release")
    return predicate["expiresAt"]


def select(current_generation, now, runner=run):
    hint = f"{REPOSITORY}:{PROFILE}-latest"
    digest = runner("/usr/bin/skopeo", "inspect", "--format", "{{.Digest}}", f"docker://{hint}").strip()
    require(digest.startswith("sha256:") and HEX.fullmatch(digest[7:]), "invalid successor hint digest")
    exact = f"{REPOSITORY}@{digest}"
    signature(runner(COSIGN, "verify", "--key", KEY, exact), digest)
    source, generation = provenance(runner(COSIGN, "verify-attestation", "--key", KEY,
                                          "--type", "slsaprovenance1", exact), digest)
    require(generation >= current_generation, "release hint regressed below the running generation")
    require(generation - current_generation <= 64,
            "release advance exceeds rollback counter bound; install intermediate generations")
    expires = freshness(runner(COSIGN, "verify-attestation", "--key", KEY, "--type", FRESHNESS, exact),
              digest, source, generation, now)
    for tag in (f"{PROFILE}-gen{generation}", f"{PROFILE}-{source}"):
        signature(runner(COSIGN, "verify", "--key", KEY, f"{REPOSITORY}:{tag}"), digest)
    return exact, generation, source, expires


@contextmanager
def registry_runner(module, root=Path("/"), temporary_directory="/run", owner_uid=0):
    # Match bootc/ostree-ext's root authfile precedence (run, etc, usr/lib).
    # https://bootc.dev/bootc/building/secrets.html
    # The existing pull identity is transport only; it cannot authorize an
    # image or change either pinned release key or the owner's grant.
    credentials = b"{}"
    for relative in ("run/ostree/auth.json", "etc/ostree/auth.json", "usr/lib/ostree/auth.json"):
        source = root / relative
        if not source.exists() and not source.is_symlink():
            continue
        source = module.trusted_path(root, Path(relative), regular=True)
        with source.open("rb") as stream:
            info = os.fstat(stream.fileno())
            require(stat.S_ISREG(info.st_mode) and info.st_uid == owner_uid
                    and info.st_nlink == 1 and info.st_mode & 0o777 == 0o600,
                    "bootc pull credentials must be a private root-owned regular file")
            credentials = stream.read(1_048_577)
        require(len(credentials) <= 1_048_576 and type(json.loads(credentials)) is dict,
                "bootc pull credentials are malformed or oversized")
        break
    with tempfile.TemporaryDirectory(prefix="tilo-board-verifier-", dir=temporary_directory) as directory:
        configuration = Path(directory) / "config.json"
        descriptor = os.open(configuration, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, "wb") as output:
            output.write(credentials)
        environment = dict(os.environ, DOCKER_CONFIG=directory, REGISTRY_AUTH_FILE=str(configuration))
        yield lambda *arguments: run(*arguments, environment=environment)


def cancelled(signum, _frame):
    # run() kills/reaps its owned child before this exits; no abandoned
    # verifier or unbounded wait survives normal service cancellation.
    raise SystemExit(128 + signum)


def installed_profile():
    location = Path("/usr/lib/tilo/hardware_profile.py")
    spec = importlib.util.spec_from_file_location("tilo_hardware_profile", location)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    profile = module.read_profile(Path("/"))
    if profile is None or module.development_origin(Path("/")) is not None:
        return None
    if profile["id"] != PROFILE:
        return None
    require(profile["architecture"] == "aarch64"
            and re.fullmatch(r"code\.tilo\.so/tilo/tilo(?::lenovo-83ed-[0-9a-f]{40}|@sha256:[0-9a-f]{64})",
                             profile["target_update_ref"]),
            "installed board update origin is not an official immutable release")
    return module


def main():
    require(len(sys.argv) == 2, "usage: tilo-board-successor.py applicable|NOW_NS")
    module = installed_profile()
    if module is None:
        return 3
    if sys.argv[1] == "applicable":
        return 0
    now_ns = int(sys.argv[1])
    require(0 < now_ns < 2**64, "invalid authenticated time")
    generation_path = module.trusted_path(Path("/"), Path("usr/lib/tilo/generation"), regular=True)
    require(generation_path.stat().st_size <= 32, "oversized image generation")
    generation = int(generation_path.read_text().strip())
    require(0 < generation < 2**64, "invalid image generation")
    with registry_runner(module) as runner:
        exact, next_generation, source, expires = select(generation, now_ns // 1_000_000_000, runner)
    if next_generation == generation:
        # Same generation may not introduce another digest. Compare bootc's
        # actual running image identity, rather than an old tag name.
        status = json.loads(run("/usr/bin/bootc", "status", "--format", "json"))
        require(status["status"]["booted"]["image"]["imageDigest"] == exact.split("@")[1],
                "release changed digest without advancing generation")
        return 3
    print(exact)
    print(next_generation)
    print(source)
    print(expires)
    return 0


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, cancelled)
    signal.signal(signal.SIGINT, cancelled)
    try:
        sys.exit(main())
    except (ValueError, OSError, KeyError, TypeError, AttributeError, subprocess.TimeoutExpired) as error:
        print(f"tilo-board-successor: {error}", file=sys.stderr)
        sys.exit(1)
