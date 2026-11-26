#!/usr/bin/env python3
"""Fetch the hosted CLI qualification approved by a protected annotated release tag.

Offline verify takes qualification.tag only from this release's trusted t output.
The release gate verifies the Apple CMS over qualification.json separately.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import urllib.request


REPOSITORY = "https://code.tilo.so/tilo/tilo.git"
PACKAGE = "https://code.tilo.so/api/packages/tilo/generic/tilo-cli-macos-stage"
PAYLOAD = Path(__file__).resolve().parents[3] / "dist/apple/hosted-cli-sign.py"
ARCHES = ("aarch64", "x86_64")
MAC = {arch: f"tilo-macos-{arch}.tar.gz" for arch in ARCHES}
LINUX = {arch: f"tilo-linux-{arch}.tar.gz" for arch in ARCHES}
ACCOUNTS = {arch: f"account-{arch}.json" for arch in ARCHES}
PROGRAMS = {"tilo", "tilo-runtime"}
META_LIMIT = 128 * 1024
ARCHIVE_LIMIT = 1024**3
DIGEST = re.compile(r"[0-9a-f]{64}")
TRAILER = b"Hosted-CLI-Qualification-SHA256"


def candidate():
    ref, commit = os.environ.get("T_REF", ""), os.environ.get("T_COMMIT", "")
    match = re.fullmatch(r"tag:(v\+cli-[0-9]+\.[0-9]+\.[0-9]+-[0-9A-Za-z]+(?:\.[0-9A-Za-z]+)*)", ref)
    if not match or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("expected a hosted CLI prerelease T_REF and its complete T_COMMIT")
    return match[1], commit


def upstream_tag(tag, commit):
    """Fetch only the exact public ref. Never follow a URL rewrite or read a working tree's configuration."""
    tag_ref = f"refs/tags/{tag}"
    with tempfile.TemporaryDirectory() as scratch:
        env = {"PATH": os.environ["PATH"], "HOME": scratch, "GIT_CONFIG_NOSYSTEM": "1",
               "GIT_CONFIG_GLOBAL": os.devnull, "GIT_TERMINAL_PROMPT": "0", "GIT_NO_REPLACE_OBJECTS": "1"}

        def git(*args):
            return subprocess.check_output(["git", "-c", "http.followRedirects=false", "-c", "protocol.allow=never",
                                            "-c", "protocol.https.allow=always", *args], cwd=scratch, env=env)

        git("init", "--bare", "--quiet", "--template=")
        git("fetch", "--quiet", "--depth=1", "--filter=tree:0", "--no-tags", REPOSITORY, f"{tag_ref}:{tag_ref}")
        if git("cat-file", "-t", tag_ref).strip() != b"tag":
            raise ValueError("the published hosted CLI ref must be an annotated tag")
        if int(git("cat-file", "-s", tag_ref)) > META_LIMIT:
            raise ValueError("the published tag exceeds the metadata limit")
        annotation = git("cat-file", "tag", tag_ref)
        qualification_pin(annotation, tag, commit)
        if git("cat-file", "-t", commit).strip() != b"commit":
            raise ValueError("the tag target must be the tested commit")
        return annotation


def qualification_pin(annotation, tag, commit):
    header, separator, body = annotation.partition(b"\n\n")
    headers = header.split(b"\n")
    expected = [b"object " + commit.encode(), b"type commit", b"tag " + tag.encode()]
    if not separator or len(headers) != 4 or headers[:3] != expected or not headers[3].startswith(b"tagger "):
        raise ValueError("the annotated tag does not name the exact source tag and commit directly")
    trailers = [line for line in body.splitlines() if line.strip().lower().startswith(TRAILER.lower())]
    if len(trailers) != 1:
        raise ValueError("the annotated tag must contain exactly one Hosted-CLI-Qualification-SHA256 trailer")
    match = re.fullmatch(TRAILER + rb": ([0-9A-Fa-f]{64})", trailers[0])
    if not match:
        raise ValueError("invalid Hosted-CLI-Qualification-SHA256 trailer")
    return match[1].decode("ascii").lower()


def fields(record, names, label):
    if not isinstance(record, dict) or record.keys() != set(names):
        raise ValueError(f"unexpected fields in {label}")
    return record


def valid_digest(value):
    return isinstance(value, str) and DIGEST.fullmatch(value) is not None


def digest_map(record, names, label):
    fields(record, names, label)
    if not all(valid_digest(value) for value in record.values()):
        raise ValueError(f"invalid SHA-256 in {label}")


def parse_json(data):
    def unique(pairs):
        record = dict(pairs)
        if len(record) != len(pairs):
            raise ValueError("duplicate field in qualification JSON")
        return record

    return json.loads(data.decode("utf-8"), object_pairs_hook=unique)


