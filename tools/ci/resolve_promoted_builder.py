#!/usr/bin/env python3
"""Resolve and verify the signed promoted builder's native children."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[2]
RESOLVER_SPEC = importlib.util.spec_from_file_location("builder_resolver", Path(__file__).with_name("resolve_builder.py"))
resolver = importlib.util.module_from_spec(RESOLVER_SPEC)
RESOLVER_SPEC.loader.exec_module(resolver)
REPOSITORY = resolver.IMAGE
LABELS_SPEC = importlib.util.spec_from_file_location("runner_labels", Path(__file__).with_name("runner_labels.py"))
runner_labels = importlib.util.module_from_spec(LABELS_SPEC)
LABELS_SPEC.loader.exec_module(runner_labels)
PROMOTION_SPEC = importlib.util.spec_from_file_location("native_index", ROOT / "os/dev/promote-native-index.py")
promotion = importlib.util.module_from_spec(PROMOTION_SPEC)
PROMOTION_SPEC.loader.exec_module(promotion)


def command(*args):
    return subprocess.check_output(args)


def resolve(run=command, fetch=resolver.fetch_manifest):
    index = resolver.resolve("", fetch)
    if not re.fullmatch(re.escape(REPOSITORY) + r"@sha256:[0-9a-f]{64}", index):
        raise ValueError("promoted builder is not immutable")
    run("cosign", "verify", "--key", "keys/cosign.pub", index)
    raw = run("docker", "buildx", "imagetools", "inspect", "--raw", index)
    if index.rsplit("@", 1)[1] != "sha256:" + hashlib.sha256(raw).hexdigest():
        raise ValueError("promoted builder index digest mismatch")
    document = json.loads(raw)
    children = {}
    reverse = {value: key for key, value in promotion.ARCHITECTURES.items()}
    for descriptor in document.get("manifests", []) if isinstance(document, dict) else []:
        platform = descriptor.get("platform", {}) if isinstance(descriptor, dict) else {}
        arch = reverse.get(platform.get("architecture"))
        digest = descriptor.get("digest") if isinstance(descriptor, dict) else None
        if arch is None or arch in children or not isinstance(digest, str):
            raise ValueError("promoted builder has invalid native children")
        children[arch] = digest
    promotion.verify_index(document, children)
    if len(set(children.values())) != 2:
        raise ValueError("promoted builder native digests must be distinct")
    for digest in children.values():
        run("cosign", "verify", "--key", "keys/cosign.pub", f"{REPOSITORY}@{digest}")
    return {"include": [dict(arch=arch,
                             runner=runner_labels.NATIVE[arch],
                             image=f"{REPOSITORY}@{children[arch]}")
                        for arch in sorted(children)]}


def main():
    matrix = resolve()
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
        output.write("matrix=" + json.dumps(matrix, separators=(",", ":")) + "\n")
    print("Resolved signed promoted builder children")


if __name__ == "__main__":
    main()
