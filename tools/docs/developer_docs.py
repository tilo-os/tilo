#!/usr/bin/env python3
"""Check and export the documentation books published on tilo.so and tilo.sh.

A book's page list (`docs/developers/pages.toml`, `docs/tilo-sh/pages.toml`)
names the published pages, which stay where they are in the repository; a page
two sites publish is listed in both, from its one file. `check` renders each
page and fails when a page is missing or has no title, or when a link names a
file, directory or heading that does not exist. `export --out DIR` writes what
a site consumes:

    DIR/manifest.json     the navigation: sections, pages, the API reference
    DIR/pages/<id>.md     each page with front matter, links rewritten

A link between listed pages becomes the other page's site address: the page in
this book when it lists the file, else the related book's, with its origin when
that book is published on another site. A link to an API source in
`[api].sources` becomes the API reference. A link to any other file or
directory becomes that path on the repository host, pinned to `--commit` when
one is given. Links inside code are left alone.

A page with a `generator` is written from the code it documents (its `source`)
at build time: `generate --out DIR` runs each generator, and `export
--generated DIR` publishes what they wrote, failing when a page is missing, so
a reference is never copied by hand and cannot fall behind its code. A link to
that source opens the reference.
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


class SourceTree:
    def __init__(self, root: Path, git_dir: Path | None = None, revision: str = "HEAD"):
        self.root = root
        self.git_dir = git_dir
        self.paths: set[str] | None = None
        self.revision: str | None = None
        if git_dir is not None:
            self.revision = self.git("rev-parse", "--verify", f"{revision}^{{commit}}").strip()

    def git(self, *args: str) -> str:
        try:
            return subprocess.check_output(
                ["git", "--git-dir", str(self.git_dir), *args], text=True, stderr=subprocess.PIPE,
            )
        except (OSError, subprocess.CalledProcessError) as error:
            raise DocsError(f"cannot read documentation source from Git: {error}") from error

    def exists(self, source: str) -> bool:
        if self.git_dir is None:
            return (self.root / source).exists()
        if self.paths is None:
            files = self.git("ls-tree", "-r", "--name-only", "-z", self.revision).split("\0")
            files = [name for name in files if name]
            self.paths = {"", *files}
            for name in files:
                self.paths.update(str(parent) for parent in Path(name).parents if str(parent) != ".")
        return source in self.paths

    def read_text(self, source: str) -> str:
        if self.git_dir is not None:
            return self.git("show", f"{self.revision}:{source}")
        path = (self.root / source).resolve()
        if not path.is_relative_to(self.root.resolve()):
            raise DocsError(f"{source}: resolves outside the repository")
        return path.read_text(encoding="utf-8")


@dataclass(frozen=True)
class Page:
    slug: str
    source: str
    section: str
    nav: str
    description: str
    generator: str = ""

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
    related_manifests: tuple[str, ...] = ()
    path: str = MANIFEST
    origin: str = ""
    title: str = ""
    example_languages: tuple[str, ...] = ()

    def url(self, page: Page) -> str:
        return f"{self.base}{page.slug}/" if page.slug else self.base

    def address_from(self, other: "Manifest", path: str) -> str:
        """`path` in this book as a page of `other` links to it: with this
        book's origin when the two are published on different sites."""
        if self.origin and other.origin and self.origin != other.origin:
            return self.origin + path
        return path


@dataclass
class Rendered:
    page: Page
    title: str
    markdown: str
    anchors: set[str]
    problems: list[str] = field(default_factory=list)


