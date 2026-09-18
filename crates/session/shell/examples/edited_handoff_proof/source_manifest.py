#!/usr/bin/env python3
"""Record repository source hashes around an externally coordinated proof build/run."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

repo = Path(__file__).resolve().parents[3]
paths = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=repo).split(b"\0")
suffixes = {".rs", ".toml", ".lock", ".capnp", ".c", ".cc", ".cpp", ".h", ".hpp", ".m", ".mm", ".sh", ".py", ".wgsl", ".glsl"}
hashes = {}
missing = []
for raw in sorted(set(paths)):
    if not raw:
        continue
    name = raw.decode()
    path = repo / name
    if path.suffix not in suffixes:
        continue
    if not path.is_file():
        missing.append(name)
        continue
    hashes[name] = hashlib.sha256(path.read_bytes()).hexdigest()
output = Path(sys.argv[1])
output.parent.mkdir(parents=True, exist_ok=True)
output.write_text(json.dumps({
    "repository": str(repo),
    "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip(),
    "coverage": "tracked and nonignored untracked files matching listed source suffixes",
    "suffixes": sorted(suffixes), "missing": missing, "sha256": hashes,
}, indent=2) + "\n")
print(f"Recorded {len(hashes)} source files: {output}")
