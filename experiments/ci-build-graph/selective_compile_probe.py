#!/usr/bin/env python3
"""Measure Cargo feature unification when dropping unselected app roots.

Tiny, offline fixture only; no product compilation and no shared target dirs.
"""
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def main():
    with tempfile.TemporaryDirectory(prefix="tilo-cargo-root-probe-") as directory:
        root = Path(directory)
        files = {
            "Cargo.toml": '[workspace]\nmembers=["sdk", "one", "two"]\nresolver="3"\n',
            "rust-toolchain.toml": (ROOT / "rust-toolchain.toml").read_text(),
            "sdk/Cargo.toml": '[package]\nname="sdk"\nversion="0.1.0"\nedition="2024"\n[features]\nwide=[]\n',
            "sdk/src/lib.rs": 'pub fn wide() -> bool { cfg!(feature="wide") }\n',
            "one/Cargo.toml": '[package]\nname="one"\nversion="0.1.0"\nedition="2024"\n[dependencies]\nsdk={path="../sdk"}\n',
            "one/src/main.rs": 'fn main() { println!("{}", sdk::wide()); }\n',
            "two/Cargo.toml": '[package]\nname="two"\nversion="0.1.0"\nedition="2024"\n[dependencies]\nsdk={path="../sdk"}\n[features]\nwide=["sdk/wide"]\n',
            "two/src/lib.rs": 'pub fn placeholder() {}\n',
        }
        for name, text in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root, check=True, capture_output=True)
        results = []
        for name, args in (
            ("full-roots", ["-p", "one", "-p", "two", "--features", "two/wide"]),
            ("selected-root-with-all-app-features", ["-p", "one", "--features", "two/wide"]),
            ("selected-root-with-explicit-shared-feature", ["-p", "one", "--features", "sdk/wide"]),
        ):
            built = subprocess.run(["cargo", "build", "--locked", "--offline", *args], cwd=root, capture_output=True, text=True)
            observed = None
            if built.returncode == 0:
                observed = subprocess.check_output([str(root / "target/debug/one")], text=True).strip()
            results.append(dict(case=name, exit_code=built.returncode, shared_feature_enabled=observed,
                                error=built.stderr if built.returncode else None))
        print(json.dumps(results, indent=2))
        assert results[0]["shared_feature_enabled"] == "true", "full-graph reference failed"
        assert results[1]["exit_code"] != 0 or results[1]["shared_feature_enabled"] == "false", "Cargo behavior changed; reassess selection"
        assert results[2]["shared_feature_enabled"] == "true", "explicit feature union failed"


if __name__ == "__main__":
    main()
