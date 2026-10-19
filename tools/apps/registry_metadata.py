#!/usr/bin/env python3
"""Build complete, signed store presentation metadata for the Tilo app fleet.

The registry is deliberately not an app's source of truth for product copy.
This tool joins checked-in copy with the package manifest and publishes a
small, owned artwork bundle alongside the immutable `.tapp` artifacts. Every
entry gets an English fallback, a non-empty icon, and an accurate list of UI
languages. Checked-in and checksum-pinned archived screenshots are included;
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
SOURCE_URL = "https://code.tilo.so/tilo/tilo"
PUBLIC_IMAGE_HOST = "https://apps.tilo.so/"
LANGUAGE_FILE = re.compile(r"^([A-Za-z]{2,3})(?:[-_][A-Za-z0-9]+)?\.(?:ftl|json|po)$")


def die(message: str) -> None:
    raise ValueError(message)


TAGLINE_MESSAGE = "app-tagline"
MAX_TAGLINE_CHARS = 80


def load_catalog(path: Path) -> dict[str, dict[str, str]]:
    raw = tomllib.loads(path.read_text(encoding="utf-8"))
    apps = raw.get("apps")
    if not isinstance(apps, dict):
        die(f"{path}: expected [apps.<name>] tables")
    parsed: dict[str, dict[str, str]] = {}
    for app, item in apps.items():
        if not isinstance(item, dict):
            die(f"{path}: apps.{app} must be a table")
        if "tagline" in item:
            die(f"{path}: apps.{app}.tagline moved to apps/{app}/search/<tag>.ftl ({TAGLINE_MESSAGE})")
        description = item.get("description")
        if not isinstance(description, str) or not description.strip():
            die(f"{path}: apps.{app}.description must be non-empty")
        parsed[app] = {"description": description.strip()}
    return parsed


def taglines(root: Path, app: str) -> dict[str, str]:
    """The app's tag line per language, from `search/<tag>.ftl`'s `app-tagline`:
    the same message the packer signs into `[package.taglines]`."""
    search = root / "apps" / app / "search"
    found: dict[str, str] = {}
    if search.is_dir():
        for source in sorted(search.glob("*.ftl")):
            for line in source.read_text(encoding="utf-8").splitlines():
                if line[:1].isspace() or "=" not in line:
                    continue
                key, value = line.split("=", 1)
                if key.strip() == TAGLINE_MESSAGE and value.strip():
                    found[source.stem.split("-")[0].lower()] = value.strip()
    if "en" not in found:
        die(f"{app}: apps/{app}/search/en.ftl needs an {TAGLINE_MESSAGE} message")
    for tag, value in found.items():
        if len(value) > MAX_TAGLINE_CHARS or "\n" in value:
            die(f"{app}: the {tag} tag line must be one line of at most {MAX_TAGLINE_CHARS} characters")
    return found


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
    return sorted(path.parent.name for path in (root / "apps").glob("*/tilo-app.toml"))


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
    """The app's screenshots: the package's own `screenshots/` (the files its
    signed manifest lists) first, else the older `shots/` directory."""
    images = {".png", ".jpg", ".jpeg", ".webp"}
    for directory in ("screenshots", "shots"):
        shots = root / "apps" / app / directory
        if not shots.is_dir():
            continue
        found = sorted(path for path in shots.iterdir()
                       if path.is_file() and path.suffix.lower() in images)
        if found:
            return found[:4]
    return []


def archived_screenshots(root: Path, app: str) -> list[dict]:
    archive = root / "apps" / app / "shots" / "archive.json"
    if not archive.is_file():
        return []
    rows = json.loads(archive.read_text(encoding="utf-8"))
    if not isinstance(rows, list):
        die(f"{archive}: expected a screenshot list")
    seen = set()
    for row in rows:
        if not isinstance(row, dict):
            die(f"{archive}: expected screenshot records")
        name, sha, size = row.get("name"), row.get("sha256"), row.get("bytes")
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+\.(png|jpg|jpeg|webp)", name):
            die(f"{archive}: invalid screenshot filename")
        if name in seen or not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{64}", sha):
            die(f"{archive}: duplicate screenshot or invalid SHA-256")
        if type(size) is not int or not 0 < size <= 32 * 1024 * 1024:
            die(f"{archive}: invalid screenshot byte length")
        expected = f"{PUBLIC_IMAGE_HOST}artifacts/tilo/generic/tilo-app-metadata/{sha}/{name}"
        if row.get("url") != expected:
            die(f"{archive}: screenshot must use its immutable public artifact URL")
        seen.add(name)
    return rows


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
    sources = {path.name: {"path": path} for path in screenshots(root, app)}
    for archived in archived_screenshots(root, app):
        if archived["name"] in sources:
            die(f"{app}: screenshot exists both locally and in the archive")
        sources[archived["name"]] = archived
    for number, filename in enumerate(sorted(sources)[:4], start=1):
        source = sources[filename]
        if "path" in source:
            local = source["path"]
            destination = f"{key}-screenshot-{number}{local.suffix.lower()}"
            shutil.copyfile(local, media_dir / destination)
            image_url = f"{media_base}/{destination}"
        else:
            image_url = source["url"]
        images.append({
            "kind": "screenshot", "url": image_url,
            "alt": f"{name} screenshot {number}",
        })
    entry = {
        "name": {"en": name},
        "description": {"en": copy["description"]},
        "tagline": taglines(root, app),
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
        taglines(root, app)
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
