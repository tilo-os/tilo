#!/usr/bin/env python3
"""Audit that every graphical Tilo app uses the production AT-SPI endpoint.

Accessibility metadata in an app crate is not an endpoint.  The single
production endpoint is ``ui::App``: on Linux its winit AccessKit adapter owns
the AT-SPI provider, delivers actions back to the event loop, and is torn down
before the window.  Every graphical binary must therefore enter through the
standard ``ui::launch`` lifecycle (or its small direct-App equivalent).  The
source check is paired with the installed-image D-Bus smoke to catch apps that
bypass the shared endpoint.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def function_body(source: str, name: str) -> str | None:
    """Extract a named Rust body for this source-shape contract (not a parser)."""
    declaration = re.search(rf"\bfn\s+{re.escape(name)}\s*\([^{{}}]*\)[^{{}}]*\{{", source)
    if declaration is None:
        return None
    start = declaration.end()
    depth = 1
    for index in range(start, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[start:index]
    return None


def adapter_registration_errors(source: str) -> list[str]:
    """Follow the shared factory, including both primary and detached views."""
    if "fn create_accessibility_adapter(" not in source:
        if "accesskit_winit::Adapter::with_event_loop_proxy" in source:
            return []  # The original inline native runner.
        return ["missing shared AT-SPI adapter registration before showing windows"]
    errors = []
    factory = function_body(source, "create_accessibility_adapter") or ""
    if not re.search(r"type\s+PrimaryA11yAdapter\s*=\s*accesskit_winit::Adapter\s*;", source) or not re.search(
        r"Some\(PrimaryA11yAdapter::with_event_loop_proxy\(\s*event_loop,\s*window,\s*proxy,?\s*\)\)", factory
    ):
        errors.append("shared AT-SPI factory must return the reviewed native adapter")
    for name, binding in (
        ("resumed", r"self\.a11y_adapter"),
        ("create_native_floating_window", r"let\s+a11y_adapter"),
    ):
        body = function_body(source, name) or ""
        # Require retention of the actual result, not an unrelated/dropped call.
        registration = re.search(
            binding + r"\s*=\s*create_accessibility_adapter\(\s*event_loop,\s*&window,\s*self\.a11y_proxy\.clone\(\),?\s*\)\s*;",
            body,
        )
        creation = body.find("event_loop.create_window(")
        if creation < 0:
            creation_match = re.search(r"event_loop\s*\.create_window\(", body)
            creation = creation_match.start() if creation_match else -1
        shown = body.find("window.set_visible(true);", max(creation, 0))
        if registration is None or not (0 <= creation < registration.start() < shown):
            errors.append(f"{name}: shared AT-SPI adapter must be retained after creation and before showing the new window")
    return errors


def window_tree_reaches_adapter(source: str) -> bool:
    """The window of its own's tree: the app's shared tree, with the row of
    window-bar slots the runner draws published beside it, reaches the
    adapter through ``slot_bar::window_tree_update``, which must build the
    app's tree and return it through the native conversion."""
    call = (
        r"adapter\.update_if_active\(\|\|\s*\{?\s*"
        r"slot_bar::window_tree_update\(\s*app_ref,\s*\w+,\s*scale,?\s*\)\s*\}?\)"
    )
    if not re.search(call, source):
        return False
    body = function_body(source, "window_tree_update")
    if body is None:
        return False
    return bool(
        re.search(r"let\s+mut\s+update\s*=\s*app\.build_a11y_tree_update\(scale\);", body)
        and re.search(r"crate::accesskit_bridge::native_tree_update\(update\)\s*$", body)
    )


