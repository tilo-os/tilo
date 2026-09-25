#!/usr/bin/python3
"""Validate the installer UKI projection against the actual base kargs."""
import re
import tomllib
import unittest
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
CONTAINERFILE = REPO / "os/Containerfile.installer"
KARGS = REPO / "os/usr/lib/bootc/kargs.d"
COMMAND = re.compile(
    r"tilo-uki-cmdline\s+(remove|append)\s+(\S+)(?:\s+(/boot/tilo-target/tilo-target\.efi))?"
)
TARGET_COPY = 'cp "$target_uki" /boot/tilo-target/tilo-target.efi'


def base_tokens():
    tokens = []
    for path in sorted(KARGS.glob("*.toml")):
        tokens.extend(tomllib.loads(path.read_text(encoding="utf-8"))["kargs"])
    tokens.append("composefs=" + "0" * 128)
    return tokens


def apply(tokens, action, argument):
    key = argument.split("=", 1)[0]
    matches = [token for token in tokens if token.split("=", 1)[0] == key]
    if action == "remove":
        if len(matches) != 1:
            raise AssertionError(f"remove {key} matched {len(matches)} tokens")
        return [token for token in tokens if token.split("=", 1)[0] != key]
    if matches:
        raise AssertionError(f"append {key} would duplicate {matches[0]}")
    return [*tokens, argument]


class InstallerCmdlineProjectionTest(unittest.TestCase):
    def test_live_and_target_projections_are_unique_and_fit_ukify_section(self):
        source = base_tokens()
        live = list(source)
        target = None
        for line in CONTAINERFILE.read_text(encoding="utf-8").splitlines():
            if TARGET_COPY in line:
                self.assertIsNone(target)
                target = list(live)
            match = COMMAND.search(line)
            if not match:
                continue
            action, argument, target_path = match.groups()
            if target_path:
                self.assertIsNotNone(target)
                target = apply(target, action, argument)
            else:
                live = apply(live, action, argument)

        # ukify includes the input newline and a terminating NUL, then rounds
        # the PE raw section to its 512-byte FileAlignment.
        initial_bytes = len(" ".join(source).encode()) + 2
        raw_room = ((initial_bytes + 511) // 512) * 512
        self.assertLessEqual(len(" ".join(live).encode()), raw_room)
        self.assertIn("tilo.install", live)
        self.assertIn("tilo.mode=normal", live)
        self.assertIn("root=tilo-auto", live)
        self.assertIn(
            "rd.luks.options=tpm2-device=auto,fido2-device=auto", live
        )
        self.assertFalse(any(token.startswith("tilo.root=") for token in live))
        self.assertFalse(any(token.startswith("tilo.luks=") for token in live))
        self.assertFalse(any(token.startswith("tilo.unlock=") for token in live))

        self.assertIsNotNone(target)
        self.assertIn("root=tilo-auto", target)
        self.assertIn(
            "rd.luks.options=tpm2-device=auto,fido2-device=auto", target
        )
        self.assertNotIn("tilo.install", target)
        self.assertFalse(any(token.startswith("tilo.root=") for token in target))
        self.assertFalse(any(token.startswith("tilo.luks=") for token in target))
        self.assertFalse(any(token.startswith("tilo.unlock=") for token in target))
        self.assertEqual(
            1, sum(token.startswith("root=") for token in target)
        )
        self.assertEqual(
            1, sum(token.startswith("rd.luks.options=") for token in target)
        )


if __name__ == "__main__":
    unittest.main()