def regular(path, limit):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= limit:
        raise ValueError(f"expected a bounded nonempty regular file for {path.name}")
    return path


def sha256(path):
    digest = hashlib.sha256()
    with regular(path, ARCHIVE_LIMIT).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def check_source(record, tag, commit, label):
    if type(record["schema"]) is not int or record["schema"] != 1 or (record["tag"], record["commit"]) != (tag, commit):
        raise ValueError(f"{label} does not cover this release and schema")


def qualification(directory, tag, commit, pin):
    data = regular(directory / "qualification.json", META_LIMIT).read_bytes()
    if hashlib.sha256(data).hexdigest() != pin:
        raise ValueError("qualification.json differs from the digest in the protected tag")
    record = fields(parse_json(data), ("schema", "commit", "tag", "raw_sha256", "signed_sha256", "account_sha256"),
                    "qualification.json")
    check_source(record, tag, commit, "qualification.json")
    digest_map(record["raw_sha256"], [*MAC.values(), *LINUX.values()], "raw CI archives")
    digest_map(record["signed_sha256"], MAC.values(), "signed Mac archives")
    digest_map(record["account_sha256"], ACCOUNTS.values(), "dedicated account evidence")
    return record


def account_observations(record, arch, tag, commit, signed):
    fields(record, ("schema", "commit", "tag", "architecture", "macos_version", "execution", "uid", "default_keychain",
                    "initial_device_items", "final_device_items", "keychain_prompts", "file_device_keys",
                    "signed_archive_sha256", "installed_sha256", "cli_version",
                    "elapsed_ms", "setup_exit", "first", "restarted"), ACCOUNTS[arch])
    check_source(record, tag, commit, ACCOUNTS[arch])
    if record["cli_version"] != "tilo " + tag.removeprefix("v+cli-"):
        raise ValueError(f"the installed {arch} CLI version does not match {tag}")
    if (record["architecture"], record["execution"], record["signed_archive_sha256"]) != (arch, "native", signed):
        raise ValueError(f"{arch} needs native qualification for this signed archive")
    if not isinstance(record["macos_version"], str) or not re.fullmatch(r"[0-9]+(?:\.[0-9]+){1,2}", record["macos_version"]):
        raise ValueError(f"invalid macOS version for {arch}")
    if record["default_keychain"] != "login.keychain-db":
        raise ValueError(f"{arch} needs the dedicated account's default login Keychain")
    for key, expected in (("initial_device_items", 0), ("final_device_items", 1), ("keychain_prompts", 0),
                          ("file_device_keys", 0), ("setup_exit", 0)):
        if type(record[key]) is not int or record[key] != expected:
            raise ValueError(f"{arch} qualification requires {key} to be {expected}")
    for key, maximum in (("uid", 2**32 - 2), ("elapsed_ms", 2**63 - 1)):
        if type(record[key]) is not int or not 0 < record[key] <= maximum:
            raise ValueError(f"invalid {key} for {arch} qualification")
    digest_map(record["installed_sha256"], PROGRAMS, "installed executables")
    first, again = record["first"], record["restarted"]
    for phase in (first, again):
        fields(phase, ("cli_device_sha256", "daemon_device_sha256", "workspace_sha256", "session", "daemon_pid"),
               "admission observation")
        if (phase["session"] != "connected" or not valid_digest(phase["cli_device_sha256"])
                or phase["cli_device_sha256"] != phase["daemon_device_sha256"] or not valid_digest(phase["workspace_sha256"])
                or type(phase["daemon_pid"]) is not int or not 0 < phase["daemon_pid"] < 2**31):
            raise ValueError(f"{arch} needs a live daemon admitted as the same device the CLI identified")
    if (again["daemon_pid"] == first["daemon_pid"] or again["cli_device_sha256"] != first["cli_device_sha256"]
            or again["workspace_sha256"] != first["workspace_sha256"]):
        raise ValueError(f"{arch} needs the same admitted workspace and device after restarting its daemon")


def verified_payload(raw, directory, arch, raw_pin, signed_pin):
    name = MAC[arch]
    result = subprocess.run(
        [sys.executable, PAYLOAD, "payload", regular(raw / name, ARCHIVE_LIMIT), raw_pin,
         arch, regular(directory / name, ARCHIVE_LIMIT), "--signed-sha256", signed_pin],
        stdout=subprocess.PIPE, check=False,
    )
    if result.returncode:
        raise ValueError(f"{arch} signed executables differ from the tested CI payload or pinned archive")
    return parse_json(result.stdout)


