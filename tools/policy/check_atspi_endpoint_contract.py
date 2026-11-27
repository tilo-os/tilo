#!/usr/bin/env python3
"""Audit that every graphical Tilo app uses the production accessibility endpoint.

Accessibility metadata in an app crate is not an endpoint.  On Linux there is
one exporter: the session.  A window sends its AccessKit tree over its
authenticated shell-ui connection to the session
(``FloatingSurfaceClientMessage::AccessibilityTree``), which publishes every
surface and every window over AT-SPI2 and writes what a client asks back to
the window (``FloatingSurfaceHostMessage::AccessibilityAction``).  No app
process holds a bus, so nothing may re-enable ``accesskit_unix`` in the
toolkit.

On Windows, macOS, iOS and Android there is no session: a window's winit
AccessKit adapter publishes to the platform service from the process that
draws it, and actions come back through the event loop.  Every graphical
binary must therefore enter through the standard ``ui::launch`` lifecycle (or
its small direct-App equivalent).  The source check is paired with the
installed-image D-Bus smoke to catch apps that bypass the shared endpoint.
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
    adapter through ``part_bar::window_tree_update``, which must build the
    app's tree and return it through the native conversion."""
    call = (
        r"adapter\.update_if_active\(\|\|\s*\{?\s*"
        r"part_bar::window_tree_update\(\s*app_ref,\s*\w+,\s*scale,?\s*\)\s*\}?\)"
    )
    if not re.search(call, source):
        return False
    body = function_body(source, "window_tree_update")
    if body is None:
        return False
    return bool(
        re.search(r"let\s+mut\s+update\s*=\s*app\.build_a11y_tree_update\(scale\);", body)
        and re.search(r"tilo_ui_base::accesskit_bridge::native_tree_update\(update\)\s*$", body)
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
        # this headless collaboration member.
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

    cargo = (root / "crates/ui/ui-app/Cargo.toml").read_text(encoding="utf-8")
    adapter = re.search(
        # Upstream 0.34.0, or the vendored fork of it built on tilo-winit.
        r'^accesskit_winit\s*=\s*(?:"0\.34\.0"|\{\s*version\s*=\s*"0\.34\.0"[^}\n]*\}'
        r'|\{\s*package\s*=\s*"tilo-accesskit-winit"\s*,\s*version\s*=\s*"0\.34\.0"\s*,'
        r'\s*path\s*=\s*"\.\./\.\./\.\./vendor/accesskit_winit"[^}\n]*\})',
        cargo,
        re.MULTILINE,
    )
    if adapter is None:
        errors.append("crates/ui/ui-app/Cargo.toml: AccessKit winit adapter/version is not the reviewed AT-SPI provider")
    elif re.search(r"\boptional\s*=\s*true\b", adapter.group(0)):
        # The adapter may sit behind the native-runner feature, but every
        # default build of `ui` must still carry it.
        features = tomllib.loads(cargo).get("features", {})
        if not any(
            "dep:accesskit_winit" in features.get(feature, [])
            for feature in features.get("default", [])
        ):
            errors.append("crates/ui/ui-app/Cargo.toml: AccessKit winit adapter is not enabled by default")

    app_paths = [
        root / "crates/ui/ui-app/src/app.rs",
        root / "crates/ui/ui-app/src/app/host.rs",
        root / "crates/ui/ui-app/src/app/native_runner.rs",
        root / "crates/ui/ui-app/src/app/part_bar.rs",
    ]
    app = "\n".join(
        path.read_text(encoding="utf-8") for path in app_paths if path.is_file()
    )
    errors.extend(
        f"crates/ui/ui-app/src/app.rs: {error}" for error in adapter_registration_errors(app)
    )
    for needle, purpose in (
        ("self.app.dispatch_a11y_action(req.target_node, &action)", "AT action routing"),
        ("self.a11y_adapter = None;", "provider teardown before window teardown"),
    ):
        if needle not in app:
            errors.append(f"crates/ui/ui-app/src/app.rs: missing shared AT-SPI {purpose}")

    # The native conversion preserves the Linux tree and applies the Windows
    # protected-value boundary. Require its returned shared tree to reach the
    # adapter, accepting both rustfmt layouts (including a trailing comma).
    tree_update = (
        r"adapter\.update_if_active\(\|\|\s*\{\s*"
        r"tilo_ui_base::accesskit_bridge::native_tree_update\(\s*"
        r"app_ref\.build_a11y_tree_update\(scale\),?\s*\)\s*\}\)"
    )
    if not re.search(tree_update, app) and not window_tree_reaches_adapter(app):
        errors.append("crates/ui/ui-app/src/app.rs: missing shared AT-SPI tree updates")

    # Linux: one publisher, the session's. A window sends its tree over the
    # shell-ui protocol it already speaks for its frames and controls, and the
    # session exports it; no app process may hold a bus again.
    if re.search(r"accesskit_unix", adapter.group(0)):
        errors.append(
            "crates/ui/ui-app/Cargo.toml: the toolkit enables accesskit_unix, "
            "which gives every app process its own AT-SPI publisher; the session is the one exporter"
        )
    if not re.search(
        r'^accesskit\s*=\s*\{[^}]*"serde"', cargo, re.MULTILINE
    ):
        errors.append(
            "crates/ui/ui-app/Cargo.toml: accesskit lacks the serde feature, "
            "so a window's tree cannot travel to the session"
        )
    window_targets = (
        root / "crates/ui/ui-app/src/app/window_targets.rs"
    ).read_text(encoding="utf-8")
    send_tree = function_body(window_targets, "send_accessibility_tree") or ""
    if not (
        re.search(r"build_a11y_tree_update\(scale\)", send_tree)
        and "AccessibilityTree" in send_tree
    ):
        errors.append(
            "crates/ui/ui-app/src/app/window_targets.rs: the window does not send "
            "its accessibility tree to the session over its shell-ui connection"
        )
    floating = (root / "crates/ui/ui-app/src/app/floating.rs").read_text(
        encoding="utf-8"
    )
    if not re.search(r"self\.send_accessibility_tree\(client, instance\)", floating):
        errors.append(
            "crates/ui/ui-app/src/app/floating.rs: the window never sends its "
            "accessibility tree on its shell-ui connection"
        )
    request = function_body(floating, "perform_accessibility_request") or ""
    if "dispatch_a11y_action" not in request:
        errors.append(
            "crates/ui/ui-app/src/app/floating.rs: an action a client of the "
            "export asks does not reach the window's assistive action path"
        )
    wire = (root / "crates/ui/ui-core/src/floating_surface/wire.rs").read_text(
        encoding="utf-8"
    )
    for variant, direction in (
        ("AccessibilityTree", "a window's tree to the session"),
        ("AccessibilityAction", "a client's action back to the window"),
    ):
        if variant not in wire:
            errors.append(
                f"crates/ui/ui-core/src/floating_surface/wire.rs: the shell-ui "
                f"protocol lacks {variant} ({direction})"
            )
    session_a11y = (root / "crates/desktop/session/src/a11y.rs").read_text(
        encoding="utf-8"
    )
    if not (
        "pub(crate) struct WindowExports" in session_a11y
        and "accesskit_unix::Adapter::new" in session_a11y
        and "push_action" in session_a11y
    ):
        errors.append(
            "crates/desktop/session/src/a11y.rs: the session does not publish app "
            "windows over AT-SPI and route clients' actions to them"
        )
    session_tabs = (root / "crates/desktop/session/src/app_tabs.rs").read_text(
        encoding="utf-8"
    )
    if not (
        "FloatingSurfaceClientMessage::AccessibilityTree" in session_tabs
        and "deliver_accessibility_now" in session_tabs
    ):
        errors.append(
            "crates/desktop/session/src/app_tabs.rs: the shell-ui service does not "
            "take windows' trees or write clients' actions back to them"
        )

    bridge = (root / "crates/ui/ui-base/src/accesskit_bridge.rs").read_text(encoding="utf-8")
    for needle, purpose in (
        ("ak.set_live(match politeness", "live-region export"),
        ("ak.set_live_atomic()", "atomic live-region updates"),
        ("if !access.state.protected", "protected-value boundary"),
    ):
        if needle not in bridge:
            errors.append(f"crates/ui/ui-base/src/accesskit_bridge.rs: missing AT-SPI {purpose}")

    # The source boundary above proves every graphical app reaches the shared
    # provider. These manifests declare the small, safe installed-image sample
    # that the independent D-Bus client actually drives. Keep it explicit: a
    # package may not silently leave core acceptance coverage by deleting a
    # scenario, and every scenario must contain an observable post-action tree
    # state rather than merely a successful in-process callback.
    # A core app declares its scenario in the package it is written as, a tilolang package
    # beside its `app.tl`, or a Rust app's own manifest; the names are the ones its window
    # exposes.
    for app, expected in {
        "files": {"root": "Home", "action": "View options"},
        "photos": {"root": "Photos", "action": "Favorites"},
        "settings": {"root": "Settings", "action": "Accessibility"},
    }.items():
        manifest = root / "apps" / app / "tilo-app.toml"
        written_as_a_package = root / "apps" / "tl" / app / "tilo-app.toml"
        if written_as_a_package.is_file():
            manifest = written_as_a_package
        if not manifest.is_file():
            errors.append(f"{manifest.relative_to(root)}: core app lacks external AT-SPI acceptance data")
            continue
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
        print(*errors, sep="\n")
        return 1
    print("AT-SPI endpoint contract passed: every graphical app enters the shared provider.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
