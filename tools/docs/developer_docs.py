#!/usr/bin/env python3
"""Check and export the app-developer documentation published on tilo.so.

`docs/developers/pages.toml` lists the published pages, which stay where they
are in the repository. `check` renders each of them and fails when a page is
missing or has no title, or when a link names a file, directory or heading
that does not exist. `export --out DIR` writes what the site consumes:

    DIR/manifest.json     the navigation: sections, pages, the API reference
    DIR/pages/<id>.md     each page with front matter, links rewritten

A link between listed pages becomes the other page's site address. A link to
an API source in `[api].sources` becomes the API reference. A link to any
other file or directory becomes that path on the repository host, pinned to
`--commit` when one is given. Links inside code are left alone.
"""

from __future__ import annotations

import argparse
import json
import posixpath
import re
import shutil
import subprocess
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "docs/developers/pages.toml"
SLUG = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})")
HEADING = re.compile(r"^ {0,3}(#{1,6})[ \t]+(.+?)[ \t]*#*[ \t]*$")
CODE_SPAN = re.compile(r"(`+)(?!`)(.+?)(?<!`)\1(?!`)")
INLINE_LINK = re.compile(
    r"(?P<image>!?)\[(?P<text>(?:[^\[\]\\]|\\.|\[[^\]]*\])*)\]"
    r"\((?P<space>\s*)(?P<target><[^>\n]*>|[^)\s]+)(?P<title>\s+\"[^\"]*\")?\s*\)"
)
REFERENCE_LINK = re.compile(r"^(?P<lead> {0,3}\[[^\]]+\]:[ \t]*)(?P<target><[^>]*>|\S+)(?P<rest>.*)$")
EXTERNAL = re.compile(r"^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|//)")


class DocsError(Exception):
    pass


@dataclass(frozen=True)
class Page:
    slug: str
    source: str
    section: str
    nav: str
    description: str

    @property
    def id(self) -> str:
        return self.slug or "index"


@dataclass(frozen=True)
class Manifest:
    base: str
    repository: str
    api_title: str
    api_path: str
    api_sources: frozenset[str]
    pages: tuple[Page, ...]

    def url(self, page: Page) -> str:
        return f"{self.base}{page.slug}/" if page.slug else self.base


@dataclass
class Rendered:
    page: Page
    title: str
    markdown: str
    anchors: set[str]
    problems: list[str] = field(default_factory=list)


def load_manifest(root: Path) -> Manifest:
    path = root / MANIFEST
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise DocsError(f"{MANIFEST}: {error}") from error
    base = data.get("base", "")
    if not (base.startswith("/") and base.endswith("/")):
        raise DocsError(f"{MANIFEST}: base must start and end with '/'")
    api = data.get("api", {})
    pages = []
    seen_slugs: set[str] = set()
    seen_sources: set[str] = set()
    for entry in data.get("page", []):
        unknown = set(entry) - {"slug", "source", "section", "nav", "description"}
        if unknown:
            raise DocsError(f"{MANIFEST}: unknown page keys {sorted(unknown)}")
        page = Page(
            slug=entry.get("slug", ""),
            source=entry.get("source", ""),
            section=entry.get("section", ""),
            nav=entry.get("nav", ""),
            description=entry.get("description", ""),
        )
        if page.slug and not SLUG.fullmatch(page.slug):
            raise DocsError(f"{MANIFEST}: slug {page.slug!r} is not lowercase kebab case")
        if page.slug in seen_slugs:
            raise DocsError(f"{MANIFEST}: slug {page.slug!r} is listed twice")
        if page.source in seen_sources:
            raise DocsError(f"{MANIFEST}: {page.source} is listed twice")
        if not (page.source and page.section and page.nav and page.description):
            raise DocsError(f"{MANIFEST}: page {page.slug!r} needs source, section, nav and description")
        seen_slugs.add(page.slug)
        seen_sources.add(page.source)
        pages.append(page)
    if not pages or pages[0].slug:
        raise DocsError(f"{MANIFEST}: the first page must have slug = \"\" (the section's index)")
    return Manifest(
        base=base,
        repository=data.get("repository", "").rstrip("/"),
        api_title=api.get("title", "API reference"),
        api_path=api.get("path", ""),
        api_sources=frozenset(api.get("sources", [])),
        pages=tuple(pages),
    )


