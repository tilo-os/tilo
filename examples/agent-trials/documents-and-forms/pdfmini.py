"""Just enough PDF to write the trial's documents and to check what an agent made.

Standard library only, so it runs on the hosted desktop's python3 with nothing
installed. The writer emits uncompressed objects and Flate streams with
base-14 fonts and AcroForm fields. The reader parses what PDF tools commonly
write: classic and incremental files, object streams, Flate content, literal
and hex strings, simple fonts and ToUnicode maps. It is not a general PDF
library: encrypted files, LZW/ASCII85 filters and composite fonts without a
ToUnicode map are refused with a reason rather than read wrongly.
"""

from __future__ import annotations

import hashlib
import re
import zlib
from dataclasses import dataclass, field
from typing import Any

# ----------------------------------------------------------------------------
# Objects
# ----------------------------------------------------------------------------


class Name(str):
    """A PDF name, without its leading slash."""


@dataclass(frozen=True)
class Ref:
    num: int
    gen: int = 0


@dataclass
class Stream:
    dict: dict
    raw: bytes

    def data(self) -> bytes:
        filters = self.dict.get("Filter")
        if filters is None:
            return self.raw
        if not isinstance(filters, list):
            filters = [filters]
        data = self.raw
        for name in filters:
            if name in ("FlateDecode", "Fl"):
                data = zlib.decompress(data)
            else:
                raise PdfError(f"stream filter /{name} is not supported")
        return data


class PdfError(Exception):
    """The file is not a PDF this module can read."""


class Keyword(str):
    """A bare word in a content stream: an operator, true, false, null."""


# ----------------------------------------------------------------------------
# Writing
# ----------------------------------------------------------------------------


def pdf_string(text: str) -> bytes:
    """A literal string: WinAnsi when it fits, UTF-16BE with a BOM otherwise."""
    try:
        raw = text.encode("cp1252")
    except UnicodeEncodeError:
        return b"<" + (b"\xfe\xff" + text.encode("utf-16-be")).hex().upper().encode() + b">"
    out = raw.replace(b"\\", b"\\\\").replace(b"(", b"\\(").replace(b")", b"\\)")
    out = out.replace(b"\r", b"\\r").replace(b"\n", b"\\n")
    return b"(" + out + b")"


def serialize(obj: Any) -> bytes:
    if isinstance(obj, bool):
        return b"true" if obj else b"false"
    if obj is None:
        return b"null"
    if isinstance(obj, Name):
        return b"/" + obj.encode("latin-1")
    if isinstance(obj, Ref):
        return f"{obj.num} {obj.gen} R".encode()
    if isinstance(obj, int):
        return str(obj).encode()
    if isinstance(obj, float):
        return (f"{obj:.4f}".rstrip("0").rstrip(".") or "0").encode()
    if isinstance(obj, bytes):
        return b"<" + obj.hex().encode() + b">"
    if isinstance(obj, str):
        return pdf_string(obj)
    if isinstance(obj, list):
        return b"[" + b" ".join(serialize(item) for item in obj) + b"]"
    if isinstance(obj, dict):
        parts = [b"/" + key.encode("latin-1") + b" " + serialize(value) for key, value in obj.items()]
        return b"<<" + b" ".join(parts) + b">>"
    if isinstance(obj, Stream):
        head = dict(obj.dict)
        head["Length"] = len(obj.raw)
        return serialize(head) + b"\nstream\n" + obj.raw + b"\nendstream"
    raise TypeError(f"cannot write {type(obj).__name__} into a PDF")


