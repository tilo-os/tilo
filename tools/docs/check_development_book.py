"""Check package/target names in the development book's literal Cargo examples."""
import re
from pathlib import Path
import shlex
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "docs/development/pages.toml"
SELECTORS = {"-p": "package", "--package": "package", "--bin": "bin", "--test": "test", "--example": "example"}


def packages(root):
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    result = {}
    for member in workspace["members"]:
        for path in root.glob(member):
            manifest = path / "Cargo.toml"
            if manifest.is_file():
                data = tomllib.loads(manifest.read_text())
                result[data["package"]["name"]] = (path, data)
    return result


def target_names(path, manifest, kind):
    names = {entry["name"] for entry in manifest.get(kind, [])}
    directory = {"bin": "src/bin", "test": "tests", "example": "examples"}[kind]
    auto = manifest["package"].get("auto" + {"bin": "bins", "test": "tests", "example": "examples"}[kind], True)
    if auto:
        names.update(file.stem for file in (path / directory).glob("*.rs"))
        names.update(file.parent.name for file in (path / directory).glob("*/main.rs"))
        if kind == "bin" and (path / "src/main.rs").is_file():
            names.add(manifest["package"]["name"])
    return names


def cargo_examples(text):
    language = None
    pending = ""
    start = 0
    for number, line in enumerate(text.splitlines(), 1):
        fence = re.match(r"^\s*(`{3,}|~{3,})(.*)$", line)
        if fence:
            language = fence[2].strip() if language is None else None
            pending = ""
            continue
        if language not in {"sh", "bash", "shell", "powershell"}:
            continue
        stripped = line.strip()
        if not pending:
            start = number
        pending += stripped.rstrip("\\`") + " "
        if stripped.endswith(("\\", "`")):
            continue
        command = pending.strip()
        pending = ""
        if command.startswith("cargo "):
            yield start, shlex.split(command, comments=True)


def check_command(words, inventory):
    selected = []
    targets = []
    index = 1
    while index < len(words):
        word = words[index]
        if word == "--":
            break
        name, equal, value = word.partition("=")
        if name in SELECTORS:
            if not equal:
                index += 1
                if index >= len(words):
                    return [f"{name} has no value"], False
                value = words[index]
            if value.isupper() or "$" in value or "<" in value:
                return [], False  # A documented substitution, not an executable example.
            if SELECTORS[name] == "package":
                selected.append(value)
            else:
                targets.append((SELECTORS[name], value))
        index += 1
    errors = [f"no Cargo package named {name!r}" for name in selected if name not in inventory]
    owners = [inventory[name] for name in selected if name in inventory] if selected else list(inventory.values())
    for kind, target in targets:
        if not any(target in target_names(path, data, kind) for path, data in owners):
            errors.append(f"no {kind} target named {target!r} in the selected packages")
    return errors, bool(selected or targets)


def main():
    inventory = packages(ROOT)
    book = tomllib.loads((ROOT / MANIFEST).read_text())
    errors = []
    checked = 0
    for page in book["page"]:
        source = page["source"]
        for number, command in cargo_examples((ROOT / source).read_text()):
            problems, observed = check_command(command, inventory)
            checked += observed
            errors.extend(f"{source}:{number}: {problem}" for problem in problems)
    if not checked:
        errors.append("development book has no literal package/target examples to audit")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"development Cargo examples OK ({checked} package/target selections; no compilation performed)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