def load_manifest(root: Path, manifest_path: str = MANIFEST, tree: SourceTree | None = None) -> Manifest:
    tree = tree or SourceTree(root)
    if Path(manifest_path).is_absolute() or ".." in Path(manifest_path).parts:
        raise DocsError("manifest must be a path inside the repository")
    try:
        data = tomllib.loads(tree.read_text(manifest_path))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise DocsError(f"{manifest_path}: {error}") from error
    base = data.get("base", "")
    if not (base.startswith("/") and base.endswith("/")):
        raise DocsError(f"{manifest_path}: base must start and end with '/'")
    api = data.get("api", {})
    pages = []
    seen_slugs: set[str] = set()
    seen_sources: set[str] = set()
    for entry in data.get("page", []):
        unknown = set(entry) - {"slug", "source", "section", "nav", "description", "generator"}
        if unknown:
            raise DocsError(f"{manifest_path}: unknown page keys {sorted(unknown)}")
        page = Page(
            slug=entry.get("slug", ""),
            source=entry.get("source", ""),
            section=entry.get("section", ""),
            nav=entry.get("nav", ""),
            description=entry.get("description", ""),
            generator=entry.get("generator", ""),
        )
        for path in (page.source, page.generator):
            if Path(path).is_absolute() or ".." in Path(path).parts:
                raise DocsError(f"{manifest_path}: page source and generator must stay inside the repository")
        if page.slug and not SLUG.fullmatch(page.slug):
            raise DocsError(f"{manifest_path}: slug {page.slug!r} is not lowercase kebab case")
        if page.slug in seen_slugs:
            raise DocsError(f"{manifest_path}: slug {page.slug!r} is listed twice")
        if page.source in seen_sources:
            raise DocsError(f"{manifest_path}: {page.source} is listed twice")
        if not (page.source and page.section and page.nav and page.description):
            raise DocsError(f"{manifest_path}: page {page.slug!r} needs source, section, nav and description")
        seen_slugs.add(page.slug)
        seen_sources.add(page.source)
        pages.append(page)
    if not pages or pages[0].slug:
        raise DocsError(f"{manifest_path}: the first page must have slug = \"\" (the section's index)")
    origin = data.get("origin", "")
    if origin and not re.fullmatch(r"https://[a-z0-9.-]+(?::[0-9]+)?", origin):
        raise DocsError(f"{manifest_path}: origin must be https://host, with no path: {origin!r}")
    languages = data.get("examples", {}).get("languages", [])
    if not all(isinstance(language, str) and language for language in languages):
        raise DocsError(f"{manifest_path}: [examples].languages must be a list of language names")
    return Manifest(
        base=base,
        repository=data.get("repository", "").rstrip("/"),
        api_title=api.get("title", "API reference"),
        api_path=api.get("path", ""),
        api_sources=frozenset(api.get("sources", [])),
        pages=tuple(pages),
        related_manifests=tuple(data.get("related_manifests", [])),
        path=manifest_path,
        origin=origin,
        title=data.get("title", ""),
        example_languages=tuple(languages),
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
    def __init__(self, root: Path, manifest: Manifest, commit: str | None, tree: SourceTree | None = None,
                 related_books_on_site: bool = True, generated: dict[str, str] | None = None):
        """`generated` holds the text each generated page's generator wrote,
        by page id; without it a generated page is checked for its generator
        only, and links into it for the page alone."""
        self.root = root
        self.tree = tree or SourceTree(root)
        self.manifest = manifest
        self.generated = generated
        books = [manifest, *(load_manifest(root, path, self.tree) for path in manifest.related_manifests)]
        self.routes: dict[str, str] = {}
        self.api_routes: dict[str, str] = {}
        # A file this book lists is its own page here; a file only a related
        # book lists is that book's page.
        self.generated_sources: dict[str, Page | None] = {}
        for book in books:
            on_site = related_books_on_site or book is manifest
            for page in book.pages:
                if page.generator and page.source not in self.generated_sources:
                    self.generated_sources[page.source] = page if book is manifest else None
                # A related book is always read so links into it can be checked.
                # It gets a site address only when the site publishes it too;
                # otherwise the link points at the repository.
                if on_site and page.source not in self.routes:
                    self.routes[page.source] = book.address_from(manifest, book.url(page))
            if on_site:
                for source in book.api_sources:
                    self.api_routes.setdefault(source, book.address_from(manifest, book.base + book.api_path))
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
        if repo_path in self.routes:
            return self.routes[repo_path] + suffix, self.check_anchor(repo_path, fragment)
        if repo_path.rstrip("/") in self.api_routes:
            return self.api_routes[repo_path.rstrip("/")], None
        if not self.tree.exists(repo_path):
            return target, f"{target}: {repo_path or '.'} does not exist"
        kind = "raw" if image else "src"
        problem = self.check_anchor(repo_path, fragment) if repo_path.endswith(".md") else None
        return f"{self.manifest.repository}/{kind}/{self.ref}/{repo_path}".rstrip("/") + suffix, problem

    def check_anchor(self, source: str, fragment: str) -> str | None:
        if not fragment:
            return None
        if source in self.generated_sources:
            page = self.generated_sources[source]
            text = self.generated.get(page.id) if page is not None and self.generated is not None else None
            if text is None:
                # Its headings are known once its generator ran (export).
                return None
            self.anchors.setdefault(source, anchors_of(text))
        if source not in self.anchors:
            text = self.tree.read_text(source)
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
        if not self.tree.exists(page.source):
            return Rendered(page, "", "", set(), [f"{self.manifest.path}: {page.source} does not exist"])
        if page.generator:
            if not self.tree.exists(page.generator):
                return Rendered(page, "", "", set(), [f"{self.manifest.path}: generator {page.generator} does not exist"])
            if self.generated is None:
                return Rendered(page, page.nav, "", set())
            if page.id not in self.generated:
                return Rendered(page, "", "", set(), [
                    f"{self.manifest.path}: no generated page {page.id}.md: run `developer_docs.py generate`"])
            markdown = self.generated[page.id]
        else:
            markdown = self.tree.read_text(page.source)
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


def render_all(root: Path, commit: str | None, manifest_path: str = MANIFEST,
               tree: SourceTree | None = None,
               related_books_on_site: bool = True,
               generated: dict[str, str] | None = None) -> tuple[Manifest, list[Rendered]]:
    manifest = load_manifest(root, manifest_path, tree)
    linker = Linker(root, manifest, commit, tree, related_books_on_site, generated)
    return manifest, [linker.render(page) for page in manifest.pages]


def read_generated(manifest: Manifest, directories: list[Path]) -> dict[str, str]:
    """The pages generators wrote, by page id, from the first directory that has each."""
    found: dict[str, str] = {}
    for page in manifest.pages:
        if not page.generator:
            continue
        for directory in directories:
            path = directory / f"{page.id}.md"
            if path.is_file():
                found[page.id] = path.read_text(encoding="utf-8")
                break
    return found


def generator_command(root: Path, generator: str) -> list[str]:
    path = str(root / generator)
    if generator.endswith(".py"):
        return [sys.executable, "-I", path]
    if generator.endswith(".ts"):
        return ["bun", path]
    return [path]


def generate(root: Path, manifest_path: str, out: Path, only: list[str]) -> list[str]:
    """Run the generators of the book's pages (or of the pages `only` names)
    into `out/<id>.md`, and answer the ids written."""
    manifest = load_manifest(root, manifest_path)
    pages = [page for page in manifest.pages if page.generator and (not only or page.id in only)]
    unknown = set(only) - {page.id for page in pages}
    if unknown:
        raise DocsError(f"{manifest_path}: no generated pages {sorted(unknown)}")
    out.mkdir(parents=True, exist_ok=True)
    for page in pages:
        command = [*generator_command(root, page.generator), "--root", str(root), "--out", str(out / f"{page.id}.md")]
        result = subprocess.run(command, cwd=root)
        if result.returncode:
            raise DocsError(f"{page.generator} failed for {page.id} (exit {result.returncode})")
    return [page.id for page in pages]


def front_matter(values: dict[str, object]) -> str:
    # JSON strings and numbers are valid YAML scalars.
    body = "\n".join(f"{key}: {json.dumps(value, ensure_ascii=False)}" for key, value in values.items())
    return f"---\n{body}\n---\n"


def export(root: Path, out: Path, commit: str | None, manifest_path: str = MANIFEST,
           tree: SourceTree | None = None, related_books_on_site: bool = True,
           generated_dirs: tuple[Path, ...] = ()) -> None:
    generated = read_generated(load_manifest(root, manifest_path, tree), list(generated_dirs))
    manifest, rendered = render_all(root, commit, manifest_path, tree, related_books_on_site, generated)
    fail_on_problems(rendered)
    destination = out.resolve()
    books = [manifest, *(load_manifest(root, path, tree) for path in manifest.related_manifests)]
    protected = [root / book.path for book in books]
    protected.extend(root / page.source for book in books for page in book.pages)
    repositories = []
    if tree is not None and tree.git_dir is not None:
        repositories.append(tree.git_dir.resolve())
    elif (root / ".git").exists():
        for option in ("--git-dir", "--git-common-dir"):
            result = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "--path-format=absolute", option],
                capture_output=True, text=True,
            )
            if result.returncode:
                raise DocsError("cannot identify Git metadata to protect the export destination")
            repositories.append(Path(result.stdout.strip()).resolve())
    protected.extend(repositories)
    if any(destination == path.resolve() or destination in path.resolve().parents for path in protected) \
            or any(path in destination.parents for path in repositories):
        raise DocsError("export destination contains documentation sources or overlaps Git metadata")
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
        if page.generator:
            entry["generator"] = page.generator
        if not sections or sections[-1]["title"] != page.section:
            sections.append({"title": page.section, "pages": []})
        sections[-1]["pages"].append(entry)  # type: ignore[union-attr]
        document = front_matter({**entry, "section": page.section, "order": order}) + "\n" + item.markdown
        (out / "pages" / f"{page.id}.md").write_text(document, encoding="utf-8")
    index = {
        "title": manifest.title,
        "origin": manifest.origin,
        "examples": {"languages": list(manifest.example_languages)},
        "base": manifest.base,
        "commit": commit,
        "repository": manifest.repository,
        "api": {"title": manifest.api_title, "url": manifest.base + manifest.api_path} if manifest.api_path else None,
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
    parser.add_argument("--manifest", default=MANIFEST, help="repository-relative book manifest")
    parser.add_argument("--git-dir", type=Path, help="read an immutable Git tree instead of a checkout")
    parser.add_argument("--revision", default="HEAD", help="Git source revision (with --git-dir)")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check", help="render every page and fail on a broken page or link")
    generate_parser = commands.add_parser("generate", help="write the generated pages (<id>.md) into a directory")
    generate_parser.add_argument("--out", type=Path, required=True)
    generate_parser.add_argument("--only", action="append", default=[], metavar="ID",
                                 help="only this page (repeatable)")
    export_parser = commands.add_parser("export", help="write the pages and navigation for the site")
    export_parser.add_argument("--out", type=Path, required=True)
    export_parser.add_argument("--commit", help="pin repository links to this commit (default: HEAD)")
    export_parser.add_argument("--generated", type=Path, action="append", default=[],
                               help="a directory of generated pages (<id>.md), from `generate`")
    export_parser.add_argument(
        "--related-books", choices=("site", "repository"), default="site",
        help="address links into a related book on this site (default), or at the repository "
             "when the site publishes only this book",
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()
    try:
        revision = getattr(args, "commit", None) or args.revision
        tree = SourceTree(root, args.git_dir, revision)
        if args.command == "check":
            manifest = load_manifest(root, args.manifest, tree)
            count = 0
            for path in (args.manifest, *manifest.related_manifests):
                book, rendered = render_all(root, tree.revision, path, tree)
                fail_on_problems(rendered)
                count += len(book.pages)
            print(f"developer docs OK ({count} pages, links resolved)")
        elif args.command == "generate":
            written = generate(root, args.manifest, args.out, args.only)
            print(f"generated {len(written)} pages into {args.out}")
        else:
            commit = tree.revision or args.commit or head_commit(root)
            export(root, args.out, commit, args.manifest, tree, args.related_books == "site",
                   tuple(args.generated))
            print(f"developer docs exported to {args.out}")
    except DocsError as error:
        print(f"developer docs: {error}", file=sys.stderr)
        print(f"FAIL: fix the pages or {args.manifest}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
