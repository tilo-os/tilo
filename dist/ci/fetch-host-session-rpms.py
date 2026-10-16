#!/usr/bin/env python3
"""Replay VM acceptance with an exact-source artifact from the public Tilo repo."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys
import tempfile
import urllib.request
import zipfile

API = "https://code.tilo.so/api/v1/repos/tilo/tilo"
MAX_ARCHIVE = 1024**3
MAX_RPMS = 4 * 1024**3


def metadata(path):
    with urllib.request.urlopen(API + path, timeout=30) as response:
        body = response.read(1024**2 + 1)
    if len(body) > 1024**2:
        raise ValueError("RPM source metadata exceeds its 1 MiB cap")
    return json.loads(body)


def select_artifact(run, artifacts, run_id, commit, jobs):
    if (run.get("id") != run_id or run.get("commit_sha") != commit
            or run.get("workflow_id") != "host-session-e2e.yml"
            or run.get("repository", {}).get("full_name") != "tilo/tilo"):
        raise ValueError("RPM source run does not match the exact accepted build identity")
    rpm_jobs = [job for job in jobs if job.get("name") == "rpms"]
    if len(rpm_jobs) != 1 or rpm_jobs[0].get("status") != "success":
        raise ValueError("RPM source packaging job did not pass lint, installation and upload")
    matches = [a for a in artifacts if a.get("name") == "fedora-44-amd64"]
    if len(matches) != 1:
        raise ValueError("RPM source must have exactly one accepted Fedora artifact")
    artifact = matches[0]
    if (artifact.get("run_id") != run_id or artifact.get("expired")
            or not 0 < artifact.get("size_in_bytes", 0) <= MAX_ARCHIVE):
        raise ValueError("RPM source artifact is expired, mismatched or exceeds 1 GiB")
    return artifact


def extract_rpms(archive, output):
    with zipfile.ZipFile(archive) as bundle:
        entries = [i for i in bundle.infolist() if i.filename.endswith(".rpm")]
        names = [i.filename for i in entries]
        matches = [re.fullmatch(r"(tilo-session(?:-apps|-devel)?)-[0-9][^/]*\.rpm", name)
                   for name in names]
        if (len(entries) != 3 or len(set(names)) != 3
                or any(match is None for match in matches)
                or {match[1] for match in matches if match} != {
                    "tilo-session", "tilo-session-apps", "tilo-session-devel"}
                or any((i.external_attr >> 16) & 0o170000 == 0o120000
                       for i in entries)
                or sum(i.file_size for i in entries) > MAX_RPMS):
            raise ValueError("RPM archive must contain exactly three flat Tilo RPMs, at most 4 GiB")
        packages = {}
        for entry in entries:
            digest = hashlib.sha256()
            copied = 0
            with bundle.open(entry) as source, (output / entry.filename).open("xb") as target:
                while block := source.read(1024**2):
                    copied += len(block)
                    if copied > entry.file_size:
                        raise ValueError("RPM archive entry exceeded declared size")
                    digest.update(block)
                    target.write(block)
            if copied != entry.file_size:
                raise ValueError("RPM archive entry is incomplete")
            packages[entry.filename] = {"byte_size": copied, "sha256": digest.hexdigest()}
        return packages


def fetch(run_id, commit, output):
    if not re.fullmatch(r"[0-9a-f]{40}", commit) or not re.fullmatch(r"[1-9][0-9]{0,18}", run_id):
        raise ValueError("RPM replay requires an explicit API run ID and full source commit")
    run_id = int(run_id)
    run = metadata(f"/actions/runs/{run_id}")
    artifact = select_artifact(run, metadata(f"/actions/runs/{run_id}/artifacts"), run_id, commit,
                               metadata(f"/actions/runs/{run_id}/jobs"))
    if output.exists():
        raise ValueError("RPM replay output already exists")
    output.parent.mkdir(parents=True, exist_ok=True)
    if shutil.disk_usage(output.parent).free < 6 * 1024**3:
        raise ValueError("RPM replay requires 6 GiB free for its bounded archive and RPMs")
    output.mkdir()
    try:
        with tempfile.TemporaryDirectory(dir=output.parent) as temporary:
            archive = Path(temporary) / "rpms.zip"
            digest = hashlib.sha256()
            copied = 0
            # Public repository API; no token, cloud credential or new access.
            with urllib.request.urlopen(API + f"/actions/artifacts/{artifact['id']}/zip",
                                        timeout=60) as source, archive.open("xb") as target:
                while block := source.read(1024**2):
                    copied += len(block)
                    if copied > artifact["size_in_bytes"] or copied > MAX_ARCHIVE:
                        raise ValueError("RPM archive exceeds its source-bound byte limit")
                    digest.update(block)
                    target.write(block)
            if copied != artifact["size_in_bytes"]:
                raise ValueError("RPM archive is incomplete")
            packages = extract_rpms(archive, output)
            (output / "source.json").write_text(json.dumps({
                "workflow": run["workflow_id"], "run": run["index_in_repo"], "run_id": run_id,
                "commit": commit, "harness_commit": os.environ["GITHUB_SHA"],
                "artifact": artifact["name"], "artifact_id": artifact["id"],
                "archive_sha256": digest.hexdigest(), "rpms": sorted(packages),
                "artifacts": packages,
                "artifact_purpose": "host-session-acceptance-only",
                "release_signing_claim": "none",
                "signature_verification": "not-performed-by-this-source-report",
            }, indent=1) + "\n")
    except BaseException:
        shutil.rmtree(output)
        raise


if __name__ == "__main__":
    try:
        fetch(sys.argv[1], sys.argv[2], Path(sys.argv[3]))
    except Exception:
        # Download redirects may carry signed URLs: never print exception URLs.
        print("Source-pinned RPM replay failed its metadata, download or storage checks", file=sys.stderr)
        sys.exit(1)