class Writer:
    """Objects numbered from 1, written as one classic file."""

    def __init__(self) -> None:
        self.bodies: list[bytes | None] = []

    def reserve(self) -> Ref:
        self.bodies.append(None)
        return Ref(len(self.bodies))

    def put(self, ref: Ref, obj: Any) -> Ref:
        self.bodies[ref.num - 1] = serialize(obj)
        return ref

    def add(self, obj: Any) -> Ref:
        return self.put(self.reserve(), obj)

    def stream(self, data: bytes, entries: dict | None = None, compress: bool = True) -> Ref:
        head = dict(entries or {})
        if compress:
            head["Filter"] = Name("FlateDecode")
            data = zlib.compress(data, 9)
        return self.add(Stream(head, data))

    def finish(self, root: Ref, info: dict | None = None) -> bytes:
        out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
        offsets = []
        for num, body in enumerate(self.bodies, start=1):
            if body is None:
                raise ValueError(f"object {num} was reserved but never written")
            offsets.append(len(out))
            out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
        trailer: dict = {"Size": len(self.bodies) + 1, "Root": root}
        if info is not None:
            trailer["Info"] = self.add_late(out, offsets, info)
        digest = hashlib.sha256(bytes(out)).digest()[:16]
        trailer["Size"] = len(offsets) + 1
        trailer["ID"] = [digest, digest]
        xref = len(out)
        out += f"xref\n0 {len(offsets) + 1}\n".encode()
        out += b"0000000000 65535 f \n"
        for offset in offsets:
            out += f"{offset:010d} 00000 n \n".encode()
        out += b"trailer\n" + serialize(trailer) + f"\nstartxref\n{xref}\n%%EOF\n".encode()
        return bytes(out)

    @staticmethod
    def add_late(out: bytearray, offsets: list[int], obj: Any) -> Ref:
        num = len(offsets) + 1
        offsets.append(len(out))
        out += f"{num} 0 obj\n".encode() + serialize(obj) + b"\nendobj\n"
        return Ref(num)


def incremental_update(original: bytes, objects: dict[int, Any]) -> bytes:
    """Append new versions of `objects` as an incremental update, as an
    editor saving a filled form does."""
    doc = Document(original)
    out = bytearray(original)
    if not out.endswith(b"\n"):
        out += b"\n"
    offsets = {}
    for num, obj in sorted(objects.items()):
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode() + serialize(obj) + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n"
    for num, offset in sorted(offsets.items()):
        out += f"{num} 1\n{offset:010d} 00000 n \n".encode()
    size = max([doc.max_num, *objects]) + 1
    trailer = {"Size": size, "Root": doc.root_ref, "Prev": doc.startxref}
    out += b"trailer\n" + serialize(trailer) + f"\nstartxref\n{xref}\n%%EOF\n".encode()
    return bytes(out)


# Base-14 fonts the writer uses, with the Helvetica widths it lays out with
# (1000-unit em, ASCII 32..126; other characters take the average).
HELVETICA_WIDTHS = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556,
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
]


def text_width(text: str, size: float) -> float:
    total = 0
    for ch in text:
        code = ord(ch)
        total += HELVETICA_WIDTHS[code - 32] if 32 <= code <= 126 else 556
    return total * size / 1000


def wrap(text: str, size: float, width: float) -> list[str]:
    lines: list[str] = []
    for paragraph in text.split("\n"):
        line = ""
        for word in paragraph.split(" "):
            candidate = f"{line} {word}" if line else word
            if line and text_width(candidate, size) > width:
                lines.append(line)
                line = word
            else:
                line = candidate
        lines.append(line)
    return lines


@dataclass
class TextPage:
    """One page of positioned text and rules, in points from the bottom left."""

    width: float = 595.0
    height: float = 842.0
    ops: list[bytes] = field(default_factory=list)
    rotate: int = 0
    y: float = 790.0

    def text(self, x: float, y: float, text: str, size: float = 11, bold: bool = False) -> None:
        font = b"/F2" if bold else b"/F1"
        self.ops.append(
            b"BT " + font + f" {size:g} Tf {x:g} {y:g} Td ".encode() + pdf_string(text) + b" Tj ET"
        )

    def rule(self, x: float, y: float, width: float, height: float = 0.8, grey: float = 0.0) -> None:
        self.ops.append(f"{grey:g} g {x:g} {y:g} {width:g} {height:g} re f 0 g".encode())

    def box(self, x: float, y: float, width: float, height: float) -> None:
        self.ops.append(f"0.5 w {x:g} {y:g} {width:g} {height:g} re S".encode())

    # Flowing helpers, top to bottom from `self.y`.
    def line(self, text: str, size: float = 11, bold: bool = False, x: float = 60, gap: float = 1.45) -> None:
        self.text(x, self.y, text, size, bold)
        self.y -= size * gap

    def para(self, text: str, size: float = 11, x: float = 60, width: float = 475) -> None:
        for line in wrap(text, size, width):
            self.line(line, size, x=x)
        self.y -= size * 0.6

    def space(self, points: float) -> None:
        self.y -= points

    def content(self) -> bytes:
        return b"\n".join(self.ops) + b"\n"