def qualify_files(raw, directory, tag, commit, pins=None):
    receipt = {"schema": 1, "tag": tag, "commit": commit, "raw_sha256": {}, "signed_sha256": {}, "account_sha256": {}}
    for name in LINUX.values():
        actual = sha256(raw / name)
        if pins and actual != pins["raw_sha256"][name]:
            raise ValueError(f"{name} differs from the tested CI archive")
        receipt["raw_sha256"][name] = actual
    devices = set()
    for arch, name in ACCOUNTS.items():
        data = regular(directory / name, META_LIMIT).read_bytes()
        account_digest = hashlib.sha256(data).hexdigest()
        if pins and account_digest != pins["account_sha256"][name]:
            raise ValueError(f"{name} differs from the evidence in the reviewed qualification")
        account = parse_json(data)
        if not isinstance(account, dict):
            raise ValueError(f"{name} must contain a dedicated account observation")
        mac = MAC[arch]
        # During preparation the Mac operator's account record supplies the expected
        # signed hash. The payload checker measures both archives from private copies.
        signed_pin = pins["signed_sha256"][mac] if pins else account.get("signed_archive_sha256")
        if not valid_digest(signed_pin):
            raise ValueError(f"missing qualified signed archive digest in {name}")
        account_observations(account, arch, tag, commit, signed_pin)
        raw_pin = pins["raw_sha256"][mac] if pins else sha256(raw / mac)
        payload = verified_payload(raw, directory, arch, raw_pin, signed_pin)
        installed = {program: payload["programs"][program]["after_sha256"] for program in PROGRAMS}
        if installed != account["installed_sha256"]:
            raise ValueError(f"{name} describes different installed executables")
        device = account["first"]["cli_device_sha256"]
        if device in devices:
            raise ValueError("the two native qualification accounts must start with independent device identities")
        devices.add(device)
        receipt["raw_sha256"][mac] = payload["raw_sha256"]
        receipt["signed_sha256"][mac] = payload["signed_sha256"]
        receipt["account_sha256"][name] = account_digest
    return receipt


def prepare(raw, directory):
    """Compile the operator's existing account evidence and archives, without signing or publishing."""
    tag, commit = candidate()
    receipt = qualify_files(raw, directory, tag, commit)
    data = (json.dumps(receipt, sort_keys=True, indent=2) + "\n").encode("ascii")
    output = directory / "qualification.json"
    with tempfile.NamedTemporaryFile(dir=directory) as pending:
        pending.write(data)
        pending.flush()
        try:
            os.link(pending.name, output)
        except FileExistsError:
            if regular(output, META_LIMIT).read_bytes() != data:
                raise ValueError("a different qualification.json already exists in the staging directory") from None
    return hashlib.sha256(data).hexdigest()


def verify(raw, directory):
    """Revalidate the tag and qualified files previously fetched into trusted t storage."""
    tag, commit = candidate()
    annotation = regular(directory / "qualification.tag", META_LIMIT).read_bytes()
    receipt = qualification(directory, tag, commit, qualification_pin(annotation, tag, commit))
    regular(directory / "qualification.cms", META_LIMIT)
    if qualify_files(raw, directory, tag, commit, receipt) != receipt:
        raise ValueError("qualification differs from the staged artifacts and tested source")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def download(commit, name, path, limit):
    opener = urllib.request.build_opener(NoRedirect())
    url = f"{PACKAGE}/{commit}/{name}"
    with opener.open(url, timeout=90) as response, path.open("xb") as out:
        if response.status != 200 or response.geturl() != url:
            raise ValueError(f"unexpected response downloading {name}")
        total = 0
        while block := response.read(1024 * 1024):
            total += len(block)
            if total > limit:
                raise ValueError(f"{name} exceeds its size limit")
            out.write(block)


def fetch(raw, directory):
    tag, commit = candidate()
    if directory.is_symlink() or (directory.exists() and (not directory.is_dir() or any(directory.iterdir()))):
        raise ValueError("qualification output must be a fresh directory")
    annotation = upstream_tag(tag, commit)
    pin = qualification_pin(annotation, tag, commit)
    directory.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=directory.parent) as tmp:
        pending = Path(tmp) / "qualified"
        pending.mkdir()
        (pending / "qualification.tag").write_bytes(annotation)
        download(commit, "qualification.json", pending / "qualification.json", META_LIMIT)
        qualification(pending, tag, commit, pin)
        for name in [*ACCOUNTS.values(), *MAC.values(), "qualification.cms"]:
            download(commit, name, pending / name, ARCHIVE_LIMIT if name in MAC.values() else META_LIMIT)
        verify(raw, pending)
        os.replace(pending, directory)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "fetch", "verify"))
    parser.add_argument("raw_dir", type=Path)
    parser.add_argument("stage_dir", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            print(prepare(args.raw_dir, args.stage_dir))
        else:
            {"fetch": fetch, "verify": verify}[args.command](args.raw_dir, args.stage_dir)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"hosted CLI qualification: {error}")
