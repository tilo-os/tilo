#!/usr/bin/env python3
"""Fetch the catalog's reading faces from their pinned upstream releases.

The Reading role's default face, New Computer Modern, and the faces a reader
can choose instead (Libertinus Serif, Literata, EB Garamond, Source Serif 4)
ship unmodified from their official releases (see
crates/sdk/ui-core/assets/fonts/PROVENANCE.md). Every archive and file is
checked against its pinned SHA-256 before anything is written.

    python3 tools/font-reading-faces.py          # write candidates outside source
    python3 tools/font-reading-faces.py --check  # fetch and compare

Python 3.11+, no third-party modules.
"""

import argparse
import importlib.util
import hashlib
import io
import sys
import tarfile
import tomllib
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "crates/sdk/ui-core/assets/fonts"

NEWCM = "https://download.gnu.org.ua/release/newcm/newcm-8.1.1.txz"
# The CTAN package of the same release carries the licence statement the
# author's archive leaves out. CTAN serves only its current release: when it
# moves on, this hash fails and the pin needs a deliberate update.
NEWCM_CTAN = "https://mirrors.ctan.org/fonts/newcomputermodern.zip"
LIBERTINUS = "https://github.com/alerque/libertinus/releases/download/v7.051/Libertinus-7.051.zip"
LITERATA = "https://github.com/googlefonts/literata/releases/download/3.103/3.103.zip"
SOURCE_SERIF = "https://github.com/adobe-fonts/source-serif/releases/download/4.005R/source-serif-4.005_Desktop.zip"
GOOGLE_FONTS = "https://raw.githubusercontent.com/google/fonts/9710da1eacb3be272583c3224dcb70f9da6eadbb"
GUST = "https://tug.org/fonts/licenses/GUST-FONT-LICENSE.txt"
LPPL = "https://www.latex-project.org/lppl/lppl-1-3c.txt"

ARCHIVES = {
    NEWCM: "1c10a9e2be6b0978a5d6e851094de72727237c1794ac200bc63604eb49adcba0",
    NEWCM_CTAN: "9cc2e55835ab9adac227ccf1b8ed68bc0f73845489ea3533b88a1f56bd05d79a",
    LIBERTINUS: "4d9be29b5cb380c35af8ba967abcc752ad1e07be1f738a9789c33e0dd7478c92",
    LITERATA: "f7fb973cafb26cf785cbebaeaf51c18f87c15a3bcf4d82a7d4857564db5b056d",
    SOURCE_SERIF: "549fdb8f9a682bd06944298621404969f6de77c2e422ff3b8244a1dcd6a0c425",
}

# The corresponding source of New Computer Modern's cuts (its GPL-licensed
# Regular cut needs one recorded): the FontForge sources in the CTAN package.
# Checked, not written into the checkout.
SOURCES = [
    (NEWCM_CTAN, "newcomputermodern/sfd.txz", "73f140c8fa850b1dc460518227332c71bb69ed0a60ede072860d60d85be49b68"),
]

