#!/usr/bin/env python3
"""Validate the real release command against Cargo targets and shipped outputs.

Runs cargo metadata --no-deps and shell argument expansion only; never compiles.
"""
import json
import os
from pathlib import Path
import re
import subprocess

repo = Path(__file__).resolve().parents[2]
# The release compile, outside the image build (os/image/compile.sh).
recipe = (repo / "os/image/compile.sh").read_text()
start = recipe.index('    && if [ "${TILO_WITH_BROWSER}" = "0" ]; then')
end = recipe.index('    && build_phase rust-windows-vm', start)
# Execute the recipe's actual conditionals and argv with a recording cargo stub.
command = "\n".join(line for line in recipe[start:end].splitlines()
                    if not line.lstrip().startswith("#"))
command = command.replace("\\\n", " ").rstrip().removesuffix("\\").lstrip()
assert command.startswith("&& ")
command = command[3:]
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--no-deps", "--format-version", "1", "--offline",
    "--manifest-path", str(repo / "Cargo.toml"),
], text=True))
packages = {p["name"]: p for p in metadata["packages"]}
staging = recipe[recipe.index("    && build_phase rust-stage-outputs", end):
                 recipe.index("    && build_phase rust-thin-apps", end)]
shipped = set(re.findall(r"target/release/([a-zA-Z0-9_-]+)", staging))
vm_bins = {"tilo-windows-vm-control", "tilo-windows-rail"}
desktop_bins = {"shell", "tilo-launcher", "tilo-runtime-mcp", "tilo-unlock-helper"}
excluded = set()
for browser in ("0", "1"):
    for desktop in ("0", "1"):
        script = """build_phase() { :; }
cargo() { printf '%s\\n' "$@"; }
""" + command
        argv = subprocess.check_output(["bash", "-ec", script], text=True,
            env=dict(os.environ, TILO_WITH_BROWSER=browser,
                     TILO_PREBUILT_DESKTOP=desktop)).splitlines()
        assert argv[:2] == ["build", "--release"], argv
        assert "--bins" not in argv, argv
        selected = [argv[i + 1] for i, value in enumerate(argv) if value == "--bin"]
        selected_packages = {argv[i + 1] for i, value in enumerate(argv) if value == "-p"}
        available = {t["name"] for name in selected_packages
                     for t in packages[name]["targets"] if "bin" in t["kind"]}
        expected = shipped - vm_bins
        if desktop == "1":
            expected -= desktop_bins
        assert len(selected) == len(set(selected)), "duplicate binary selection"
        assert set(selected) == expected, (browser, desktop, set(selected) ^ expected)
        assert set(selected) <= available, set(selected) - available
        # The Browser is a tapp and its engine a runtime: no release binary.
        assert not any("browser" in value for value in argv), argv
        excluded |= available - set(selected)
        print(f"PASS browser={browser} prebuilt_desktop={desktop}: {len(selected)} shipped binaries")
assert {"shots", "tilo-loadclient", "ux-bench", "click_test_client",
        "tilo-model-catalog"} <= excluded, excluded
print("Unshipped binaries excluded:", ", ".join(sorted(excluded)))
