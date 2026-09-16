#!/usr/bin/env python3
"""Reproduce named routes using the ignored, assertion-bearing unit harness."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[3]
evidence = Path(__file__).resolve().parent
binary = Path(sys.argv[1]).resolve()
def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

with tempfile.TemporaryDirectory(prefix="qos-api-task11-fix1-") as runtime:
    env = dict(os.environ)
    for key in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"]:
        path = Path(runtime) / key.lower()
        path.mkdir()
        env[key] = str(path)
    env["API_TESTER_CAPTURE_DIR"] = str(evidence / "captures")
    command = [str(binary), "app::tests::composition_fix1_capture_routes", "--exact", "--ignored", "--nocapture"]
    result = subprocess.run(command, cwd=root, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (evidence / "logs" / "captures.log").write_text(result.stdout)
    print(result.stdout)
    result.check_returncode()
    manifest = {
        "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
        "capture_binary": str(binary),
        "normal_binary": str(root / "target/debug/api_tester"),
        "normal_binary_sha256": sha(root / "target/debug/api_tester"),
        "capture_binary_sha256": sha(binary),
        "command": command,
        "runtime_profile": {key: env[key] for key in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"]},
        "runtime_profile_removed_after_capture": True,
        "fixture": "Default Params/Headers and long Params/Headers/Form/navigation at 480x900 1x and 800x900 2x. Long content is staged before initial document sync; table assertions cover the same fixture. Navigation is snapped fully open before Driver creation, no in-flight animation. Captures do not validate saved-draft adoption.",
        "known_gap": "Existing saved-request-to-bound-draft loading overwrites local method/URL/body during next sync; staged captures do not verify that transition.",
        "sources": {p: sha(root / p) for p in ["apps/api_tester/src/app.rs", "apps/api_tester/src/navrow.rs", "apps/api_tester/tests/a11y.rs", "ui/src/widget/toggle_group.rs", "ui/src/richtext.rs", "ui/src/text_render.rs", "ui/src/widget/layout.rs", "ui/src/headless.rs", "Cargo.lock"]},
        "captures": {p.name: sha(p) for p in sorted((evidence / "captures").iterdir()) if p.suffix in [".png", ".json"]},
    }
    (evidence / "capture-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