@dataclass
class Widget:
    """An AcroForm field with one widget on a page."""

    name: str
    label: str
    rect: tuple[float, float, float, float]
    checkbox: bool = False


def write_document(
    pages: list[TextPage],
    title: str,
    widgets: dict[int, list[Widget]] | None = None,
    images: dict[int, tuple[int, int, bytes]] | None = None,
) -> bytes:
    """A PDF of `pages`; `widgets` and `images` are keyed by page index.

    An image is (width, height, RGB bytes), drawn over the whole page."""
    widgets = widgets or {}
    images = images or {}
    w = Writer()
    catalog = w.reserve()
    pages_ref = w.reserve()
    helv = w.add({"Type": Name("Font"), "Subtype": Name("Type1"), "BaseFont": Name("Helvetica"),
                  "Encoding": Name("WinAnsiEncoding")})
    helv_bold = w.add({"Type": Name("Font"), "Subtype": Name("Type1"), "BaseFont": Name("Helvetica-Bold"),
                       "Encoding": Name("WinAnsiEncoding")})
    zadb = w.add({"Type": Name("Font"), "Subtype": Name("Type1"), "BaseFont": Name("ZapfDingbats")})
    fonts = {"F1": helv, "F2": helv_bold, "Helv": helv, "ZaDb": zadb}
    kids: list[Ref] = []
    fields: list[Ref] = []
    check_on = check_off = None
    for index, page in enumerate(pages):
        page_ref = w.reserve()
        resources: dict = {"Font": dict(fonts)}
        content = page.content()
        if index in images:
            iw, ih, rgb = images[index]
            image = w.stream(rgb, {"Type": Name("XObject"), "Subtype": Name("Image"), "Width": iw,
                                   "Height": ih, "ColorSpace": Name("DeviceRGB"), "BitsPerComponent": 8})
            resources = {"XObject": {"Im0": image}}
            content = f"q {page.width:g} 0 0 {page.height:g} 0 0 cm /Im0 Do Q\n".encode()
        annots = []
        for widget in widgets.get(index, []):
            entry: dict = {
                "Type": Name("Annot"), "Subtype": Name("Widget"), "T": widget.name, "TU": widget.label,
                "Rect": list(widget.rect), "P": page_ref, "F": 4,
                "MK": {"BC": [0.2, 0.2, 0.2], "BG": [1, 1, 1]},
            }
            if widget.checkbox:
                if check_on is None:
                    x0, y0, x1, y1 = widget.rect
                    side = min(x1 - x0, y1 - y0)
                    check_on = w.stream(
                        f"q 0 g BT /ZaDb {side * 0.8:g} Tf {side * 0.12:g} {side * 0.2:g} Td (4) Tj ET Q".encode(),
                        {"Type": Name("XObject"), "Subtype": Name("Form"), "BBox": [0, 0, side, side],
                         "Resources": {"Font": {"ZaDb": zadb}}}, compress=False)
                    check_off = w.stream(b"", {"Type": Name("XObject"), "Subtype": Name("Form"),
                                               "BBox": [0, 0, side, side]}, compress=False)
                entry.update({"FT": Name("Btn"), "V": Name("Off"), "AS": Name("Off"),
                              "DA": "/ZaDb 0 Tf 0 g",
                              "AP": {"N": {"Yes": check_on, "Off": check_off}}})
                entry["MK"]["CA"] = "4"
            else:
                entry.update({"FT": Name("Tx"), "V": "", "DA": "/Helv 10 Tf 0 g"})
            ref = w.add(entry)
            annots.append(ref)
            fields.append(ref)
        page_dict: dict = {
            "Type": Name("Page"), "Parent": pages_ref, "MediaBox": [0, 0, page.width, page.height],
            "Resources": resources, "Contents": w.stream(content),
        }
        if page.rotate:
            page_dict["Rotate"] = page.rotate
        if annots:
            page_dict["Annots"] = annots
        w.put(page_ref, page_dict)
        kids.append(page_ref)
    w.put(pages_ref, {"Type": Name("Pages"), "Kids": kids, "Count": len(kids)})
    root: dict = {"Type": Name("Catalog"), "Pages": pages_ref}
    if fields:
        root["AcroForm"] = {"Fields": fields, "NeedAppearances": True, "DA": "/Helv 0 Tf 0 g",
                            "DR": {"Font": {"Helv": helv, "ZaDb": zadb}}}
    w.put(catalog, root)
    return w.finish(catalog, {"Title": title, "Producer": "Tilo agent trial pack"})


