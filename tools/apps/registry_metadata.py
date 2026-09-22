#!/usr/bin/env python3
"""Build complete, signed store presentation metadata for the Tilo app fleet.

The registry is deliberately not an app's source of truth for product copy.
This tool joins checked-in copy with the package manifest and publishes a
small, owned artwork bundle alongside the immutable `.tapp` artifacts. Every
entry gets an English fallback, a non-empty icon, and an accurate list of UI
languages. Existing checked-in screenshots are included when they exist;
otherwise a generated, product-specific SVG remains a useful, accessible card
instead of a blank placeholder.
"""

from __future__ import annotations

import argparse
import html
import json
from pathlib import Path
import re
import shutil
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[2]
CATALOG = ROOT / "dist/apps/registry-metadata.toml"
SOURCE_URL = "https://code.sandbox.example/qos/qos"
PUBLIC_IMAGE_HOST = "https://apps.tilo.so/"
LANGUAGE_FILE = re.compile(r"^([A-Za-z]{2,3})(?:[-_][A-Za-z0-9]+)?\.(?:ftl|json|po)$")


def die(message: str) -> None:
    raise ValueError(message)


def load_catalog(path: Path) -> dict[str, dict[str, str]]:
    raw = tomllib.loads(path.read_text(encoding="utf-8"))
    apps = raw.get("apps")
    if not isinstance(apps, dict):
        die(f"{path}: expected [apps.<name>] tables")
    parsed: dict[str, dict[str, str]] = {}
    for app, item in apps.items():
        if not isinstance(item, dict):
            die(f"{path}: apps.{app} must be a table")
        description, tagline = item.get("description"), item.get("tagline")
        if not isinstance(description, str) or not description.strip():
            die(f"{path}: apps.{app}.description must be non-empty")
        if not isinstance(tagline, str) or not tagline.strip():
            die(f"{path}: apps.{app}.tagline must be non-empty")
        parsed[app] = {"description": description.strip(), "tagline": tagline.strip()}
    return parsed


def manifest(root: Path, app: str) -> dict:
    path = root / "apps" / app / "tilo-app.toml"
    if not path.is_file():
        die(f"missing manifest for {app}: {path}")
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    package = data.get("package", {})
    if not isinstance(package.get("id"), str) or not package["id"].startswith("so.tilo."):
        die(f"{path}: package.id must use so.tilo.")
    if not isinstance(package.get("name"), str) or not package["name"].strip():
        die(f"{path}: package.name must be non-empty")
    return package


def fleet(root: Path) -> list[str]:
    # Browser is an OS-integrated WPE application, deliberately outside the
    # independently downloadable app fleet.
    return sorted(path.parent.name for path in (root / "apps").glob("*/tilo-app.toml")
                  if path.parent.name != "browser")


def languages(root: Path, app: str) -> list[str]:
    locale_dir = root / "apps" / app / "locales"
    tags = {"en"}
    if locale_dir.is_dir():
        for source in locale_dir.iterdir():
            match = LANGUAGE_FILE.match(source.name)
            if match:
                tags.add(match.group(1).lower())
    return sorted(tags)


def slug(value: str) -> str:
    cleaned = re.sub(r"[^a-z0-9]+", "-", value.lower()).strip("-")
    if not cleaned:
        die(f"cannot make an artwork filename from {value!r}")
    return cleaned


def svg_icon(name: str, category: str) -> str:
    # A tiny, deterministic owned poster.  A single visual language makes the
    # un-shot apps feel intentional; the label keeps it useful in a screen
    # reader and when a screenshot is unavailable.
    hue = sum(name.encode("utf-8")) % 360
    title = html.escape(name)
    subtitle = html.escape(category or "Tilo app")
    initial = html.escape(name[:1].upper())
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="768" height="432" viewBox="0 0 768 432" role="img" aria-labelledby="title desc">
  <title id="title">{title}</title><desc id="desc">{title} for Tilo</desc>
  <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop stop-color="hsl({hue} 72% 48%)"/><stop offset="1" stop-color="hsl({(hue + 44) % 360} 64% 26%)"/></linearGradient></defs>
  <rect width="768" height="432" rx="48" fill="url(#g)"/>
  <circle cx="122" cy="118" r="64" fill="white" fill-opacity=".18"/><text x="122" y="145" text-anchor="middle" fill="white" font-family="system-ui, sans-serif" font-size="88" font-weight="700">{initial}</text>
  <text x="72" y="316" fill="white" font-family="system-ui, sans-serif" font-size="52" font-weight="700">{title}</text><text x="74" y="360" fill="white" fill-opacity=".78" font-family="system-ui, sans-serif" font-size="24">{subtitle}</text>
