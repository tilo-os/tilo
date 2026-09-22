#!/usr/bin/env python3
"""Package a development guest from retained Wine; never compile Wine here."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess

REPO = Path(__file__).resolve().parents[2]


def reference(value):
    if not re.fullmatch(r"code\.podesta\.ai/qos/[a-z0-9][a-z0-9._/-]*@sha256:[0-9a-f]{64}", value):
        raise ValueError("expected immutable tilo registry reference")
    return value


def inputs(env):
    result = {name: env.get(name, "") for name in (
        "WINDOWS_GUEST_BUILDROOT_REF", "WINE_PAIR_REF", "DEVELOPMENT_WINE_REF",
        "DEVELOPMENT_WINE_KEY_SHA256", "WINDOWS_GUEST_RPMS_REF")}
    reference(result["WINDOWS_GUEST_BUILDROOT_REF"])
    for name in ("WINE_PAIR_REF", "DEVELOPMENT_WINE_REF", "WINDOWS_GUEST_RPMS_REF"):
        if result[name]:
            reference(result[name])
    signed = bool(result["DEVELOPMENT_WINE_REF"])
    key = result["DEVELOPMENT_WINE_KEY_SHA256"]
    if signed != bool(key) or key and not re.fullmatch(r"[0-9a-f]{64}", key):
        raise ValueError("retained signed Wine requires independent public-key SHA256")
    if signed == bool(result["WINE_PAIR_REF"]):
        raise ValueError("select either a Wine pair to sign or retained signed Wine")
    if result["WINDOWS_GUEST_RPMS_REF"] and not signed:
        raise ValueError("retained snapshot requires its original signed Wine and key")
    return result


def run(*args, capture=False):
    return subprocess.run(args, cwd=REPO, check=True, text=True,
                          stdout=subprocess.PIPE if capture else None).stdout


def build(tag, recipe, arguments, context=".", extra=()):
    command = ["docker", "buildx", "build", "--load", "--platform", "linux/amd64",
               "--provenance=false", "-t", tag, "-f", str(recipe)]
    for name, value in arguments.items():
        command += ["--build-arg", f"{name}={value}"]
    run(*command, *extra, str(context))


def publish(tag):
    run("docker", "push", tag)
    digests = json.loads(run("docker", "image", "inspect", "--format",
                            "{{json .RepoDigests}}", tag, capture=True))
    repository = tag.rsplit(":", 1)[0]
    matches = [item for item in digests if item.startswith(repository + "@")]
    if len(matches) != 1:
        raise ValueError("published image must have one matching repository digest")
    return reference(matches[0])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--validate-only", action="store_true")
    args = parser.parse_args()
    selected = inputs(os.environ)
    if args.validate_only:
        return
    branch = os.environ.get("GITHUB_REF", "")
    if branch != "refs/heads/main" and not branch.startswith("refs/heads/codex/"):
        raise ValueError("development packaging requires main or a codex branch")
    sha, run_id = os.environ["GITHUB_SHA"], os.environ["GITHUB_RUN_ID"]
    if not re.fullmatch(r"[0-9a-f]{40}", sha) or not run_id.isdecimal():
        raise ValueError("invalid CI source identity")
    evidence = REPO / "guest-evidence"
    evidence.mkdir(exist_ok=False)
    work = REPO / "guest-packaging"
    work.mkdir(exist_ok=False)
    suffix = f"candidate-{sha}-{run_id}"
    buildroot = selected["WINDOWS_GUEST_BUILDROOT_REF"]

    def retain():
        (evidence / "inputs.json").write_text(json.dumps({
            "schema": 1, "development_only": True, "source_revision": sha,
            "inputs": selected}, indent=2) + "\n")

    retain()
    if not selected["DEVELOPMENT_WINE_REF"]:
        pair = work / "pair"
        pair.mkdir()
        run("docker", "pull", selected["WINE_PAIR_REF"])
        container = run("docker", "create", selected["WINE_PAIR_REF"], "/unused", capture=True).strip()
        try:
            run("docker", "cp", container + ":/wine-paired/.", str(pair))
        finally:
            run("docker", "rm", container)
        tooling = "tilo-windows-guest-signing:" + suffix
        build(tooling, "os/Containerfile.windows-guest-tools",
              {"WINDOWS_GUEST_BUILDROOT_REF": buildroot})
        export = work / "export"
        export.mkdir()
        # CI's Docker daemon may live in another container. Copy inputs through
        # its API rather than bind job-local paths from the daemon's filesystem.
        # Private GnuPG state exists only inside this disposable signer.
        signer = run("docker", "create", "--network=none", "--entrypoint", "python3", tooling,
            "/tilo/development-wine-repository.py", "sign", "--pair", "/wine-paired",
            "--pair-reference", selected["WINE_PAIR_REF"], "--pin", "/tilo/wine-source.json",
            "--output", "/signed-wine", capture=True).strip()
        try:
            run("docker", "cp", str(REPO / "os/dev") + "/.", signer + ":/tilo/")
            run("docker", "cp", str(pair), signer + ":/wine-paired")
            receipt = run("docker", "start", "--attach", signer, capture=True)
            status = run("docker", "inspect", "--format", "{{.State.ExitCode}}", signer, capture=True).strip()
            if status != "0":
                raise RuntimeError("development Wine signer failed")
            run("docker", "cp", signer + ":/signed-wine", str(export))
        finally:
            run("docker", "rm", "-f", signer)
        public = json.loads(receipt)
        key = public["public_key_sha256"]
        if not re.fullmatch(r"[0-9a-f]{64}", key):
            raise ValueError("signer returned invalid public-key digest")
        (evidence / "signing-receipt.json").write_text(json.dumps(public, indent=2) + "\n")
        recipe = work / "Containerfile.signed-wine"
        recipe.write_text("FROM scratch\nCOPY signed-wine /signed-wine\n")
        tag = "code.podesta.ai/qos/tilo-development-wine:" + suffix
        build(tag, recipe, {}, export)
        selected["DEVELOPMENT_WINE_REF"] = publish(tag)
        selected["DEVELOPMENT_WINE_KEY_SHA256"] = key
        retain()
    build_args = {key: selected[key] for key in (
        "WINDOWS_GUEST_BUILDROOT_REF", "DEVELOPMENT_WINE_REF", "DEVELOPMENT_WINE_KEY_SHA256")}
    if not selected["WINDOWS_GUEST_RPMS_REF"]:
        tag = "code.podesta.ai/qos/tilo-development-windows-guest-rpms:" + suffix
        build(tag, "os/Containerfile.windows-guest-rpms", build_args)
        selected["WINDOWS_GUEST_RPMS_REF"] = publish(tag)
        retain()
    run("python3", "os/dev/fetch-windows-guest-assets.py", str(work / "assets"))
    build_args["WINDOWS_GUEST_RPMS_REF"] = selected["WINDOWS_GUEST_RPMS_REF"]
    tag = "code.podesta.ai/qos/tilo-development-windows-guest:" + suffix
    build(tag, "os/Containerfile.windows-guest", build_args,
          extra=("--build-context", f"github-assets={work / 'assets'}"))
    guest = publish(tag)
    (evidence / "guest-reference.txt").write_text(guest + "\n")
    # This receipt proves assembly only, never Windows application acceptance.
    (evidence / "result.json").write_text(json.dumps({
        "schema": 1, "development_only": True, "reference": guest,
        "runtime_acceptance_verified": False, "inputs": selected}, indent=2) + "\n")


if __name__ == "__main__":
    main()
