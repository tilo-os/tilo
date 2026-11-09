"""Lay out the web entry from a desktop bundle.

usage: layout.py BUNDLE_DIR SITE_DIR

The bundle's files keep their names, which say nothing about their content,
so the whole bundle goes under one directory named after its content,
`v/<hash>/`, which never changes once published and is cached as immutable.
Only the pages stay at the top — the two entry pages (`index.html`,
`app/index.html`) and the sign-in page (`sign-in.html`, where a service's
sign-in link lands) — revalidated on every visit and pointing into that
directory, and beside them their home-screen icon (`apple-touch-icon.png`),
where Safari also looks for it unasked; every other file is found relative to
its module (`import.meta.url`), so it follows. The entry configuration
(`entry.json`) is written when the container starts.

A first visit starts at one module, and the browser learns of the modules it
imports only after downloading and parsing it: at sixty-odd modules three and
four levels deep that is a waterfall of round trips before the editor runs.
So each page names every module its start module statically imports, in the
order a browser meets them, as `<link rel="modulepreload">` next to the hints
the page was written with; each is then fetched at once, alongside the start
module. The sign-in page's graph is four modules, the entry page's sixty-odd:
one round trip beyond the page either way.
"""

import hashlib
import json
import os
import pathlib
import re
import shutil
import sys

# Text in a string is not code, and neither is a comment: either can read like
# an import. What a string keeps is a module's name, where one names a module
# (`code_of`). A quote stands for itself alone as a one-character class, which
# needs no escape in a raw pattern, and an escaped quote would.
STRING = re.compile(
    r'''(?<![\\\w])['][^'\n]*[']'''
    + r'''|(?<![\\\w])`[^`]*`|//[^\n]*|/\*.*?\*/''',
    re.S,
)
# A static import of a module for its side effects (`import './a.mjs';`). A
# dynamic import runs from code, after a `=` or a `(`, and is not one.
SIDE_EFFECT = re.compile(r'''(?<![=(,])\bimport\s+['"]([^'"]+)['"]''')
# An import or re-export clause, up to the `from` its specifier follows: a
# default or named binding (`import a from`, `import a, {b} from`), a bare
# `import from`, or a re-export (`export * from`, `export {a} from`). Names and
# braces have no `from` of their own, and a list of names may break across
# lines.
CLAUSE = re.compile(
    r'''(?:\bimport\s*(?:\{[^}]*\}|[\w$*]+)?|\bexport\s*(?:\*|\{[^}]*\}))\s*from\b''',
    re.S,
)
# The specifier a clause is followed by.
SPECIFIER = re.compile(r'''\s*['"]([^'"]+)['"]''')


def code_of(path):
    """`path`'s source, with its comments and its strings' text blanked.

    One string keeps its text: the one an import names a module with.
    """
    def text_of(match):
        kept = after.match(match.string, match.end())
        quote = match.group(0)
        return quote if kept else quote[0] + ' ' * (len(quote) - 2) + quote[-1]

    # What a specifier's string is followed by, and no other string is.
    after = re.compile(r'''\s*[;),]''')
    return STRING.sub(text_of, path.read_text())


def static_imports(module):
    """The modules `module` statically imports, in the order they are written.

    Dynamic imports are not: each runs when some path reaches it, and what a
    session may never reach is bytes on every first load if it is preloaded.
    """
    code = code_of(module)
    entries = [(m.start(), m.group(1)) for m in SIDE_EFFECT.finditer(code)]
    for clause in CLAUSE.finditer(code):
        specifier = SPECIFIER.match(code, clause.end())
        if specifier:
            entries.append((clause.start(), specifier.group(1)))
    found = []
    for _, specifier in sorted(entries):
        if not specifier.startswith(('.', '/')):
            continue
        target = pathlib.Path(os.path.normpath(str(module.parent / specifier)))
        if all(str(target) != str(other) for other in found):
            found.append(target)
    return found