# (catalog file, source, member of the archive or None for a plain file).
# Each result is checked against the SHA-256 the catalog manifest pins.
FILES = [
    ("NewCM10-Book.otf", NEWCM, "newcm-8.1.1/otf/NewCM10-Book.otf"),
    ("NewCM10-BookItalic.otf", NEWCM, "newcm-8.1.1/otf/NewCM10-BookItalic.otf"),
    ("NewCM10-Regular.otf", NEWCM, "newcm-8.1.1/otf/NewCM10-Regular.otf"),
    ("NewCM10-Italic.otf", NEWCM, "newcm-8.1.1/otf/NewCM10-Italic.otf"),
    ("NewCM10-Bold.otf", NEWCM, "newcm-8.1.1/otf/NewCM10-Bold.otf"),
    ("NewCM10-BoldItalic.otf", NEWCM, "newcm-8.1.1/otf/NewCM10-BoldItalic.otf"),
    ("NewCM08-Book.otf", NEWCM, "newcm-8.1.1/otf/NewCM08-Book.otf"),
    ("LICENSES/New-Computer-Modern-License.txt", NEWCM_CTAN, "newcomputermodern/License.txt"),
    ("LICENSES/GUST-FONT-LICENSE.txt", GUST, None),
    ("LICENSES/LPPL-1.3c.txt", LPPL, None),
    ("LibertinusSerif-Regular.otf", LIBERTINUS, "Libertinus-7.051/static/OTF/LibertinusSerif-Regular.otf"),
    ("LibertinusSerif-Italic.otf", LIBERTINUS, "Libertinus-7.051/static/OTF/LibertinusSerif-Italic.otf"),
    ("LibertinusSerif-Bold.otf", LIBERTINUS, "Libertinus-7.051/static/OTF/LibertinusSerif-Bold.otf"),
    ("LibertinusSerif-BoldItalic.otf", LIBERTINUS, "Libertinus-7.051/static/OTF/LibertinusSerif-BoldItalic.otf"),
    ("LibertinusSerifDisplay-Regular.otf", LIBERTINUS, "Libertinus-7.051/static/OTF/LibertinusSerifDisplay-Regular.otf"),
    ("LICENSES/Libertinus-OFL.txt", LIBERTINUS, "Libertinus-7.051/OFL.txt"),
    ("Literata[opsz,wght].ttf", LITERATA, "fonts/variable/Literata[opsz,wght].ttf"),
    ("Literata-Italic[opsz,wght].ttf", LITERATA, "fonts/variable/Literata-Italic[opsz,wght].ttf"),
    ("LICENSES/Literata-OFL.txt", LITERATA, "OFL.txt"),
    ("EBGaramond[wght].ttf", f"{GOOGLE_FONTS}/ofl/ebgaramond/EBGaramond%5Bwght%5D.ttf", None),
    ("EBGaramond-Italic[wght].ttf", f"{GOOGLE_FONTS}/ofl/ebgaramond/EBGaramond-Italic%5Bwght%5D.ttf", None),
    ("LICENSES/EB-Garamond-OFL.txt", f"{GOOGLE_FONTS}/ofl/ebgaramond/OFL.txt", None),
    ("SourceSerif4Variable-Roman.ttf", SOURCE_SERIF, "source-serif-4.005_Desktop/VAR/SourceSerif4Variable-Roman.ttf"),
    ("SourceSerif4Variable-Italic.ttf", SOURCE_SERIF, "source-serif-4.005_Desktop/VAR/SourceSerif4Variable-Italic.ttf"),
    ("LICENSES/Source-Serif-OFL.txt", SOURCE_SERIF, "source-serif-4.005_Desktop/LICENSE.md"),
]


def fetch(url, sha256=None):
    with urllib.request.urlopen(url) as response:
        data = response.read()
    actual = hashlib.sha256(data).hexdigest()
    if sha256 is not None and actual != sha256:
        raise SystemExit(f"{url}: SHA-256 {actual} is not the pinned {sha256}")
    return data


def member(archive, url, name):
    if url.endswith(".txz"):
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:xz") as tar:
            extracted = tar.extractfile(name)
            if extracted is None:
                raise SystemExit(f"{url}: {name} is not a file")
            return extracted.read()
    with zipfile.ZipFile(io.BytesIO(archive)) as zipped:
        return zipped.read(name)


def pinned():
    """SHA-256 of every catalog file, from the manifest."""
    manifest = tomllib.loads((CATALOG / "manifest.toml").read_text())
    return {entry["file"]: entry["sha256"] for entry in manifest["font"] + manifest["license"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail if the checkout differs")
    parser.add_argument("--out", type=Path, default=ROOT / "target/font-candidates",
                        help="write regenerated binary candidates outside source")
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location("font_catalog", ROOT / "tools/font-catalog.py")
    catalog_module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(catalog_module)
    reviewed = catalog_module.prepared_root(CATALOG, "fixtures") if args.check else args.out
    reviewed.mkdir(parents=True, exist_ok=True)
    pins = pinned()
    archives = {}
    stale = []
    for output, url, name in FILES:
        sha256 = pins.get(output)
        if name is None:
            data = fetch(url)
        else:
            if url not in archives:
                archives[url] = fetch(url, ARCHIVES[url])
            data = member(archives[url], url, name)
        digest = hashlib.sha256(data).hexdigest()
        if digest != sha256:
            raise SystemExit(f"{output}: SHA-256 {digest} is not the manifest's {sha256}")
        print(f"{output}: {len(data)} bytes, sha256 {digest}")
        target = reviewed / output
        if args.check:
            if not target.is_file() or target.read_bytes() != data:
                stale.append(output)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
    for url, name, sha256 in SOURCES:
        if url not in archives:
            archives[url] = fetch(url, ARCHIVES[url])
        digest = hashlib.sha256(member(archives[url], url, name)).hexdigest()
        if digest != sha256:
            raise SystemExit(f"{url}: {name} SHA-256 {digest} is not the pinned {sha256}")
        print(f"{name} (source): sha256 {digest}")
    if stale:
        sys.exit(f"out of date: {', '.join(stale)}")


if __name__ == "__main__":
    main()