# ----------------------------------------------------------------------------
# Reading
# ----------------------------------------------------------------------------

WHITESPACE = b"\x00\t\n\x0c\r "
DELIMITERS = b"()<>[]{}/%"
OBJ_HEADER = re.compile(rb"(\d+)\s+(\d+)\s+obj\b")


class Lexer:
    def __init__(self, data: bytes, pos: int = 0) -> None:
        self.data = data
        self.pos = pos

    def skip_space(self) -> None:
        data, n = self.data, len(self.data)
        while self.pos < n:
            c = data[self.pos]
            if c in WHITESPACE:
                self.pos += 1
            elif c == 0x25:  # %
                while self.pos < n and data[self.pos] not in b"\r\n":
                    self.pos += 1
            else:
                return

    def at_end(self) -> bool:
        self.skip_space()
        return self.pos >= len(self.data)

    def word(self) -> bytes:
        start = self.pos
        data, n = self.data, len(self.data)
        while self.pos < n and data[self.pos] not in WHITESPACE and data[self.pos] not in DELIMITERS:
            self.pos += 1
        return data[start:self.pos]

    def literal(self) -> bytes:
        # At "(".
        self.pos += 1
        depth, out = 1, bytearray()
        data, n = self.data, len(self.data)
        escapes = {ord("n"): b"\n", ord("r"): b"\r", ord("t"): b"\t", ord("b"): b"\b", ord("f"): b"\f",
                   ord("("): b"(", ord(")"): b")", ord("\\"): b"\\"}
        while self.pos < n:
            c = data[self.pos]
            self.pos += 1
            if c == 0x5C:  # backslash
                if self.pos >= n:
                    break
                e = data[self.pos]
                self.pos += 1
                if e in escapes:
                    out += escapes[e]
                elif 0x30 <= e <= 0x37:
                    digits = bytes([e])
                    while len(digits) < 3 and self.pos < n and 0x30 <= data[self.pos] <= 0x37:
                        digits += bytes([data[self.pos]])
                        self.pos += 1
                    out.append(int(digits, 8) & 0xFF)
                elif e == 0x0D:
                    if self.pos < n and data[self.pos] == 0x0A:
                        self.pos += 1
                elif e == 0x0A:
                    pass
                else:
                    out.append(e)
            elif c == 0x28:
                depth += 1
                out.append(c)
            elif c == 0x29:
                depth -= 1
                if depth == 0:
                    return bytes(out)
                out.append(c)
            else:
                out.append(c)
        raise PdfError("a string never ends")

    def hexstring(self) -> bytes:
        end = self.data.index(b">", self.pos)
        digits = re.sub(rb"[^0-9A-Fa-f]", b"", self.data[self.pos + 1:end])
        self.pos = end + 1
        if len(digits) % 2:
            digits += b"0"
        return bytes.fromhex(digits.decode())

    def name(self) -> Name:
        self.pos += 1
        raw = self.word()
        decoded = re.sub(rb"#([0-9A-Fa-f]{2})", lambda m: bytes([int(m.group(1), 16)]), raw)
        return Name(decoded.decode("latin-1"))

    def next(self) -> Any:
        """The next token: an object, a delimiter marker or a Keyword."""
        self.skip_space()
        if self.pos >= len(self.data):
            raise PdfError("the file ends inside an object")
        data = self.data
        c = data[self.pos]
        if c == 0x2F:
            return self.name()
        if c == 0x28:
            return LitString(self.literal())
        if c == 0x3C:
            if data[self.pos + 1:self.pos + 2] == b"<":
                self.pos += 2
                return "<<"
            return HexString(self.hexstring())
        if c == 0x3E and data[self.pos + 1:self.pos + 2] == b">":
            self.pos += 2
            return ">>"
        if c in b"[]{}":
            self.pos += 1
            return chr(c)
        word = self.word()
        if not word:
            self.pos += 1
            return Keyword(chr(c))
        try:
            if re.fullmatch(rb"[+-]?\d+", word):
                return int(word)
            if re.fullmatch(rb"[+-]?(\d+\.\d*|\.\d+|\d+\.)", word):
                return float(word)
        except ValueError:
            pass
        return Keyword(word.decode("latin-1"))

    def parse_object(self, tok: Any = None) -> Any:
        tok = self.next() if tok is None else tok
        if tok == "<<":
            out: dict = {}
            while True:
                key = self.next()
                if key == ">>":
                    return out
                if not isinstance(key, Name):
                    raise PdfError("a dictionary key is not a name")
                out[str(key)] = self.parse_object()
        if tok == "[":
            items: list = []
            while True:
                t = self.next()
                if t == "]":
                    return items
                items.append(self.parse_object(t))
        if isinstance(tok, int) and not isinstance(tok, bool):
            save = self.pos
            try:
                gen = self.next()
                if isinstance(gen, int) and not isinstance(gen, bool):
                    r = self.next()
                    if r == Keyword("R"):
                        return Ref(tok, gen)
            except PdfError:
                pass
            self.pos = save
            return tok
        if isinstance(tok, Keyword):
            if tok == "true":
                return True
            if tok == "false":
                return False
            if tok == "null":
                return None
        return tok


