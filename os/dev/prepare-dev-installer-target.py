#!/usr/bin/env python3
"""Export an immutable development image for disposable installer VM tests.

This preserves the registry manifest and checks all OCI blobs. It does not
authenticate a CI run or produce release signatures; the run ID is a locator.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import re
import subprocess


def prepare(args):
    match = re.fullmatch(r"registry\.tilo\.so/tilo/tilo-vm@(sha256:[0-9a-f]{64})", args.image)
    if (match is None or re.fullmatch(r"[0-9a-f]{40}", args.source_revision) is None
            or re.fullmatch(r"[1-9][0-9]*", args.ci_run_id) is None
            or args.hardware_profile not in ("generic", "lenovo-83ed")
            or re.fullmatch(r"registry\.tilo\.so/tilo/tilo-vm:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}",
                            args.update_origin) is None):
        raise ValueError("development target requires an immutable ARM image and explicit source, run, profile and origin")
    output = args.output
    if output.exists() or output.is_symlink() or not output.parent.is_dir() or output.parent.is_symlink():
        raise ValueError("target output must be new under a regular existing directory")
    output.mkdir(mode=0o700)
    subprocess.run(["skopeo", "copy", "--preserve-digests", "docker://" + args.image,
                    "oci:" + str(output / "oci") + ":target"], check=True)
    metadata = {"schema": "tilo.installer-dev-target.v1", "architecture": "aarch64",
                "target_ref": args.image, "target_digest": match[1],
                "hardware_profile": args.hardware_profile, "update_origin": args.update_origin,
                "source_revision": args.source_revision, "ci_run_id": args.ci_run_id}
    (output / "target.json").write_text(json.dumps(metadata, sort_keys=True) + "\n")
    module = Path(__file__).with_name("installer-offline-target.py")
    spec = importlib.util.spec_from_file_location("dev_target_transport", module)
    transport = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(transport)
    return transport.verify(output, development=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("image", "source-revision", "ci-run-id", "hardware-profile", "update-origin"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--output", type=Path, required=True)
    try:
        print(json.dumps(prepare(parser.parse_args()), sort_keys=True))
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"development installer target: {error}\n")


if __name__ == "__main__":
    main()