def graph(entry):
    """Every module `entry` statically reaches, breadth first.

    That is the order a browser meets them too: the entry module's own imports
    before theirs, each level in the order they are written, so a page that
    names them all starts their fetches at once instead of a level per round
    trip.
    """
    reached = []
    frontier = [entry]
    while frontier:
        reached.extend(frontier)
        following = []
        for module in frontier:
            for target in static_imports(module):
                if target not in reached and target not in following:
                    following.append(target)
        frontier = following
    return reached


bundle, site = map(pathlib.Path, sys.argv[1:])
if site.exists() and any(site.iterdir()):
    sys.exit(f'{site} must be empty')
build = json.loads((bundle / 'build.json').read_text())
files = build['files']
# The version is the bundle's own file hashes (build.json lists every file).
version = hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()[:20]
assets = site / 'v' / version
shutil.copytree(bundle, assets)

# Each page starts one module, names it by a path of its own, and preloads it
# (so a page laid out without this step still starts at once). A bundle built
# for a site that serves no sign-in page — an app's own — carries none, and is
# laid out without one.
pages = {'index.html': './entry-page.mjs', 'app/index.html': '../entry-page.mjs',
         'sign-in.html': './sign-in.mjs'}
pages = {page: entry for page, entry in pages.items() if (assets / page).is_file()}
graphs = {}
for entry in dict.fromkeys(pages.values()):
    start = pathlib.PurePosixPath(entry).name
    # The bundle is the bundle: a module a page's start module imports and it
    # lacks is a broken build, not a page to serve.
    modules = graph(assets / start)
    missing = [module for module in modules if not module.is_file()]
    missing += [target for module in modules for target in static_imports(module)
                if not target.is_file()]
    if missing:
        sys.exit(f'{missing[0].relative_to(assets)} is imported but not in the bundle')
    graphs[entry] = [module.relative_to(assets).as_posix() for module in modules]

for page, entry in pages.items():
    start = pathlib.PurePosixPath(entry).name
    names = graphs[entry]
    prefix = '../' if '/' in page else './'
    text = (assets / page).read_text()
    statement = f"from '{entry}';"
    if text.count(statement) != 1:
        sys.exit(f'{page}: expected one import of {entry}')
    text = text.replace(statement, f"from '{prefix}v/{version}/{start}';")
    for rel in ('preload', 'modulepreload'):
        hint = f'<link rel="{rel}" href="{prefix}'
        text = text.replace(hint, f'{hint}v/{version}/')
    # A module the page loads by a script of its own (the first screen's,
    # which works before the entry module is in) is in the bundle too, and
    # moves with it: left at the page's path it would be a request for nothing.
    def versioned(match):
        name = match.group(2)
        if not (assets / name).is_file():
            sys.exit(f'{page}: loads {name}, which is not in the bundle')
        return f'{match.group(1)}{prefix}v/{version}/{name}"'
    text = re.sub(r'(<script\b[^>]*\bsrc=")' + re.escape(prefix) + r'(?!v/)([^"]+\.m?js)"', versioned, text)
    # The start module the page preloads itself leads the graph's hints; every
    # other module it reaches gets one here.
    anchor = f'<link rel="modulepreload" href="{prefix}v/{version}/{start}">'
    if text.count(anchor) != 1:
        sys.exit(f'{page}: expected one modulepreload of {start}')
    for name in names:
        if name == start:
            continue
        text = re.sub(r'\n[ \t]*' + re.escape(f'<link rel="modulepreload" href="{prefix}v/{version}/{name}">'),
                      '', text)
    hints = ''.join(f'\n  <link rel="modulepreload" href="{prefix}v/{version}/{name}">'
                    for name in names)
    text = text.replace(anchor, hints)
    (site / page).parent.mkdir(parents=True, exist_ok=True)
    (site / page).write_text(text)
    # Only the pages at the top are entries: a copy under v/ would be served
    # with the entry.json of its own directory, which is not the home one.
    (assets / page).unlink()
if (assets / 'app').is_dir():
    (assets / 'app').rmdir()
(assets / 'apple-touch-icon.png').rename(site / 'apple-touch-icon.png')
(site / 'version.json').write_text(json.dumps({'version': version, 'revision': build['revision']}) + '\n')
print(version)