class LitString(bytes):
    pass


class HexString(bytes):
    pass


def text_string(value: Any) -> str:
    """A PDF text string (or name) as Python text."""
    if isinstance(value, Name):
        return str(value)
    if isinstance(value, bytes):
        if value.startswith(b"\xfe\xff"):
            return value[2:].decode("utf-16-be", errors="replace")
        if value.startswith(b"\xef\xbb\xbf"):
            return value[3:].decode("utf-8", errors="replace")
        return value.decode("latin-1")
    if value is None:
        return ""
    return str(value)


class Document:
    def __init__(self, data: bytes) -> None:
        if not data.lstrip()[:5] == b"%PDF-":
            raise PdfError("the file does not start with %PDF-")
        self.data = data
        self.objects: dict[int, Any] = {}
        self.trailers: list[dict] = []
        self._scan()
        if any("Encrypt" in t for t in self.trailers):
            raise PdfError("the PDF is encrypted")
        self.root_ref = self._find_root()
        self.max_num = max(self.objects) if self.objects else 0
        found = data.rfind(b"startxref")
        self.startxref = int(data[found + 9:].split()[0]) if found >= 0 else 0

    def _scan(self) -> None:
        data = self.data
        pos = 0
        streams: list[tuple[int, Stream]] = []
        # The next "trailer" keyword at or after `pos`, found again only
        # once the scan passes it; len(data) when there is none.
        trailer_at = -1
        while True:
            match = OBJ_HEADER.search(data, pos)
            if trailer_at < pos:
                found = data.find(b"trailer", pos)
                trailer_at = len(data) if found < 0 else found
            if trailer_at < len(data) and (match is None or trailer_at < match.start()):
                lex = Lexer(data, trailer_at + 7)
                try:
                    trailer = lex.parse_object()
                    if isinstance(trailer, dict):
                        self.trailers.append(trailer)
                    pos = lex.pos
                except PdfError:
                    pos = trailer_at + 7
                continue
            if match is None:
                break
            num = int(match.group(1))
            lex = Lexer(data, match.end())
            try:
                obj = lex.parse_object()
            except (PdfError, ValueError, IndexError):
                pos = match.end()
                continue
            lex.skip_space()
            if isinstance(obj, dict) and data.startswith(b"stream", lex.pos):
                start = lex.pos + 6
                if data[start:start + 2] == b"\r\n":
                    start += 2
                elif data[start:start + 1] in (b"\n", b"\r"):
                    start += 1
                length = obj.get("Length")
                end = -1
                if isinstance(length, int) and data.startswith(b"endstream", self._skip_ws(start + length)):
                    end = start + length
                if end < 0:
                    end = data.find(b"endstream", start)
                    if end < 0:
                        raise PdfError(f"object {num} has a stream that never ends")
                    raw = data[start:end].rstrip(b"\r\n")
                    end_after = end + 9
                else:
                    raw = data[start:end]
                    end_after = data.find(b"endstream", end) + 9
                stream = Stream(obj, raw)
                self.objects[num] = stream
                if obj.get("Type") == "XRef":
                    self.trailers.append(obj)
                if obj.get("Type") == "ObjStm":
                    streams.append((num, stream))
                pos = end_after
            else:
                self.objects[num] = obj
                pos = lex.pos
        for _, stream in streams:
            self._expand(stream)

    @staticmethod
    def _skip_ws_static(data: bytes, pos: int) -> int:
        while pos < len(data) and data[pos] in WHITESPACE:
            pos += 1
        return pos

    def _skip_ws(self, pos: int) -> int:
        return self._skip_ws_static(self.data, pos)

    def _expand(self, stream: Stream) -> None:
        data = stream.data()
        count = self.resolve(stream.dict.get("N"))
        first = self.resolve(stream.dict.get("First"))
        lex = Lexer(data)
        pairs = []
        for _ in range(count):
            pairs.append((lex.next(), lex.next()))
        for num, offset in pairs:
            # An object stream holds objects no later update replaced; an
            # object written again at top level (an incremental save) wins.
            if num in self.objects and not isinstance(self.objects[num], Stream):
                continue
            self.objects.setdefault(num, Lexer(data, first + offset).parse_object())

    def _find_root(self) -> Ref:
        for trailer in reversed(self.trailers):
            root = trailer.get("Root")
            if isinstance(root, Ref):
                return root
        for num in sorted(self.objects, reverse=True):
            obj = self.objects[num]
            if isinstance(obj, dict) and obj.get("Type") == "Catalog":
                return Ref(num)
        raise PdfError("the PDF has no catalog")

    def resolve(self, value: Any) -> Any:
        seen = 0
        while isinstance(value, Ref):
            value = self.objects.get(value.num)
            seen += 1
            if seen > 32:
                raise PdfError("a reference loops")
        return value

    @property
    def catalog(self) -> dict:
        return self.resolve(self.root_ref)

    def pages(self) -> list[Page]:
        out: list[Page] = []

        def walk(node_ref: Any, inherited: dict, depth: int) -> None:
            if depth > 64:
                raise PdfError("the page tree is too deep")
            node = self.resolve(node_ref)
            if not isinstance(node, dict):
                return
            carried = dict(inherited)
            for key in ("Resources", "Rotate", "MediaBox"):
                if key in node:
                    carried[key] = node[key]
            if node.get("Type") == "Pages" or "Kids" in node:
                for kid in self.resolve(node.get("Kids")) or []:
                    walk(kid, carried, depth + 1)
            else:
                ref = node_ref if isinstance(node_ref, Ref) else None
                out.append(Page(self, node, carried, ref))

        walk(self.catalog.get("Pages"), {}, 0)
        return out

    def fields(self) -> dict[str, FormField]:
        """Every terminal form field by its full name."""
        form = self.resolve(self.catalog.get("AcroForm"))
        out: dict[str, FormField] = {}
        if not isinstance(form, dict):
            return out

        def walk(ref: Any, prefix: str, inherited_ft: Any, depth: int) -> None:
            if depth > 32:
                return
            node = self.resolve(ref)
            if not isinstance(node, dict):
                return
            part = text_string(self.resolve(node.get("T"))) if "T" in node else ""
            name = f"{prefix}.{part}" if prefix and part else (part or prefix)
            ft = node.get("FT", inherited_ft)
            kids = [k for k in (self.resolve(node.get("Kids")) or []) if isinstance(self.resolve(k), dict)]
            named_kids = [k for k in kids if "T" in self.resolve(k)]
            if named_kids:
                for kid in named_kids:
                    walk(kid, name, ft, depth + 1)
                return
            value = self.resolve(node.get("V"))
            states = [self.resolve(k).get("AS") for k in kids] or [node.get("AS")]
            out[name] = FormField(name, str(ft) if ft else "", value, states)

        for ref in self.resolve(form.get("Fields")) or []:
            walk(ref, "", None, 0)
        return out