def slugify(heading: str) -> str:
    """The heading id Astro assigns (github-slugger) for rendered heading text."""
    return re.sub(r"[^\w\- ]", "", heading.lower()).replace(" ", "-")


def heading_text(markdown: str) -> str:
    text = CODE_SPAN.sub(lambda match: match.group(2).strip(), markdown)
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]+>", "", text)
    text = re.sub(r"(\*\*|__|\*)(.+?)\1", r"\2", text)
    return text.replace("\\", "").strip()


def prose_lines(markdown: str):
    """Yield (index, line, in_code) for every line, tracking fenced code."""
    fence: str | None = None
    for index, line in enumerate(markdown.split("\n")):
        match = FENCE.match(line)
        if fence is None and match:
            fence = match.group(1)
            yield index, line, True
        elif fence is not None:
            if match and match.group(1)[0] == fence[0] and len(match.group(1)) >= len(fence) \
                    and not line.strip()[len(match.group(1)):].strip():
                fence = None
            yield index, line, True
        else:
            yield index, line, False


def headings(markdown: str) -> list[tuple[int, str]]:
    found = []
    for _, line, in_code in prose_lines(markdown):
        if in_code:
            continue
        match = HEADING.match(line)
        if match:
            found.append((len(match.group(1)), heading_text(match.group(2))))
    return found


def anchors_of(markdown: str) -> set[str]:
    anchors: set[str] = set()
    counts: dict[str, int] = {}
    for _, text in headings(markdown):
        slug = slugify(text)
        count = counts.get(slug, 0)
        counts[slug] = count + 1
        anchors.add(slug if count == 0 else f"{slug}-{count}")
    for match in re.finditer(r"""\b(?:id|name)=["']([^"']+)["']""", markdown):
        anchors.add(match.group(1))
    return anchors


class Linker:
    def __init__(self, root: Path, manifest: Manifest, commit: str | None):
        self.root = root
        self.manifest = manifest
        self.by_source = {page.source: page for page in manifest.pages}
        self.ref = f"commit/{commit}" if commit else "branch/main"
        self.anchors: dict[str, set[str]] = {}

    def resolve(self, page: Page, target: str, image: bool) -> tuple[str, str | None]:
        """Return the rewritten target and a problem, if any."""
        if EXTERNAL.match(target):
            return target, None
        path, _, fragment = target.partition("#")
        if not path:
            return target, self.check_anchor(page.source, fragment)
        if path.startswith("/"):
            repo_path = posixpath.normpath(path.lstrip("/"))
        else:
            repo_path = posixpath.normpath(posixpath.join(posixpath.dirname(page.source), path))
        if repo_path == ".." or repo_path.startswith("../"):
            return target, f"{target}: leaves the repository"
        repo_path = "" if repo_path == "." else repo_path
        suffix = f"#{fragment}" if fragment else ""
        linked = self.by_source.get(repo_path)
        if linked is not None:
            return self.manifest.url(linked) + suffix, self.check_anchor(repo_path, fragment)
        if repo_path.rstrip("/") in self.manifest.api_sources:
            return self.manifest.base + self.manifest.api_path, None
        on_disk = self.root / repo_path
        if not on_disk.exists():
            return target, f"{target}: {repo_path or '.'} does not exist"
        kind = "raw" if image and on_disk.is_file() else "src"
        return f"{self.manifest.repository}/{kind}/{self.ref}/{repo_path}".rstrip("/") + suffix, None

    def check_anchor(self, source: str, fragment: str) -> str | None:
        if not fragment or source not in self.by_source:
            return None
        if source not in self.anchors:
            text = (self.root / source).read_text(encoding="utf-8")
            self.anchors[source] = anchors_of(text)
        if fragment not in self.anchors[source]:
            return f"#{fragment}: no such heading in {source}"
        return None

    def rewrite_line(self, page: Page, line: str, problems: list[str], number: int) -> str:
        spans: list[str] = []

        def hide(match: re.Match[str]) -> str:
            spans.append(match.group(0))
            return f"\0{len(spans) - 1}\0"

        masked = CODE_SPAN.sub(hide, line)

        def inline(match: re.Match[str]) -> str:
            raw = match.group("target")
            bracketed = raw.startswith("<")
            target = raw[1:-1] if bracketed else raw
            new, problem = self.resolve(page, target, bool(match.group("image")))
            if problem:
                problems.append(f"{page.source}:{number}: {problem}")
            new = f"<{new}>" if bracketed else new
            return (f"{match.group('image')}[{match.group('text')}]({match.group('space')}"
                    f"{new}{match.group('title') or ''})")

        rewritten = INLINE_LINK.sub(inline, masked)
        reference = REFERENCE_LINK.match(rewritten)
        if reference:
            raw = reference.group("target")
            bracketed = raw.startswith("<")
            new, problem = self.resolve(page, raw[1:-1] if bracketed else raw, False)
            if problem:
                problems.append(f"{page.source}:{number}: {problem}")
            new = f"<{new}>" if bracketed else new
            rewritten = f"{reference.group('lead')}{new}{reference.group('rest')}"
        return re.sub(r"\0(\d+)\0", lambda match: spans[int(match.group(1))], rewritten)

    def render(self, page: Page) -> Rendered:
        path = self.root / page.source
        if not path.is_file():
            return Rendered(page, "", "", set(), [f"{MANIFEST}: {page.source} does not exist"])
        markdown = path.read_text(encoding="utf-8")
        problems: list[str] = []
        found = headings(markdown)
        if not found or found[0][0] != 1:
            problems.append(f"{page.source}: the first heading must be the page's '# title'")
        if sum(1 for level, _ in found if level == 1) > 1:
            problems.append(f"{page.source}: only the title may be a level-one heading")
        title = found[0][1] if found else ""
        lines = markdown.split("\n")
        for index, line, in_code in prose_lines(markdown):
            if not in_code:
                lines[index] = self.rewrite_line(page, line, problems, index + 1)
        return Rendered(page, title, "\n".join(lines), anchors_of(markdown), problems)


