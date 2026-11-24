#!/usr/bin/env python3
"""Keep the decision about number 0's directory in one place.

``presets::N0`` is iroh's "everything at number 0's": its public relays and a
DNS directory that signs this device's key in on the spot and republishes it
every five minutes, whatever the endpoint is doing. What Tilo decided instead: keep
the relays and the directory, and check what is published — by the same
condition that holds the relays back — in
``crates/core/endpoint-reachability/src/directory.rs``.

A durable endpoint that binds ``presets::N0`` therefore quietly restores the
unguarded behaviour. This check refuses it in production code and points at the
preset's replacement. Two kinds of endpoint are exempt, because they are not
a device's identity:

* A test's endpoint, under ``#[cfg(test)]`` or in a test file: it is bound,
  used and dropped, and never outlives the test that binds it. Nothing to
  list: the test attribute is what exempts it.
* A process that is not a desktop. ``EXEMPT`` names each file and why:
  ``tilo-server``'s daemon, the initrd's sibling unlock and enrolment, and a
  peer network that dials the peers its caller names. Each lives as
  long as the command that started it, dials an address it was handed, or is
  the server devices are meant to find. They are listed so that a device
  endpoint added beside one has to be argued for rather than absorbed.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, production_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]

# Where the decision lives. This is the only file that may reach for the
# preset, because reaching for it is what the policy is about.
DECISION = "crates/core/endpoint-reachability/src/directory.rs"

# Production files that still bind `presets::N0`, with why each may. A
# one-line process or a transport that only dials with the address it was
# handed; the session's and the filesystem's endpoints are not here.
EXEMPT = {
    # A client of the device administrator, which chooses its own relays.
    # The initrd's sibling unlock: it asks the siblings named in a token that
    # a boot already holds, and the process ends with the unlock.
    "crates/os/tilo-auth/src/sibling/exchange.rs": "initrd unlock exchange",
    # The initrd's sibling enrolment: it reaches a sibling by the peer network's
    # own names where the token gives no address, and ends with the enrolment.
    "crates/os/tilo-auth/src/sibling/enrol.rs": "initrd sibling enrolment",
    # A P2P node that dials the peers a caller names.
    "crates/platform/tilo-net-p2p/src/lib.rs": "net-p2p dialer",
    # The server daemon's own endpoint, which is what devices dial.
    "crates/core/tilo-server/src/daemon.rs": "server daemon",
}

PRESET = re.compile(r"\bpresets::N0\b")


def sources(root: Path) -> list[Path]:
    return sorted(
        path
        for path in (root / "crates").rglob("*.rs")
        if "vendor" not in path.parts
    )


def check(root: Path = ROOT) -> list[str]:
    """Return a line for each production use of the preset the policy refuses."""
    found = []
    for path in sources(root):
        relative = path.relative_to(root)
        key = relative.as_posix()
        if key == DECISION or is_test_file(relative) or key in EXEMPT:
            continue
        source = path.read_text(encoding="utf-8")
        text = production_source(source)
        for match in PRESET.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            found.append(
                f"{key}:{line}: binds iroh's `presets::N0`, which signs this "
                "device in to number 0's directory whatever the endpoint is "
                "doing; take `tilo_endpoint_reachability::collaborating()` (or "
                "`Watchonly`) instead, or add the file to EXEMPT in "
                f"{Path(__file__).name} with why it is not a device endpoint"
            )
    for key in EXEMPT:
        if not (root / key).is_file():
            found.append(f"{key}: exempt in {Path(__file__).name} but no longer exists")
    return found


def main() -> int:
    found = check()
    if found:
        print("\n".join(found))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
