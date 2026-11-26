"""Report the images published by the tilo.sh release run after its gates pass."""

import json
import os
from pathlib import Path
import re
import sys


def report(root: Path, commit: str, ref: str) -> None:
    if not re.fullmatch(r"[0-9a-f]{40}", commit) or not ref:
        raise ValueError("the release run must name its source commit and reference")

    rows = [f"source {commit}"]
    for name, directory in (("lounge", "lounge/release"), ("web-entry", "web-entry"), ("api", "api")):
        receipt = json.loads((root / directory / "receipt.json").read_text())
        if (receipt.get("sourceRevision"), receipt.get("reference")) != (commit, ref):
            raise ValueError(f"{name} was published for {receipt.get('sourceRevision')} on {receipt.get('reference')}, "
                             f"expected {commit} on {ref}")
        repository = f"registry.tilo.so/tilo/{name}"
        digest = receipt.get("digest", "")
        if (not re.fullmatch(r"sha256:[0-9a-f]{64}", digest) or receipt.get("repository") != repository
                or receipt.get("image") != f"{repository}@{digest}"):
            raise ValueError(f"{name} receipt must name {repository} by its published digest")
        rows.append(f"{name} {repository}@{digest}")
    print("\n".join(rows))


if __name__ == "__main__":
    try:
        report(Path(sys.argv[1]), os.environ["T_COMMIT"], os.environ["T_REF"])
    except (KeyError, OSError, ValueError) as error:
        sys.exit(str(error))