@dataclass
class FormField:
    name: str
    kind: str
    value: Any
    states: list

    @property
    def text(self) -> str:
        return text_string(self.value)

    @property
    def checked(self) -> bool:
        value = self.value
        if isinstance(value, Name):
            return value not in ("Off", "")
        return any(isinstance(s, Name) and s not in ("Off", "") for s in self.states)


class Page:
    def __init__(self, doc: Document, node: dict, inherited: dict, ref: Ref | None) -> None:
        self.doc = doc
        self.node = node
        self.inherited = inherited
        self.ref = ref

    @property
    def rotate(self) -> int:
        value = self.doc.resolve(self.inherited.get("Rotate", 0)) or 0
        return int(value) % 360

    def content(self) -> bytes:
        contents = self.doc.resolve(self.node.get("Contents"))
        if contents is None:
            return b""
        parts = contents if isinstance(contents, list) else [contents]
        out = []
        for part in parts:
            stream = self.doc.resolve(part)
            if isinstance(stream, Stream):
                out.append(stream.data())
        return b"\n".join(out)

    def has_images(self) -> bool:
        res = self.doc.resolve(self.inherited.get("Resources")) or {}
        xobjects = self.doc.resolve(res.get("XObject")) if isinstance(res, dict) else None
        if not isinstance(xobjects, dict):
            return False
        return any(
            isinstance(self.doc.resolve(x), Stream) and self.doc.resolve(x).dict.get("Subtype") == "Image"
            for x in xobjects.values()
        )

    def text(self) -> str:
        return extract_text(self.doc, self.content(), self.doc.resolve(self.inherited.get("Resources")) or {})


