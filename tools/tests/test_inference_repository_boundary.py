"""Keep project-specific inference research outside the OS/app repository."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


class InferenceRepositoryBoundaryTests(unittest.TestCase):
    def test_model_and_hardware_research_roots_are_external(self):
        for relative in (
            "docs/projects/llm-asic",
            "hardware/llm-asic",
            "tools/chip",
            "tools/tests/test_chip_model_profile.py",
        ):
            with self.subTest(path=relative):
                self.assertFalse(
                    (ROOT / relative).exists(),
                    f"{relative} belongs in tilo/inference-hardware; keep the generic app here",
                )


if __name__ == "__main__":
    unittest.main()