def render_all(root: Path, commit: str | None) -> tuple[Manifest, list[Rendered]]:
    manifest = load_manifest(root)
    linker = Linker(root, manifest, commit)
    return manifest, [linker.render(page) for page in manifest.pages]


def front_matter(values: dict[str, object]) -> str:
    # JSON strings and numbers are valid YAML scalars.
    body = "\n".join(f"{key}: {json.dumps(value, ensure_ascii=False)}" for key, value in values.items())
    return f"---\n{body}\n---\n"


def export(root: Path, out: Path, commit: str | None) -> None:
    manifest, rendered = render_all(root, commit)
    fail_on_problems(rendered)
    if out.exists():
        shutil.rmtree(out)
    (out / "pages").mkdir(parents=True)
    ref = f"commit/{commit}" if commit else "branch/main"
    sections: list[dict[str, object]] = []
    for order, item in enumerate(rendered):
        page = item.page
        entry = {
            "id": page.id,
            "slug": page.slug,
            "url": manifest.url(page),
            "title": item.title,
            "nav": page.nav,
            "description": page.description,
            "source": page.source,
            "sourceUrl": f"{manifest.repository}/src/{ref}/{page.source}",
        }
        if not sections or sections[-1]["title"] != page.section:
            sections.append({"title": page.section, "pages": []})
        sections[-1]["pages"].append(entry)  # type: ignore[union-attr]
        document = front_matter({**entry, "section": page.section, "order": order}) + "\n" + item.markdown
        (out / "pages" / f"{page.id}.md").write_text(document, encoding="utf-8")
    index = {
        "base": manifest.base,
        "commit": commit,
        "repository": manifest.repository,
        "api": {"title": manifest.api_title, "url": manifest.base + manifest.api_path},
        "sections": sections,
    }
    (out / "manifest.json").write_text(json.dumps(index, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def fail_on_problems(rendered: list[Rendered]) -> None:
    problems = [problem for item in rendered for problem in item.problems]
    if problems:
        raise DocsError("\n".join(problems))


def head_commit(root: Path) -> str | None:
    result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=ROOT)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check", help="render every page and fail on a broken page or link")
    export_parser = commands.add_parser("export", help="write the pages and navigation for the site")
    export_parser.add_argument("--out", type=Path, required=True)
    export_parser.add_argument("--commit", help="pin repository links to this commit (default: HEAD)")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    try:
        if args.command == "check":
            manifest, rendered = render_all(root, None)
            fail_on_problems(rendered)
            print(f"developer docs OK ({len(manifest.pages)} pages, links resolved)")
        else:
            export(root, args.out, args.commit or head_commit(root))
            print(f"developer docs exported to {args.out}")
    except DocsError as error:
        print(f"developer docs: {error}", file=sys.stderr)
        print(f"FAIL: fix the pages or {MANIFEST}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