</svg>\n'''


def screenshots(root: Path, app: str) -> list[Path]:
    shots = root / "apps" / app / "shots"
    if not shots.is_dir():
        return []
    return sorted(path for path in shots.iterdir()
                  if path.is_file() and path.suffix.lower() in {".png", ".jpg", ".jpeg", ".webp"})[:4]


def build_one(root: Path, app: str, copy: dict[str, str], media_base: str,
              output: Path, media_dir: Path) -> None:
    package = manifest(root, app)
    name = package["name"].strip()
    category = package.get("category", "Tilo app")
    key = slug(app)
    icon_file = f"{key}.svg"
    (media_dir / icon_file).write_text(svg_icon(name, category), encoding="utf-8", newline="\n")
    images = [{
        "kind": "icon", "url": f"{media_base}/{icon_file}", "alt": f"{name} icon",
    }]
    for number, source in enumerate(screenshots(root, app), start=1):
        filename = f"{key}-screenshot-{number}{source.suffix.lower()}"
        shutil.copyfile(source, media_dir / filename)
        images.append({
            "kind": "screenshot", "url": f"{media_base}/{filename}",
            "alt": f"{name} screenshot {number}",
        })
    entry = {
        "name": {"en": name},
        "description": {"en": copy["description"]},
        "tagline": {"en": copy["tagline"]},
        "images": {"en": images},
        "supported_languages": languages(root, app),
        "source_url": SOURCE_URL,
    }
    if not entry["images"]["en"]:
        die(f"{app}: generated no product images")
    if not all(url["url"].startswith(PUBLIC_IMAGE_HOST) for url in images):
        die(f"{app}: artwork must be served from apps.tilo.so")
    (output / f"{app}.json").write_text(json.dumps(entry, indent=2) + "\n", encoding="utf-8")


def validate(root: Path, catalog: Path) -> tuple[dict[str, dict[str, str]], list[str]]:
    copy = load_catalog(catalog)
    apps = fleet(root)
    missing, extra = sorted(set(apps) - set(copy)), sorted(set(copy) - set(apps))
    if missing or extra:
        detail = []
        if missing:
            detail.append("missing=" + ", ".join(missing))
        if extra:
            detail.append("unknown=" + ", ".join(extra))
        die("registry metadata coverage mismatch: " + "; ".join(detail))
    for app in apps:
        manifest(root, app)
        if not languages(root, app):
            die(f"{app}: no supported language")
    return copy, apps


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("audit", "build"))
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--catalog", type=Path, default=CATALOG)
    parser.add_argument("--media-base", default="")
    parser.add_argument("--apps", default="", help="space/comma-separated app directory names to build")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--media-output", type=Path)
    args = parser.parse_args()
    try:
        copy, apps = validate(args.root, args.catalog)
        if args.command == "audit":
            print(json.dumps({"apps": apps, "count": len(apps)}, indent=2))
            return 0
        if not args.media_base.startswith(PUBLIC_IMAGE_HOST):
            die("--media-base must be a public https://apps.tilo.so/ URL")
        if args.output is None or args.media_output is None:
            die("build requires --output and --media-output")
        args.output.mkdir(parents=True, exist_ok=True)
        args.media_output.mkdir(parents=True, exist_ok=True)
        selected = apps if not args.apps else sorted(set(filter(None, re.split(r"[ ,]+", args.apps))))
        unknown = sorted(set(selected) - set(apps))
        if unknown:
            die("requested apps are not in the downloadable fleet: " + ", ".join(unknown))
        for app in selected:
            build_one(args.root, app, copy[app], args.media_base.rstrip("/"), args.output, args.media_output)
    except (OSError, tomllib.TOMLDecodeError, ValueError) as error:
        print(f"registry metadata: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
