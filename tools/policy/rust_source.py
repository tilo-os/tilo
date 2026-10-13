"""Shared reading of Rust sources for the repository policy checks."""

import re
from pathlib import Path

# Where a file's conventional trailing test module starts.
TEST_MODULE_MARKERS = ("\n#[cfg(test)]", "\n#[cfg(all(test")
LINE_COMMENT = re.compile(r"(?m)^[ \t]*//.*$")


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def is_test_file(relative: Path) -> bool:
    """Whether `relative` is test or benchmark code rather than production code."""
    name = relative.name
    return (
        "tests" in relative.parts
        or "benches" in relative.parts
        or name == "tests.rs"
        or name.endswith("_tests.rs")
    )


def production_source(text: str) -> str:
    """The source up to its conventional trailing test module, with full-line
    comments blanked so diagnostics keep their line numbers."""
    ends = [text.find(marker) for marker in TEST_MODULE_MARKERS if marker in text]
    source = text[: min(ends)] if ends else text
    return LINE_COMMENT.sub("", source)
