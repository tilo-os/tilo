#!/usr/bin/env python3
"""Give an app library the `definition()` every in-process host mounts.

For apps whose binary builds `LaunchSpec::new(<id>, <crate>::app_root)`, the
library gains

    pub fn definition() -> &'static ui::application::ApplicationDefinition {
        ui::launch::LaunchSpec::new("<id>", app_root)
            .component_actions(actions::register)   // when the app has actions
            .as_definition("<name>")
    }

with id and name from tilo-app.toml.

usage: app_definition.py APP_DIR...
"""

from __future__ import annotations

import pathlib
import re
import sys
import tomllib


def definition(app: pathlib.Path) -> str | None:
    lib = app / "src/lib.rs"
    text = lib.read_text()
    if re.search(r"^pub fn definition\(\)", text, re.M):
        return None
    if not re.search(r"^pub fn app_root\(\) -> Box<dyn (ui::widget::)?Widget>", text, re.M):
        raise SystemExit(f"{app}: no `pub fn app_root() -> Box<dyn Widget>` in src/lib.rs")
    package = tomllib.loads((app / "tilo-app.toml").read_text())["package"]
    actions = any(
        (app / path).exists()
        and re.search(r"pub fn register\(registry: &mut", (app / path).read_text())
        for path in ("src/actions.rs", "src/actions/mod.rs")
    ) and re.search(r"^\s*(pub )?mod actions;", text, re.M)
    body = [f'    ui::launch::LaunchSpec::new("{package["id"]}", app_root)']
    if actions:
        body.append("        .component_actions(actions::register)")
    body.append(f'        .as_definition("{package["name"]}")')
    return (
        "\n/// The app as every host mounts it: the live root"
        + (" and its actions.\n" if actions else ".\n")
        + "pub fn definition() -> &'static ui::application::ApplicationDefinition {\n"
        + "\n".join(body)
        + "\n}\n"
    )


def main(argv: list[str]) -> int:
    for app in map(pathlib.Path, argv):
        addition = definition(app)
        if addition is None:
            print(f"{app}: already has definition()")
            continue
        lib = app / "src/lib.rs"
        text = lib.read_text()
        match = re.search(r"^pub fn app_root\(\)[^{]*\{.*?^\}\n", text, re.M | re.S)
        assert match, app
        lib.write_text(text[: match.end()] + addition + text[match.end() :])
        print(f"{app}: added definition()")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
