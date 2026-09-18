#!/usr/bin/env python3
"""Keep every graphical Tilo app on the production AT-SPI endpoint.

Accessibility metadata in an app crate is not an endpoint.  The single
production endpoint is ``ui::App``: on Linux its winit AccessKit adapter owns
the AT-SPI provider, delivers actions back to the event loop, and is torn down
before the window.  Every graphical binary must therefore enter through the
standard ``ui::launch`` lifecycle (or its small direct-App equivalent).  This
cheap source boundary is deliberately paired with the installed-image D-Bus
smoke; it prevents a new app from silently bypassing the shared endpoint.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def audit(root: Path = ROOT) -> list[str]:
    errors: list[str] = []
    app_root = root / "apps"
    for manifest in sorted(app_root.glob("*/Cargo.toml")):
        main = manifest.parent / "src/main.rs"
        if not main.is_file():
            continue
        source = main.read_text(encoding="utf-8")
        # `tilo-agent` links the lightweight app-model facade to consume
        # resource descriptors, but deliberately has no window.  The launch
        # specification and direct App construction are the two windowed
        # entrypoint shapes; do not turn a headless collaboration participant
        # into a fake graphical provider just to satisfy this guard.
        if "LaunchSpec" not in source and "App::new(" not in source:
            continue
        standard_launch = "ui::launch::run(" in source or "ui::launch::run_or_exit(" in source
        direct_app = "App::new(" in source and (
            ".run_with_setup(" in source or ".run();" in source
        )
        if not (standard_launch or direct_app):
            errors.append(
                f"{main.relative_to(root)}: graphical app bypasses ui::App; "
                "use ui::launch::run or App::run so it reaches AT-SPI"
            )

    cargo = (root / "ui/Cargo.toml").read_text(encoding="utf-8")
    if 'accesskit_winit = "0.34.0"' not in cargo:
        errors.append("ui/Cargo.toml: AccessKit winit adapter/version is not the reviewed AT-SPI provider")

    app = (root / "ui/src/app.rs").read_text(encoding="utf-8")
    for needle, purpose in (
        ("accesskit_winit::Adapter::with_event_loop_proxy", "adapter registration before showing windows"),
        ("self.app.dispatch_a11y_action(req.target_node, &action)", "AT action routing"),
        ("self.a11y_adapter = None;", "provider teardown before window teardown"),
    ):
        if needle not in app:
            errors.append(f"ui/src/app.rs: missing shared AT-SPI {purpose}")

    # The native conversion preserves the Linux tree and applies the Windows
    # protected-value boundary. Require its returned shared tree to reach the
    # adapter, accepting both rustfmt layouts (including a trailing comma).
    tree_update = (
        r"adapter\.update_if_active\(\|\|\s*\{\s*"
        r"crate::accesskit_bridge::native_tree_update\(\s*"
        r"app_ref\.build_a11y_tree_update\(scale\),?\s*\)\s*\}\)"
    )
    if not re.search(tree_update, app):
        errors.append("ui/src/app.rs: missing shared AT-SPI tree updates")

    bridge = (root / "ui/src/accesskit_bridge.rs").read_text(encoding="utf-8")
    for needle, purpose in (
        ("ak.set_live(match politeness", "live-region export"),
        ("ak.set_live_atomic()", "atomic live-region updates"),
        ("if !access.state.protected", "protected-value boundary"),
    ):
        if needle not in bridge:
            errors.append(f"ui/src/accesskit_bridge.rs: missing AT-SPI {purpose}")

    native = (root / "shell/src/compositor/accessibility.rs").read_text(encoding="utf-8")
    for needle, purpose in (
        ("accesskit_unix::Adapter", "native shell provider"),
        ("self.rx.try_iter()", "nonblocking compositor handoff"),
        ("self.adapter.update_if_active", "native shell tree updates"),
    ):
        if needle not in native:
            errors.append(f"shell/src/compositor/accessibility.rs: missing {purpose}")
    return errors


def main() -> int:
    errors = audit()
    if errors:
        print("AT-SPI endpoint contract failed:", file=sys.stderr)
        print(*[f"- {error}" for error in errors], sep="\n", file=sys.stderr)
        return 1
    print("AT-SPI endpoint contract passed: every graphical app enters the shared provider.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
