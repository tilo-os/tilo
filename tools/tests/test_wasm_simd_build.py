"""The page's wasm is built and optimized with SIMD enabled.

`fearless_simd`, which `vello_cpu`'s fine-raster kernels dispatch through,
chooses its level from the target features the module was compiled with, and
on wasm it has no runtime detection: without `+simd128` in the compile flags
it picks the scalar fallback even in a browser that runs SIMD. And binaryen
refuses to read a module whose SIMD instructions it was not told to enable,
so the optimization step has to ask too. Both live in one place each; a
change that drops either is a page that silently paints with scalar
kernels again.
"""
from __future__ import annotations

from pathlib import Path
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
CARGO_CONFIG = ROOT / ".cargo" / "config.toml"
WASM_OPT_FLAGS = ROOT / "tools" / "web" / "wasm-opt-flags"
BUILD_SCRIPT = ROOT / "crates/core/tilo-browser-runtime/scripts/build.sh"


def flags(path: Path) -> list[str]:
    """Every argument a flags file names, its comments left out."""
    words: list[str] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0]
        words.extend(line.split())
    return words


class WasmSimdBuildTests(unittest.TestCase):
    def test_the_wasm32_build_enables_simd128(self):
        target = tomllib.loads(CARGO_CONFIG.read_text(encoding="utf-8"))["target"]
        enabled = [
            entry.get("rustflags", [])
            for name, entry in target.items()
            if "wasm32" in name
        ]
        self.assertTrue(enabled, f"no wasm32 target flags in {CARGO_CONFIG}")
        features = " ".join(
            f"{word} {following}"
            for flags_ in enabled
            for word, following in zip(flags_, flags_[1:])
            if word in ("-C", "--codegen")
        )
        self.assertIn(
            "target-feature=+simd128",
            features,
            "wasm32 builds must compile with +simd128, or fearless_simd picks "
            "its scalar fallback in every browser",
        )

    def test_wasm_opt_may_read_the_simd_instructions(self):
        self.assertIn(
            "--enable-simd",
            flags(WASM_OPT_FLAGS),
            "wasm-opt refuses SIMD input without --enable-simd",
        )

    def test_the_bundle_build_neither_replaces_nor_resets_the_target_features(self):
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        for replacement in ('export RUSTFLAGS=', "RUSTFLAGS='-C target-feature='"):
            self.assertNotIn(
                replacement,
                script,
                "build.sh must let .cargo/config.toml's wasm32 flags apply",
            )


if __name__ == "__main__":
    unittest.main()