# ToUnicode CMaps -------------------------------------------------------------


def parse_cmap(data: bytes) -> tuple[dict[bytes, str], list[int]]:
    mapping: dict[bytes, str] = {}
    widths: set[int] = set()

    def uni(raw: bytes) -> str:
        return raw.decode("utf-16-be", errors="replace")

    for block in re.findall(rb"begincodespacerange(.*?)endcodespacerange", data, re.S):
        for lo in re.findall(rb"<([0-9A-Fa-f]+)>\s*<[0-9A-Fa-f]+>", block):
            widths.add(len(lo) // 2)
    for block in re.findall(rb"beginbfchar(.*?)endbfchar", data, re.S):
        for src, dst in re.findall(rb"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]*)>", block):
            mapping[bytes.fromhex(src.decode())] = uni(bytes.fromhex(dst.decode()))
            widths.add(len(src) // 2)
    for block in re.findall(rb"beginbfrange(.*?)endbfrange", data, re.S):
        for lo, hi, dst in re.findall(rb"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>\s*(<[0-9A-Fa-f]*>|\[[^\]]*\])", block):
            width = len(lo) // 2
            widths.add(width)
            start, end = int(lo, 16), int(hi, 16)
            if end - start > 65535:
                continue
            if dst.startswith(b"["):
                targets = re.findall(rb"<([0-9A-Fa-f]*)>", dst)
                for i, t in enumerate(targets):
                    mapping[(start + i).to_bytes(width, "big")] = uni(bytes.fromhex(t.decode()))
            else:
                base = bytes.fromhex(dst[1:-1].decode())
                for i in range(end - start + 1):
                    target = base[:-1] + bytes([(base[-1] + i) & 0xFF]) if base else b""
                    mapping[(start + i).to_bytes(width, "big")] = uni(target)
    return mapping, sorted(widths) or [1]


class FontDecoder:
    def __init__(self, doc: Document, font: Any) -> None:
        font = doc.resolve(font)
        self.cmap: dict[bytes, str] = {}
        self.widths = [1]
        self.composite = False
        if isinstance(font, dict):
            self.composite = font.get("Subtype") == "Type0"
            tounicode = doc.resolve(font.get("ToUnicode"))
            if isinstance(tounicode, Stream):
                self.cmap, self.widths = parse_cmap(tounicode.data())
        if self.composite and not self.cmap:
            raise PdfError("a composite font has no ToUnicode map, so its text cannot be read")

    def decode(self, raw: bytes) -> str:
        if not self.cmap:
            return raw.decode("cp1252", errors="replace")
        out, i = [], 0
        while i < len(raw):
            for w in self.widths:
                code = raw[i:i + w]
                if code in self.cmap:
                    out.append(self.cmap[code])
                    i += w
                    break
            else:
                w = 2 if self.composite else 1
                if not self.composite:
                    out.append(raw[i:i + 1].decode("cp1252", errors="replace"))
                i += w
        return "".join(out)


def extract_text(doc: Document, content: bytes, resources: dict, depth: int = 0) -> str:
    """The text a content stream shows, in drawing order, a space between
    positioned runs and a newline between text objects."""
    if depth > 8:
        return ""
    fonts = doc.resolve(resources.get("Font")) if isinstance(resources, dict) else None
    fonts = fonts if isinstance(fonts, dict) else {}
    xobjects = doc.resolve(resources.get("XObject")) if isinstance(resources, dict) else None
    xobjects = xobjects if isinstance(xobjects, dict) else {}
    decoders: dict[str, FontDecoder] = {}
    current: FontDecoder | None = None
    out: list[str] = []
    operands: list[Any] = []
    lex = Lexer(content)

    def show(raw: bytes) -> None:
        out.append((current or FontDecoder(doc, None)).decode(raw))

    while not lex.at_end():
        try:
            tok = lex.next()
        except PdfError:
            break
        if isinstance(tok, Keyword):
            op = str(tok)
            if op == "BI":
                end = content.find(b"EI", lex.pos)
                lex.pos = len(content) if end < 0 else end + 2
            elif op == "Tf" and operands and isinstance(operands[0], Name):
                name = str(operands[0])
                if name not in decoders:
                    decoders[name] = FontDecoder(doc, fonts.get(name))
                current = decoders[name]
            elif op == "Tj" and operands and isinstance(operands[-1], bytes):
                show(operands[-1])
            elif op in ("'", '"') and operands and isinstance(operands[-1], bytes):
                out.append(" ")
                show(operands[-1])
            elif op == "TJ" and operands and isinstance(operands[-1], list):
                for item in operands[-1]:
                    if isinstance(item, bytes):
                        show(item)
                    elif isinstance(item, (int, float)) and item < -200:
                        out.append(" ")
            elif op in ("Td", "TD", "T*", "Tm"):
                out.append(" ")
            elif op == "ET":
                out.append("\n")
            elif op == "Do" and operands and isinstance(operands[0], Name):
                xobj = doc.resolve(xobjects.get(str(operands[0])))
                if isinstance(xobj, Stream) and xobj.dict.get("Subtype") == "Form":
                    inner = doc.resolve(xobj.dict.get("Resources")) or resources
                    out.append(extract_text(doc, xobj.data(), inner, depth + 1))
            operands = []
        else:
            try:
                operands.append(lex.parse_object(tok))
            except PdfError:
                break
    return "".join(out)


def squash(text: str) -> str:
    """Text with all whitespace removed and folded case: what phrase checks
    compare, so line breaks and run splits do not matter."""
    return re.sub(r"\s+", "", text).casefold()
