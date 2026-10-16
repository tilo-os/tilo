#!/usr/bin/env bash
# Offline release-contract checks: these intentionally do not download weights.
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bash -n "$script_dir/prepare-release.sh"
python3 - "$script_dir" <<'PY'
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
recipe = (root / "prepare-release.sh").read_text()
manifest = dict(
    reversed(line.split("  ", 1)) for line in (root / "model-files.sha256").read_text().splitlines()
)

revision = "8d10d2f3bb75ae3b6d527c77944fc5e7dcd94b29809d47a739a7a728a912b491"
assert manifest["u2net.onnx"] == revision
assert re.fullmatch(r"[0-9a-f]{64}", manifest["LICENSE"])
assert f"MODEL_REVISION='{revision}'" in recipe
assert "MODEL_SIZE=175997641" in recipe
assert '"artifact_format": "onnx_file"' in recipe
assert '"allowed_runtimes": []' in recipe
assert '"context_window": 0' in recipe
assert '"segmentation": true' in recipe
assert '"vision": false' in recipe
assert '"face_recognition": false' in recipe
assert '"license_id": "Apache-2.0"' in recipe
assert "${release_url}/u2net.onnx" in recipe and "${release_url}/LICENSE" in recipe
assert "latest" not in recipe
PY
