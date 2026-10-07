#!/usr/bin/env python3
"""Run the production pinned-collection boundary without building an image."""
from pathlib import Path
import os
import subprocess
import textwrap

source = Path(".forgejo/workflows/build-image.yml").read_text()
step = source.split("      - name: Materialize the pinned app collection\n", 1)[1]
step = step.split("\n      - name: Materialize the pinned desktop payload", 1)[0]
# This diagnostic supplies the known immutable collection explicitly. Reuse
# eligibility belongs to the image plan; all auth/archive/source/hash/runtime
# checks after that decision are the actual production workflow code.
start = step.index("          # The Authorized Integration grants")
script = "set -euo pipefail\n" + textwrap.dedent(step[start:])
for directory in ("os/app-artifacts", "os/app-runtime"):
    Path(directory).mkdir(parents=True, exist_ok=True)
result = subprocess.run(["bash", "-c", script], env=os.environ, capture_output=True, text=True)
materialized = result.returncode == 0 and "::notice::using pinned prebuilt app collection" in result.stdout
print("production_collection_materialized=" + str(materialized).lower())
print("production_collection_exit=" + str(result.returncode))
if not materialized:
    # Never emit raw child output: the OIDC branch may issue a masking command.
    print("production_collection_failed_before_completed_verification=true")
raise SystemExit(0 if materialized else 1)
