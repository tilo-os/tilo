"""Guard the selective inputs and one heavy-parent copy in Containerfile.dev."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
RECIPE = ROOT / "os/Containerfile.dev"

# Sources read by the dev Cargo build and install manifest. Extra runtime files
# must not enter the build context and invalidate compilation on ordinary edits.
EXPECTED_COPIES = (
    "COPY Cargo.toml Cargo.lock rust-toolchain.toml /build/",
    "COPY .cargo /build/.cargo",
    "COPY crates /build/crates",
    "COPY apps /build/apps",
    "COPY examples /build/examples",
    "COPY vendor /build/vendor",
    "COPY dist /build/dist",
    "COPY os/components.toml /build/os/components.toml",
    "COPY os/scripts/install-components.py os/scripts/verify-elf-hardening.sh "
    "os/scripts/cargo-freshness.sh /build/os/scripts/",
    "COPY os/usr/share/tilo/installer-locales.txt "
    "/build/os/usr/share/tilo/installer-locales.txt",
    "COPY tools/policy/app_portability_rules.toml "
    "/build/tools/policy/app_portability_rules.toml",
    "COPY tools/policy/app_capability_usage.toml "
    "/build/tools/policy/app_capability_usage.toml",
)


def stages(recipe: str) -> dict[str, list[str]]:
    result: dict[str, list[str]] = {}
    current = None
    for line in recipe.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        match = re.fullmatch(r"FROM .+ AS ([\w-]+)", line, re.IGNORECASE)
        if match:
            current = match.group(1)
            result[current] = []
        elif current:
            result[current].append(line)
    return result


class DevBuilderStagingTest(unittest.TestCase):
    def test_inputs_are_staged_once_on_scratch(self):
        with self.subTest(phase="selective_inputs_are_staged_on_scratch"):
            recipe = RECIPE.read_text(encoding="utf-8")
            self.assertIn("FROM scratch AS dev-builder-sources", recipe)
            instructions = stages(recipe)["dev-builder-sources"]
            self.assertEqual(instructions, list(EXPECTED_COPIES))
        with self.subTest(phase="toolchain_receives_one_staged_source_layer"):
            instructions = stages(RECIPE.read_text(encoding="utf-8"))["dev-builder"]
            copies = [line for line in instructions if line.startswith("COPY ")]
            self.assertEqual(copies, ["COPY --from=dev-builder-sources /build/ /build/"])
            self.assertFalse(any("COPY . /build" in line for line in instructions))


if __name__ == "__main__":
    unittest.main()