def audit(root: Path = ROOT) -> list[str]:
    errors: list[str] = []
    app_root = root / "apps"
    for manifest in sorted(app_root.glob("*/Cargo.toml")):
        main = manifest.parent / "src/main.rs"
        if not main.is_file():
            continue
        source = main.read_text(encoding="utf-8")
        # `tilo-agent` links the app-model facade to consume
        # resource descriptors, but has no window. The launch specification and
        # direct App construction are the two windowed entrypoint shapes; skip
        # this headless collaboration participant.
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

    cargo = (root / "crates/sdk/ui-core/Cargo.toml").read_text(encoding="utf-8")
    adapter = re.search(
        # Upstream 0.34.0, or the vendored fork of it built on tilo-winit.
        r'^accesskit_winit\s*=\s*(?:"0\.34\.0"|\{\s*version\s*=\s*"0\.34\.0"[^}\n]*\}'
        r'|\{\s*package\s*=\s*"tilo-accesskit-winit"\s*,\s*version\s*=\s*"0\.34\.0"\s*,'
        r'\s*path\s*=\s*"\.\./\.\./\.\./vendor/accesskit_winit"[^}\n]*\})',
        cargo,
        re.MULTILINE,
    )
    if adapter is None:
        errors.append("crates/sdk/ui-core/Cargo.toml: AccessKit winit adapter/version is not the reviewed AT-SPI provider")
    elif re.search(r"\boptional\s*=\s*true\b", adapter.group(0)):
        # The adapter may sit behind the native-runner feature, but every
        # default build of `ui` must still carry it.
        features = tomllib.loads(cargo).get("features", {})
        if not any(
            "dep:accesskit_winit" in features.get(feature, [])
            for feature in features.get("default", [])
        ):
            errors.append("crates/sdk/ui-core/Cargo.toml: AccessKit winit adapter is not enabled by default")

    app_paths = [
        root / "crates/sdk/ui-core/src/app.rs",
        root / "crates/sdk/ui-core/src/app/host.rs",
        root / "crates/sdk/ui-core/src/app/native_runner.rs",
        root / "crates/sdk/ui-core/src/app/native_runner/slot_bar.rs",
    ]
    app = "\n".join(
        path.read_text(encoding="utf-8") for path in app_paths if path.is_file()
    )
    errors.extend(
        f"crates/sdk/ui-core/src/app.rs: {error}" for error in adapter_registration_errors(app)
    )
    for needle, purpose in (
        ("self.app.dispatch_a11y_action(req.target_node, &action)", "AT action routing"),
        ("self.a11y_adapter = None;", "provider teardown before window teardown"),
    ):
        if needle not in app:
            errors.append(f"crates/sdk/ui-core/src/app.rs: missing shared AT-SPI {purpose}")

    # The native conversion preserves the Linux tree and applies the Windows
    # protected-value boundary. Require its returned shared tree to reach the
    # adapter, accepting both rustfmt layouts (including a trailing comma).
    tree_update = (
        r"adapter\.update_if_active\(\|\|\s*\{\s*"
        r"crate::accesskit_bridge::native_tree_update\(\s*"
        r"app_ref\.build_a11y_tree_update\(scale\),?\s*\)\s*\}\)"
    )
    if not re.search(tree_update, app) and not window_tree_reaches_adapter(app):
        errors.append("crates/sdk/ui-core/src/app.rs: missing shared AT-SPI tree updates")

    bridge = (root / "crates/sdk/ui-core/src/accesskit_bridge.rs").read_text(encoding="utf-8")
    for needle, purpose in (
        ("ak.set_live(match politeness", "live-region export"),
        ("ak.set_live_atomic()", "atomic live-region updates"),
        ("if !access.state.protected", "protected-value boundary"),
    ):
        if needle not in bridge:
            errors.append(f"crates/sdk/ui-core/src/accesskit_bridge.rs: missing AT-SPI {purpose}")

    native = (root / "crates/session/shell/src/compositor/accessibility.rs").read_text(encoding="utf-8")
    for needle, purpose in (
        ("accesskit_unix::Adapter", "native shell provider"),
        ("self.rx.try_iter()", "nonblocking compositor handoff"),
        ("wake.ping()", "idle-compositor wakeup"),
        ("self.adapter.update_if_active", "native shell tree updates"),
    ):
        if needle not in native:
            errors.append(f"crates/session/shell/src/compositor/accessibility.rs: missing {purpose}")

    native_loop = (root / "crates/session/shell/src/compositor/native.rs").read_text(encoding="utf-8")
    if "drop(native_accessibility);" not in native_loop:
        errors.append("crates/session/shell/src/compositor/native.rs: missing native AT-SPI provider teardown")

    # The source boundary above proves every graphical app reaches the shared
    # provider. These manifests declare the small, safe installed-image sample
    # that the independent D-Bus client actually drives. Keep it explicit: a
    # package may not silently leave core acceptance coverage by deleting a
    # scenario, and every scenario must contain an observable post-action tree
    # state rather than merely a successful in-process callback.
    for app, expected in {
        "file-explorer": {"root": "Files activity", "action": "View options"},
        "photos": {"root": "Photos", "action": "Favorites"},
        "settings": {"root": "Settings", "action": "Accessibility"},
    }.items():
        manifest = root / "apps" / app / "tilo-app.toml"
        with manifest.open("rb") as source:
            document = tomllib.load(source)
        scenario = document.get("accessibility", {}).get("atspi_acceptance")
        if not isinstance(scenario, dict):
            errors.append(f"{manifest.relative_to(root)}: core app lacks external AT-SPI acceptance data")
            continue
        for key, value in expected.items():
            if scenario.get(key) != value:
                errors.append(f"{manifest.relative_to(root)}: AT-SPI acceptance {key} must be {value!r}")
        for key in ("required", "after_action"):
            values = scenario.get(key)
            if not isinstance(values, list) or not values or not all(isinstance(item, str) and item for item in values):
                errors.append(f"{manifest.relative_to(root)}: AT-SPI acceptance {key} must be a non-empty string list")

    errors += external_client_errors(root)
    return errors


def external_client_errors(root: Path) -> list[str]:
    """The external AT-SPI client is test tooling: the VM harness hands the checkout's copy
    to a booted guest, and the image must not carry it (no Python ships in the image)."""
    errors = []
    client = "tools/atspi_external_client.py"
    if not (root / client).is_file():
        errors.append(f"{client}: the external AT-SPI client is absent")
    harness = root / "os/dev/vm.sh"
    if not harness.is_file() or f'"{client} ' not in harness.read_text(encoding="utf-8"):
        errors.append("os/dev/vm.sh: the VM harness does not deliver the external AT-SPI client into the guest")
    containerfile = root / "os/Containerfile"
    if containerfile.is_file() and client in containerfile.read_text(encoding="utf-8"):
        errors.append("os/Containerfile: the image ships the external AT-SPI client, which is test tooling")
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
