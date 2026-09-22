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
manifest = (root / "model-files.sha256").read_text().splitlines()

assert "a3992d502a8714ce2f6207fde3507ccf9503f224" in recipe
assert "c62343ee72f094ef574ab3af2717d0c60fec5e0808b238f19ac4c0d22fd152f7" in recipe
assert '"vision": true' in recipe
assert '"face_recognition": false' in recipe
assert "latest" not in recipe
assert any(line.endswith("  model.safetensors") for line in manifest)
for line in manifest:
    digest, path = line.split("  ", 1)
    assert re.fullmatch(r"[0-9a-f]{64}", digest), digest
    assert path and not path.startswith("/") and ".." not in pathlib.PurePosixPath(path).parts
PY
