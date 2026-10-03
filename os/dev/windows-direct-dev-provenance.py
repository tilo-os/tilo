#!/usr/bin/env python3
"""Verify a CI development image against live Forgejo records.

This is an explicit developer trust path, not release signing or SLSA.
It does not authorize disk writes. Physical callers must also bind the VM
proof, inspect the installed source and verify the reviewed disk extents.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

REPOSITORY = "registry.tilo.so/tilo/tilo-vm"
SHA = re.compile(r"sha256:[0-9a-f]{64}\Z")
SOURCE = re.compile(r"[0-9a-f]{40}\Z")
IMAGE_TYPES = {"application/vnd.oci.image.manifest.v1+json",
               "application/vnd.docker.distribution.manifest.v2+json"}
FGJ = ("fgj", "--hostname", "code.tilo.so")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def file_bytes(path, limit):
    require(path.is_file() and not path.is_symlink() and 0 < path.stat().st_size <= limit,
            f"CI input is missing, redirected or oversized: {path.name}")
    return path.read_bytes()


def forgejo(*arguments):
    result = subprocess.run([*FGJ, *arguments], capture_output=True, timeout=60, check=True)
    require(0 < len(result.stdout) <= 32 * 1024 * 1024, "Forgejo response is empty or oversized")
    return result.stdout


def successful_job(jobs, name, run_id):
    matches = [job for job in jobs if type(job) is dict and job.get("name") == name]
    require(len(matches) == 1 and matches[0].get("status") == "success"
            and matches[0].get("run_id") == run_id
            and type(matches[0].get("id")) is int and matches[0]["id"] > 0
            and type(matches[0].get("attempt")) is int and matches[0]["attempt"] > 0,
            f"CI development job has no unique successful result: {name}")
    return matches[0]


def verify_image(args):
    match = re.fullmatch(re.escape(REPOSITORY) + r"@(sha256:[0-9a-f]{64})", args.image)
    require(match is not None and args.profile in ("generic", "lenovo-83ed")
            and re.fullmatch(r"[1-9][0-9]*", args.ci_run_id) is not None,
            "development verification requires an immutable image, profile and internal CI run ID")
    digest = match[1]
    manifest_bytes = file_bytes(args.manifest, 16 * 1024 * 1024)
    require("sha256:" + hashlib.sha256(manifest_bytes).hexdigest() == digest,
            "CI development manifest differs from its image pin")
    manifest = json.loads(manifest_bytes)
    require(type(manifest) is dict and manifest.get("schemaVersion") == 2
            and manifest.get("mediaType") in IMAGE_TYPES
            and type(manifest.get("config")) is dict and type(manifest.get("layers")) is list
            and manifest["layers"], "CI development input is not one image manifest")
    config_bytes = file_bytes(args.config, 1024 * 1024)
    descriptor = manifest["config"]
    require(descriptor.get("size") == len(config_bytes)
            and descriptor.get("digest") == "sha256:" + hashlib.sha256(config_bytes).hexdigest(),
            "CI development config is not bound by the image manifest")
    config = json.loads(config_bytes)
    require(type(config) is dict and config.get("architecture") == "arm64"
            and config.get("os") == "linux" and type(config.get("config")) is dict,
            "CI development image is not Linux ARM64")
    labels = config["config"].get("Labels", {})
    require(type(labels) is dict and labels.get("sh.tilo.image.flavour") == "dev"
            and labels.get("org.tilo.with-windows") == "1"
            and type(labels.get("org.opencontainers.image.revision")) is str
            and SOURCE.fullmatch(labels["org.opencontainers.image.revision"]),
            "CI development image lacks the requested flavour, source or Windows payload")
    revision = labels["org.opencontainers.image.revision"]
    run_id = int(args.ci_run_id)
    run_bytes = forgejo("api", f"repos/tilo/tilo/actions/runs/{run_id}")
    run = json.loads(run_bytes)
    require(type(run) is dict and run.get("id") == run_id
            and run.get("workflow_id") in ("os-dev.yml", "os-dev-arm.yml") and run.get("prettyref") == "main"
            and run.get("is_fork_pull_request") is False
            and run.get("commit_sha") == revision
            and run.get("status") in ("success", "failure")
            and type(run.get("index_in_repo")) is int and run["index_in_repo"] > 0,
            "live Forgejo run does not bind the development image source")
    jobs = json.loads(forgejo("api", f"repos/tilo/tilo/actions/runs/{run_id}/jobs"))
    require(type(jobs) is list and len(jobs) <= 256, "Forgejo job inventory is invalid")
    image_job = successful_job(jobs, "build-and-push (aarch64, runner-large-arm64)", run_id)
    suffix = "lenovo-83ed" if args.profile == "lenovo-83ed" else "ci"
    origin = f"{REPOSITORY}:main-{revision[:7]}-w{run_id}-{suffix}"
    name = "board: lenovo-83ed (lenovo-83ed)" if args.profile == "lenovo-83ed" else image_job["name"]
    job = successful_job(jobs, name, run_id)
    log = forgejo("actions", "run", "view", str(run["index_in_repo"]), "--job", name, "--log",
                  "-R", "tilo/tilo")
    if args.profile == "lenovo-83ed":
        publication = ("Trying to pull " + args.image + "...").encode()
    else:
        publication = ("exporting manifest " + digest).encode()
    require(publication in log, "successful CI job does not identify the pinned development image")
    producer_log = (forgejo("actions", "run", "view", str(run["index_in_repo"]),
                            "--job", image_job["name"], "--log", "-R", "tilo/tilo")
                    if args.profile == "lenovo-83ed" else log)
    published = ("(" + origin + "@" + digest + ")" if args.profile == "lenovo-83ed" else
                 "pushed " + origin + "-aarch64 (" + digest + ")").encode()
    require(published in producer_log,
            "successful producer job does not bind the published cohort tag to this digest")
    return {"schema": "tilo.windows-direct-ci-dev-image.v1",
            "authority": "live-forgejo-development-run", "image": args.image,
            "image_kind": args.profile, "source_revision": revision,
            "invocation_id": str(run_id), "update_origin": origin,
            "image_job_id": image_job["id"], "publication_job_id": job["id"],
            "publication_job_attempt": job.get("attempt"),
            "config_sha256": hashlib.sha256(config_bytes).hexdigest(),
            "producer_log_sha256": hashlib.sha256(producer_log).hexdigest(),
            "publication_log_sha256": hashlib.sha256(log).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--ci-run-id", required=True)
    parser.add_argument("--profile", choices=("generic", "lenovo-83ed"), required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        require(not args.output.exists() and not args.output.is_symlink()
                and args.output.parent.is_dir(), "CI evidence output must be new")
        evidence = verify_image(args)
        args.output.write_text(json.dumps(evidence, sort_keys=True) + "\n")
        print("CI development image verified against live Forgejo: " + args.image)
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"development CI verification: {error}\n")


if __name__ == "__main__":
    main()
