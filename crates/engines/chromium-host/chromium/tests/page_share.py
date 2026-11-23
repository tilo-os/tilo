#!/usr/bin/env python3
"""A real page hands its whole frame over when its window's share is watched.

Usage: page_share.py CEF_HOST PAGE_SHARE_PROBE

The probe (the toolkit's `page-share-probe` example) runs the engine as Browser's
windows run it, drawn from its pixels and captured as a display list only while a
session shares the window with someone watching, draws it with the web view a
window draws it with, and says the window is watched as the session's ask for a
whole frame does. What the share's outbox then holds is what the app sends its
session. The page is the form the remote end-to-end run's machines answer, at the
size its window has there and from the same kind of origin (a loopback server).

Pages of the size people read are handed over too, with every image they show: a
long article with photographs, a long form and a large table, in a window of
ordinary size. Each prints how large its whole frame and its images are. A page
larger than the workspace carries is not waited for: the app says how large it is.
A page
that changes reaches what was handed over while the change is new, and a page
nothing changes costs no frame, watched or not: the probe turns the app's loop as
its runner does, a frame for each wake of the engine, and counts.

What a page costs its engine is held to counts, which no busy machine changes: a
page is handed over, or said to be too large, of one capture of its layout, and
an engine that takes longer over a capture than the app waits before it looks
again is asked for no second one, and its answer is taken when it comes.

What arrives is also compared with what the engine laid out, item by item, on
pages of the kinds people share: a styled article, a table, a form with every
common control, text in both directions with an emoji, and a page zoomed in.
Each page measures itself with the engine's own layout (the box of every line
of text, the face the engine set it in, what every control holds and shows)
and posts that to the suite; the probe prints what the share handed over. A run
of text must arrive in the engine's own line box with its size, weight, slant,
colour and class of face; a control as its person sees it; a secret never.
No personal profile, display or network beyond that loopback.
"""
import base64
import glob
import http.server
import json
import os
from pathlib import Path
import random
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import zlib

if len(sys.argv) != 3:
    raise SystemExit(__doc__)
HOST = Path(sys.argv[1]).resolve(strict=True)
PROBE = Path(sys.argv[2]).resolve(strict=True)

# How long an app has to hand its page over once asked, here: the whole of it, the
# engine's snapshot and the frames around it, on a machine without a GPU.
WITHIN_MS = 5000
# The window the machines share: its page's size there.
WIDTH, HEIGHT = 604, 708

FORM = b"""<!doctype html><title>Apply</title><h1>Apply for a visitor visa</h1>
<form action="/sent" method="get">
<label>Given names <input name="given" value="Amina"></label>
<label>Family name <input name="family" value="Otieno"></label>
<label>Nationality <select name="nationality" id="nat"><option value="">Select a country</option>
<option selected>Kenya</option><option>Other</option></select></label>
<div id="other" hidden><label>Which country? <input name="other" required></label></div>
<label>Passport number <input name="passport" value="A1234567"></label>
<fieldset><legend>Refused before?</legend>
<label><input type="radio" name="refused" value="yes"> Yes</label>
<label><input type="radio" name="refused" value="no" checked> No</label></fieldset>
<label>Arrival <input type="date" name="arrival" value="2027-03-01"></label>
<button>Send application</button></form>
<script>document.getElementById('nat').addEventListener('change', e =>
document.getElementById('other').hidden = e.target.value !== 'Other');</script>
"""
# What the form shows whoever looks at it: its heading and three of its labels.
SHOWN = ("Apply for a visitor visa", "Given names", "Family name", "Passport number")
# What its fields hold.
HELD = ("Amina", "Otieno", "A1234567")

# A window of ordinary size, for the pages people read.
LARGE = (1280, 800)
# How long a page of megabytes has to be handed over whole here: its capture, its
# encoding and every image read back from the engine, on a machine without a GPU
# that runs the rest of the suite beside it.
LARGE_WITHIN_MS = 20000
# How long a page nothing changes is counted for, watched and then unwatched.
STILL_MS = 4000
# How soon a change the page makes to itself is in what was handed over.
PROMPT_MS = 1000
# An engine slower over a capture than the app waits before it looks again, twice
# over: the app looks after three seconds and again six seconds later
# (`REQUEST_TIMEOUT` of the toolkit's paint source).
SLOW_CAPTURE_MS = 10000


def png(width, height, seed):
    """A picture no encoder makes smaller: noise, about three bytes a pixel, as a
    photograph is after its own compression."""
    noise = random.Random(seed)
    rows = b"".join(b"\x00" + noise.randbytes(width * 3) for _ in range(height))

    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data)))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows, 0)) + chunk(b"IEND", b""))


def article():
    """A long article: a headline, a large photograph under it, and a smaller one
    every twenty paragraphs."""
    sentence = ("The harbour kept its own hours, and the people who worked it had long "
                "since stopped arguing with the tide about when a day began. ")
    parts = ["<!doctype html><title>Article</title><style>body{font:16px sans-serif;"
             "max-width:760px;margin:auto}img{display:block;max-width:100%}</style>"
             "<h1>The long way round</h1><img src='/hero.png' width=760 height=422>"]
    for paragraph in range(180):
        parts.append(f"<p>{paragraph + 1}. {sentence * 3}</p>")
        if paragraph % 20 == 10:
            parts.append(f"<img src='/photo/{paragraph // 20}.png' width=320 height=200>")
    parts.append("<p>The last paragraph of the article.</p>")
    return "".join(parts).encode()


def long_form():
    """A long form: two hundred and forty answered questions."""
    parts = ["<!doctype html><title>Register</title><style>label{display:block;margin:6px}"
             "</style><h1>Register a vessel</h1><form>"]
    for field in range(240):
        parts.append(f"<label>Question {field + 1} <input name='f{field}' "
                     f"value='Answer {field + 1}'></label>")
    parts.append("<label>Final declaration <input name='last' value='Signed'></label>"
                 "<button>Send</button></form>")
    return "".join(parts).encode()


def table(rows):
    """A large table: eight columns of what a log holds."""
    parts = ["<!doctype html><title>Shipments</title><style>td,th{border:1px solid #999;"
             "padding:2px 6px}</style><h1>Shipments</h1><table><tr>"]
    parts += [f"<th>Column {column + 1}</th>" for column in range(8)]
    parts.append("</tr>")
    for row in range(rows):
        parts.append(f"<tr><td>Row {row + 1}</td>")
        parts += [f"<td>{(row * 7919 + column * 104729) % 100000}</td>"
                  for column in range(7)]
        parts.append("</tr>")
    parts.append("</table><p>End of the shipments.</p>")
    return "".join(parts).encode()


# A page that changes itself, stamping each change with the clock.
TICKING = b"""<!doctype html><title>Ticking</title><h1>A page that changes</h1>
<p id="t">tick 0 @0</p>
<script>let n = 0; const t = document.getElementById('t');
const tick = () => { t.textContent = 'tick ' + n++ + ' @' + Date.now(); };
tick(); setInterval(tick, 1300);</script>
"""

# One picture larger than the workspace carries.
POSTER = (b"<!doctype html><title>Poster</title><h1>One large picture</h1>"
          b"<img src='/poster.png' width=800 height=550><p>Under the picture.</p>")

# What a page says of itself, measured by the engine that laid it out: every
# line box of its text with the style it is set in, and every control with what
# it holds. A zoomed page's script measures in the page's own pixels, which the
# engine lays out larger by `devicePixelRatio`; the boxes are posted as laid
# out. `klass` is the generic family the engine sets a style's face like, told
# by the width of a line of text: nothing where the engine's faces do not tell
# the three apart.
MEASURE = """<script>
(() => {
  const pen = document.createElement('canvas').getContext('2d');
  const line = 'The quick brown fox, iIl1 mmmWWW 0123456789';
  const klass = style => {
    const width = family => {
      pen.font = `${style.fontStyle} ${style.fontWeight} 40px ${family}`;
      return pen.measureText(line).width;
    };
    const own = width(style.fontFamily);
    const like = ['serif', 'sans-serif', 'monospace'].filter(g => Math.abs(width(g) - own) < 0.01);
    return like.length === 1 ? like[0] : null;
  };
  const measure = () => {
    const z = devicePixelRatio;
    const box = r => [r.x * z, r.y * z, r.width * z, r.height * z];
    const active = document.activeElement;
    const out = {zoom: z, focus: active && active !== document.body ? active.id : null,
                 has_focus: document.hasFocus(), texts: [], controls: []};
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let node; (node = walker.nextNode());) {
      const element = node.parentElement;
      if (!node.nodeValue.trim()) continue;
      if (element.closest('script,style,select,textarea,noscript')) continue;
      const style = getComputedStyle(element);
      const range = document.createRange();
      range.selectNodeContents(node);
      out.texts.push({text: node.nodeValue, rects: [...range.getClientRects()].map(box),
        family: style.fontFamily, klass: klass(style), size: parseFloat(style.fontSize) * z,
        weight: Number(style.fontWeight), italic: style.fontStyle === 'italic',
        color: style.color, line: style.textDecorationLine});
    }
    for (const element of document.querySelectorAll('input,select,textarea,button')) {
      const style = getComputedStyle(element);
      out.controls.push({id: element.id, tag: element.localName, type: element.type,
        rect: box(element.getBoundingClientRect()), value: element.value,
        checked: !!element.checked, disabled: element.disabled, read_only: !!element.readOnly,
        placeholder: element.placeholder || '', color: style.color,
        placeholder_color: getComputedStyle(element, '::placeholder').color,
        option: element.localName === 'select' && element.selectedOptions[0]
          ? element.selectedOptions[0].label : null,
        outline: style.outlineStyle, outline_color: style.outlineColor,
        family: style.fontFamily, klass: klass(style), size: parseFloat(style.fontSize) * z});
    }
    navigator.sendBeacon('/measured' + location.pathname, JSON.stringify(out));
  };
  const soon = () => setTimeout(measure, 60);
  addEventListener('load', soon);
  addEventListener('resize', soon);
  addEventListener('focus', soon, true);
  if (document.fonts) document.fonts.ready.then(soon);
  setTimeout(measure, 700);
})();
</script>"""

# A styled article: a face of the page's own, headings in another family,
# a link, a struck-out price, emphasis, code, lists, a quotation and a table.
STYLED = """<!doctype html><meta charset="utf-8"><title>Styled</title><style>
@font-face{font-family:"Harbour";src:url(/harbour.ttf)}
body{font:17px/1.5 "Harbour",Georgia,serif;max-width:640px;margin:24px auto;color:#222}
h1{font:700 34px/1.2 "Helvetica Neue",Arial,sans-serif}
h2{font:600 22px/1.3 Arial,sans-serif;color:#0a3d62}
a{color:#0645ad}
blockquote{border-left:4px solid #999;margin-left:0;padding-left:16px;font-style:italic}
code{font:14px monospace;background:#eee}
.old{text-decoration:line-through}
.note{letter-spacing:.5px;text-transform:uppercase;font-size:12px}
td,th{border:1px solid #999;padding:2px 8px} th{background:#0a3d62;color:#fff}
td.sum{text-align:right}
</style>
<h1>The harbour at dawn</h1><p class="note">By the night shell</p>
<p>The harbour kept its own hours, and <a href="/more">the people who worked it</a> had long
since stopped arguing with the tide. The fee was <span class="old">12 crowns</span> 9 crowns,
<em>paid at the hold</em>, and <strong>nobody</strong> waited for a receipt longer than it takes
to read <code>tide.table</code> twice over.</p>
<h2>What the boats carried</h2>
<ul><li>Salt and rope</li><li>Letters for the island</li></ul>
<ol><li>First the pilots</li><li>Then the ferries</li></ol>
<blockquote>Nobody argues with the tide.</blockquote>
<table><tr><th>Berth</th><th>Vessel</th><th>Tonnes</th></tr>
<tr><td>North 1</td><td>Aurelia</td><td class="sum">1,204</td></tr>
<tr><td>North 2</td><td>Kestrel</td><td class="sum">87</td></tr></table>
<p>The last line of the article.</p>
""" + MEASURE

# A form with every common control, in every state a reviewer has to tell
# apart: answered and empty, read-only and disabled, ticked and not, a date and
# a number the engine words itself, and two secrets.
PASSWORD, CARD = "hunter2-secret", "4111111111111111"
CONTROLS = f"""<!doctype html><meta charset="utf-8"><title>Controls</title>
<h1>Every control</h1><form>
<p><label>Name <input id="name" value="Ada Lovelace"></label>
<label>Empty <input id="empty" placeholder="Type a name"></label></p>
<p><label>Read only <input id="readonly" value="Fixed" readonly></label>
<label>Disabled <input id="disabled" value="Not now" disabled></label></p>
<p><label>Password <input id="password" type="password" value="{PASSWORD}"></label>
<label>Card <input id="card" autocomplete="cc-number" value="{CARD}"></label></p>
<p><label>Date <input id="date" type="date" value="2027-03-01"></label>
<label>No date <input id="nodate" type="date"></label>
<label>Time <input id="time" type="time" value="14:30"></label>
<label>Number <input id="number" type="number" value="1234.5"></label></p>
<p><label>Country <select id="country"><option>Kenya</option><option selected>Brazil</option>
</select></label>
<label>Closed <select id="closed" disabled><option>None</option></select></label></p>
<p><label><input id="agree" type="checkbox" checked> Agree</label>
<label><input id="news" type="checkbox"> News</label>
<label><input id="off" type="checkbox" checked disabled> Off</label>
<label><input id="yes" type="radio" name="r"> Yes</label>
<label><input id="no" type="radio" name="r" checked> No</label></p>
<p><label>Level <input id="level" type="range" min="0" max="10" value="7"></label>
<label>Colour <input id="colour" type="color" value="#ff8800"></label></p>
<p><label>Notes <textarea id="notes">Line one
Line two</textarea></label>
<label>No notes <textarea id="nonotes" placeholder="Say more"></textarea></label></p>
<p><button id="send">Send</button> <input id="submit" type="submit">
<input id="reset" type="reset" value="Start over"></p>
</form>
<script>document.getElementById('name').focus();</script>
""" + MEASURE

# Text in both directions, mixed in one line, and emoji.
DIRECTIONS = """<!doctype html><meta charset="utf-8"><title>Directions</title>
<h1>Two directions</h1>
<p dir="rtl" lang="he">שלום עולם, זהו מבחן.</p>
<p dir="rtl" lang="ar">مرحبا بالعالم</p>
<p>English with עברית inside it.</p>
<p>Launch 🚀 day 🎉 done ✅</p>
<p><label>Name <input id="rtl" dir="rtl" value="שלום"></label></p>
""" + MEASURE


def page_font():
    """A face for a page to bring as its own: any small one this machine has, served
    under a name no machine has, so nothing but the page's own file can set it."""
    for pattern in ("/usr/share/fonts/**/*.ttf", "/usr/share/fonts/**/*.otf"):
        for path in sorted(glob.glob(pattern, recursive=True)):
            if os.path.getsize(path) < 2 << 20:
                return Path(path).read_bytes()
    return b""


# What each page measured of itself, by its path: the newest it posted.
MEASURED = {}

# Set to have the suite print what it compared, whole: every item that arrived, what each
# page measured, the engine's own layout snapshot of the form with every control (what
# the converter's tests keep as a fixture, `crates/engines/paint-stream/tests/fixtures`)
# and the faces the machine has. Each is printed deflated and base64 encoded, in lines
# of `RECORD name part/parts data`.
RECORD = bool(os.environ.get("TILO_PAGE_SHARE_RECORD"))


def record(name, data):
    if not RECORD:
        return
    packed = base64.b64encode(zlib.compress(data, 9)).decode()
    lines = [packed[at:at + 3000] for at in range(0, len(packed), 3000)] or [""]
    for number, line in enumerate(lines):
        print(f"RECORD {name} {number + 1}/{len(lines)} {line}", flush=True)

HTML = "text/html; charset=utf-8"
PAGES = {
    "/": (HTML, FORM),
    "/article": (HTML, article()),
    "/hero.png": ("image/png", png(900, 500, 1)),
    "/long-form": (HTML, long_form()),
    "/table": (HTML, table(2000)),
    "/register": (HTML, table(5000)),
    "/ticking": (HTML, TICKING),
    "/poster": (HTML, POSTER),
    "/poster.png": ("image/png", png(1600, 1100, 2)),
    "/styled": (HTML, STYLED.encode()),
    "/controls": (HTML, CONTROLS.encode()),
    "/directions": (HTML, DIRECTIONS.encode()),
    "/harbour.ttf": ("font/ttf", page_font()),
}
PAGES.update({f"/photo/{photo}.png": ("image/png", png(320, 200, 10 + photo))
              for photo in range(9)})


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        kind, body = PAGES.get(self.path.split("?")[0], PAGES["/"])
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        """What a page measured of itself."""
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length)
        if self.path.startswith("/measured/"):
            MEASURED[self.path[len("/measured"):]] = body
        self.send_response(204)
        self.end_headers()

    def log_message(self, *_):
        pass


# The bit that says a run of text is what a field holds, drawn in the room the field
# has rather than in a line box the engine laid out (`ItemKey::SLOT`).
SLOT = 1 << 13
# The parts of an element the converter names (`snapshot.rs`).
CHOICE_MARK, CHOICE_BOX, OUTLINE, FIELD_MARK, FIELD_VALUE, TEXT_LINE = 5, 6, 7, 8, 10, 13
# Slot text drawn from its box's end (`ItemKey::SLOT_END`).
SLOT_END = 1 << 12
DOT = "\u2022"


def rgba(css):
    """A computed colour as the four bytes a frame carries."""
    inner = css[css.index("(") + 1:css.index(")")].replace("/", " ").replace(",", " ").split()
    alpha = float(inner[3]) if len(inner) > 3 else 1.0
    return [round(float(inner[0])), round(float(inner[1])), round(float(inner[2])),
            round(alpha * 255)]


def apart(first, second):
    """How far two boxes are apart, at their farthest edge."""
    return max(abs(a - b) for a, b in zip(first, second))


def inside(inner, outer, by=2.0):
    return (inner[0] >= outer[0] - by and inner[1] >= outer[1] - by
            and inner[0] + inner[2] <= outer[0] + outer[2] + by
            and inner[1] + inner[3] <= outer[1] + outer[3] + by)


def text_faults(items, measured):
    """Every line box the engine laid out against the runs that arrived: what differs."""
    runs = [item for item in items if item["k"] == "text" and not item["part"] & SLOT]
    faults = []
    for text in measured["texts"]:
        said = " ".join(text["text"].split())[:40]
        for rect in text["rects"]:
            if rect[2] < 1 or rect[3] < 1:
                continue
            run = min(runs, key=lambda run: apart(run["r"], rect), default=None)
            if run is None or apart(run["r"], rect) > 1.5:
                faults.append(f"{said!r}: the engine's line box {rect} has no run; the nearest "
                              f"is {run and (run['text'], run['r'])}")
                continue
            # A line box is one line: a line break of the page's source is a space on it.
            if any(space in run["text"] for space in "\n\t\r") or "  " in run["text"]:
                faults.append(f"{said!r}: the run {run['text']!r} carries the source's spacing, "
                              "which a device draws as a second line")
            if abs(run["size"] - text["size"]) > 0.6:
                faults.append(f"{said!r}: set at {run['size']}, the engine at {text['size']}")
            if run["weight"] != text["weight"] or run["italic"] != text["italic"]:
                faults.append(f"{said!r}: weight {run['weight']} italic {run['italic']}, the "
                              f"engine {text['weight']} {text['italic']}")
            if run["color"] != rgba(text["color"]):
                faults.append(f"{said!r}: coloured {run['color']}, the engine {text['color']}")
            if text["klass"] and run["class"] != text["klass"]:
                faults.append(f"{said!r}: a {run['class']} face for {run['family']!r}, the "
                              f"engine a {text['klass']} one")
    return faults


class PageShare(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f"http://127.0.0.1:{cls.server.server_port}"
        if RECORD:
            faces = subprocess.run(["fc-list", ":", "family"], stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, check=False).stdout
            record("faces", faces)

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def probe(self, *arguments, expect=SHOWN, path="/", size=(WIDTH, HEIGHT),
              within=WITHIN_MS):
        """Run the probe on the page at `path` and return what it printed: one object."""
        runtime = tempfile.mkdtemp(prefix="tilo-page-share-")
        try:
            env = dict(os.environ,
                       XDG_RUNTIME_DIR=runtime,
                       XDG_CONFIG_HOME=runtime + "/config",
                       XDG_CACHE_HOME=runtime + "/cache",
                       XDG_DATA_HOME=runtime + "/data")
            env.pop("DISPLAY", None)
            env.pop("WAYLAND_DISPLAY", None)
            command = [str(PROBE), str(HOST), runtime + "/profile", self.url + path,
                       str(size[0]), str(size[1]), "--within-ms", str(within), *arguments]
            for text in expect:
                command += ["--expect", text]
            ran = subprocess.run(command, env=env, stdout=subprocess.PIPE, timeout=180)
            said = [line for line in ran.stdout.decode(errors="replace").splitlines()
                    if line.startswith("{")]
            self.assertEqual(ran.returncode, 0, ran.stdout[-2000:])
            self.assertTrue(said, "the probe printed no result")
            result = json.loads(said[-1])
            print(f"\n  probe {path} {' '.join(arguments) or '(watched once loaded)'}: "
                  f"{json.dumps({k: v for k, v in result.items() if k not in ('texts', 'items')})}",
                  flush=True)
            return result
        finally:
            # The engine's storage children can finish after the host exits.
            deadline = time.monotonic() + 10
            while Path(runtime).exists():
                try:
                    shutil.rmtree(runtime)
                except OSError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.05)

    def handed_over(self, result, what, within=WITHIN_MS):
        self.assertTrue(result["whole"], f"{what}: no whole frame of the page came: {result}")
        self.assertFalse(result["missing"],
                         f"{what}: the page handed over lacks {result['missing']}; "
                         f"it carries {result['texts']}")
        self.assertIsNotNone(result["complete_ms"], result)
        self.assertLess(result["complete_ms"], within, result)
        self.assertGreater(result["whole_bytes"], 0, result)
        self.assertLessEqual(result["largest_payload"], result["largest_carried"],
                             "every payload fits what the app's channel to its session carries")

    def of_one_capture(self, result, what):
        """What the page cost its engine until it was whole, or said to be too large: one
        capture of its layout, answered, and one more at most for each picture the engine
        painted meanwhile, which is a change of the page. A page nothing changes costs
        exactly one, however long the engine took over it."""
        captures = result["captures"]
        print(f"  {what}: {captures['asked']} captures asked and {captures['answered']} "
              f"answered, taking {captures['took_ms']} ms, beside {captures['asks']} smaller "
              f"asks, while the engine painted {captures['pictures']} pictures", flush=True)
        self.assertGreaterEqual(captures["answered"], 1,
                                f"{what}: the engine answered no capture: {captures}")
        self.assertLessEqual(captures["asked"], 1 + captures["pictures"],
                             f"{what}: the page was captured more than once and did not "
                             f"change: {captures}")

    def test_a_page_asked_for_is_handed_over_whole_within_seconds(self):
        self.handed_over(self.probe(), "a window watched once its page loaded")

    def test_a_share_watched_before_the_page_loaded_comes_to_hold_the_page(self):
        self.handed_over(self.probe("--watched-before-load"),
                         "a window watched before its page loaded")

    def test_the_page_handed_over_carries_what_its_fields_hold(self):
        self.handed_over(self.probe(expect=SHOWN + HELD), "a form that was answered")

    def large(self, path, top, bottom, what):
        """Hand over the page at `path` in a window of ordinary size: `top` is what its
        window shows first, `bottom` what its document ends with."""
        result = self.probe("--has", bottom, "--still-ms", "1000", path=path, size=LARGE,
                            within=LARGE_WITHIN_MS, expect=(top,))
        page = result["page"] or {}
        print(f"  {what}: whole frame {page.get('whole_bytes')} bytes in "
              f"{page.get('items')} items; {page.get('images')} images of "
              f"{page.get('image_bytes')} bytes, the largest {page.get('largest_image')}; "
              f"handed over: whole frame {result['whole_bytes']} bytes, "
              f"{result['images']} images of {result['image_bytes']} bytes, "
              f"{result['images_missing']} missing, largest payload "
              f"{result['largest_payload']}, in {result['complete_ms']} ms; "
              f"the document's end {'not ' if result['lacks'] else ''}carried", flush=True)
        self.handed_over(result, what, within=LARGE_WITHIN_MS)
        self.assertEqual(result["images_missing"], 0,
                         f"{what}: the page handed over names images that never came: {result}")
        self.assertEqual(result["images"], page.get("images"),
                         f"{what}: not every image of the page was handed over: {result}")
        self.of_one_capture(result, what)
        return result

    def test_a_long_article_is_handed_over_with_its_photographs(self):
        result = self.large("/article", "The long way round",
                            "The last paragraph of the article.", "a long article")
        self.assertGreaterEqual(result["images"], 1, result)

    def test_a_long_form_is_handed_over(self):
        self.large("/long-form", "Register a vessel", "Final declaration", "a long form")

    def test_a_large_table_is_handed_over(self):
        self.large("/table", "Shipments", "End of the shipments.", "a table of 2000 rows")

    def test_a_page_larger_than_the_workspace_carries_is_said_to_be(self):
        """A table of 5000 rows is a whole frame of megabytes more than one object of the
        workspace's live session holds. Nothing of it is handed over as a page, and the app
        does not keep quiet about it: it says how large the frame is, which is what the
        session tells both devices in words."""
        result = self.probe(path="/register", size=LARGE, within=LARGE_WITHIN_MS,
                            expect=("Shipments",))
        page = result["page"] or {}
        print(f"  a table of 5000 rows: whole frame {page.get('whole_bytes')} bytes in "
              f"{page.get('items')} items; the app says {result['too_large']} bytes, "
              f"more than the {result['largest_carried']} a share hands over", flush=True)
        self.assertFalse(result["whole"], f"a page too large was handed over: {result}")
        self.assertIsNotNone(result["too_large"],
                             f"the app said nothing of a page it cannot hand over: {result}")
        self.assertGreater(result["too_large"], result["largest_carried"], result)
        self.of_one_capture(result, "a table of 5000 rows")

    def test_a_capture_slower_than_the_app_waits_for_is_taken_and_asked_for_once(self):
        """An engine that takes ten seconds over a capture, on a busy machine or of a very
        large page, is not asked for the capture again while it is at work on it, and what
        it answers is the page: handed over whole, of one capture."""
        result = self.probe("--slow-capture-ms", str(SLOW_CAPTURE_MS), within=LARGE_WITHIN_MS)
        self.handed_over(result, "a page of a slow engine", within=LARGE_WITHIN_MS)
        self.of_one_capture(result, "a page of a slow engine")
        self.assertGreaterEqual(result["captures"]["slowest_ms"], SLOW_CAPTURE_MS, result)

    def test_the_engine_answers_what_it_is_asked_in_order(self):
        """What tells an engine still at work on a capture from one that lost the request:
        something small asked behind a capture of a large page is answered behind it."""
        result = self.probe("--order", path="/table", size=LARGE, within=LARGE_WITHIN_MS,
                            expect=("Shipments",))
        order = result["order"]
        print(f"  asked for a capture and, behind it, something small: the engine answered "
              f"the {order['first']} first, the capture after {order['capture_ms']} ms and "
              f"the small ask after {order['small_ms']} ms", flush=True)
        self.assertIsNotNone(order["capture_ms"], order)
        self.assertIsNotNone(order["small_ms"], order)
        self.assertEqual(order["first"], "capture", order)

    def test_a_picture_larger_than_the_workspace_carries_leaves_its_page_without_it(self):
        """The page arrives, with its text; the one picture of five megabytes does not,
        which the device showing the page cannot tell from a picture still loading."""
        result = self.probe("--has", "Under the picture.", "--still-ms", "1000",
                            path="/poster", size=LARGE, within=LARGE_WITHIN_MS,
                            expect=("One large picture",))
        page = result["page"] or {}
        print(f"  a page with one picture of {page.get('largest_image')} bytes: handed over "
              f"{result['images']} images, {result['images_missing']} missing", flush=True)
        self.handed_over(result, "a page with one very large picture", within=LARGE_WITHIN_MS)
        self.assertFalse(result["lacks"], result)
        self.assertEqual((result["images"], result["images_missing"]), (0, 1), result)

    def test_a_page_that_changes_reaches_what_was_handed_over_while_it_is_new(self):
        result = self.probe("--changes", "5", path="/ticking", expect=("tick",))
        self.handed_over(result, "a page that changes itself")
        took = result["change_ms"]
        print(f"  changes reached what was handed over after {took} ms", flush=True)
        self.assertEqual(len(took), 5, f"five changes were waited for: {result}")
        self.assertLess(max(took), PROMPT_MS, result)

    def test_a_page_nothing_changes_costs_no_frame_watched_or_not(self):
        result = self.probe("--still-ms", str(STILL_MS))
        self.handed_over(result, "a still page")
        for phase, what in (("still_watched", "while it is watched"),
                            ("still_unwatched", "once nobody watches")):
            cost = result[phase]
            print(f"  a still page {what}, over {STILL_MS} ms: {cost}", flush=True)
            self.assertEqual(cost["frames"], 0,
                             f"a still page drew frames {what}: {cost}")
            self.assertEqual(cost["view_asks"], 0,
                             f"the view of a still page asked for frames {what}: {cost}")
            self.assertEqual(cost["payloads"], 0,
                             f"a still page handed something over {what}: {cost}")

    def looks(self, path, *arguments, expect, size=(WIDTH, HEIGHT)):
        """Hand over the page at `path` and return what arrived, item by item, with what
        the page measured of itself."""
        MEASURED.pop(path, None)
        result = self.probe("--items", "--settle-ms", "1500", *arguments, path=path, size=size,
                            within=LARGE_WITHIN_MS, expect=expect)
        self.handed_over(result, path, within=LARGE_WITHIN_MS)
        self.assertIn(path, MEASURED, f"{path} measured nothing of itself")
        measured = json.loads(MEASURED[path])
        boxes = sum(len(text["rects"]) for text in measured["texts"])
        print(f"  {path} {' '.join(arguments)}: whole frame {result['whole_bytes']} bytes in "
              f"{len(result['items'])} items; the engine laid out {boxes} line boxes at zoom "
              f"{measured['zoom']}", flush=True)
        name = path.strip("/") + "".join(arguments).replace("--", ".")
        record(f"{name}.items", json.dumps(result["items"]).encode())
        record(f"{name}.measured", MEASURED[path])
        return result["items"], measured

    def no_faults(self, faults, what):
        for fault in faults:
            print(f"  {what}: {fault}", flush=True)
        self.assertFalse(faults, f"{what}: what arrived differs from what the engine laid out")

    def test_a_styled_article_arrives_as_the_engine_laid_it_out(self):
        """Every line of a styled article arrives in the engine's own line box, set at the
        engine's size, weight, slant and colour and in its class of face; a struck-out
        price is struck out and a link underlined; a table's cells keep their edges."""
        items, measured = self.looks("/styled", expect=("The harbour at dawn",), size=LARGE)
        faults = text_faults(items, measured)
        lines = [item["r"] for item in items
                 if item["k"] == "fill" and item["part"] == TEXT_LINE]
        for text in measured["texts"]:
            if text["line"] == "none":
                continue
            for rect in text["rects"]:
                if not any(inside(line, rect) and line[2] >= rect[2] - 2 for line in lines):
                    faults.append(f"{text['text']!r} is drawn with a line ({text['line']}) "
                                  "and arrives without")
        edges = [item for item in items if item["k"] == "border"
                 and item["color"] == [153, 153, 153, 255]]
        if len(edges) < 9:
            faults.append(f"the table's nine cells arrive with {len(edges)} edges")
        self.no_faults(faults, "a styled article")

    def test_every_common_control_is_shown_as_its_person_sees_it(self):
        """A form with every common control, the first field being edited. What a field
        shows arrives, not what it holds: a date and a number in the engine's words, the
        option a select shows with its mark, a tick, a slider's thumb, a colour's swatch,
        a hint where nothing was typed, a dimmed answer where none can be given, and the
        engine's ring around the field being edited and around no other."""
        snapshot = None
        arguments = ["--focus"]
        if RECORD:
            handle, snapshot = tempfile.mkstemp(prefix="tilo-page-share-snapshot-")
            os.close(handle)
            os.chmod(snapshot, 0o666)
            arguments += ["--snapshot", snapshot]
        try:
            items, measured = self.looks("/controls", *arguments, expect=("Every control",),
                                         size=LARGE)
            if snapshot:
                record("controls.snapshot", Path(snapshot).read_bytes())
        finally:
            if snapshot:
                os.unlink(snapshot)
        faults = text_faults(items, measured)
        controls = {control["id"]: control for control in measured["controls"]}

        def within(control, kind, part=None):
            return [item for item in items if item["k"] == kind
                    and (part is None or item["part"] == part)
                    and inside(item["r"], control["rect"])]

        def shows(name, text, colour=None):
            control = controls[name]
            held = [item for item in within(control, "text") if item["part"] & SLOT]
            said = [item["text"] for item in held]
            if said != [text] if isinstance(text, str) else said != text:
                faults.append(f"{name} shows {text!r} and arrives as {said}")
            wanted = rgba(control[colour or "color"])
            if any(item["color"] != wanted for item in held):
                faults.append(f"{name} is coloured {control[colour or 'color']} and arrives "
                              f"as {[item['color'] for item in held]}")

        shows("name", "Ada Lovelace")
        shows("empty", "Type a name", "placeholder_color")
        shows("readonly", "Fixed")
        shows("disabled", "Not now")
        shows("password", DOT * len(PASSWORD))
        shows("card", DOT * len(CARD))
        shows("date", "03/01/2027")
        shows("nodate", "mm/dd/yyyy")
        # The engine sets a narrow space that does not break before the half of the day.
        shows("time", "02:30\u202fPM")
        shows("number", "1234.5")
        shows("country", "Brazil")
        shows("closed", "None")
        shows("notes", ["Line one", "Line two"])
        shows("nonotes", "Say more", "placeholder_color")
        shows("submit", "Submit")
        shows("reset", "Start over")
        if rgba(controls["disabled"]["color"]) == rgba(controls["name"]["color"]):
            faults.append("the engine draws a disabled answer like any other: nothing to tell")
        for name in ("date", "nodate", "time", "country", "closed"):
            if len(within(controls[name], "vector", FIELD_MARK)) != 1:
                faults.append(f"{name} arrives without the mark that says it opens")
        for name in ("name", "number", "notes"):
            if within(controls[name], "vector", FIELD_MARK):
                faults.append(f"{name} arrives with a mark and opens nothing")
        for name, ticked in (("agree", True), ("news", False), ("off", True),
                             ("yes", False), ("no", True)):
            marks = [item for item in items if item["part"] == CHOICE_MARK
                     and inside(item["r"], controls[name]["rect"])]
            if bool(marks) != ticked:
                faults.append(f"{name} is {'chosen' if ticked else 'not chosen'} and arrives "
                              f"with {len(marks)} marks")
            if not [item for item in items if item["part"] == CHOICE_BOX
                    and inside(item["r"], controls[name]["rect"])]:
                faults.append(f"{name} arrives without its box")
        level = controls["level"]["rect"]
        thumbs = [item["r"] for item in items if item["part"] == FIELD_VALUE
                  and inside(item["r"], level)]
        if len(thumbs) != 1:
            faults.append(f"the slider arrives with {len(thumbs)} thumbs")
        else:
            along = (thumbs[0][0] + thumbs[0][2] / 2 - level[0]) / level[2]
            if not 0.6 < along < 0.8:
                faults.append(f"the slider stands at 7 of 10 and its thumb arrives {along:.2f} "
                              "of the way along")
        swatches = [item["color"] for item in items if item["part"] == FIELD_VALUE
                    and inside(item["r"], controls["colour"]["rect"])]
        if swatches != [[255, 136, 0, 255]]:
            faults.append(f"the colour #ff8800 arrives as {swatches}")
        # The field being edited, as the engine marks it.
        if measured["focus"] != "name" or not measured["has_focus"]:
            faults.append(f"the page has no field being edited: {measured['focus']}, "
                          f"{measured['has_focus']}")
        rings = [item for item in items if item["k"] == "border" and item["part"] == OUTLINE]
        edited = controls["name"]
        around = [ring for ring in rings if inside(edited["rect"], ring["r"], 0.5)
                  and apart(ring["r"], edited["rect"]) <= 6]
        if len(around) != 1 or around[0]["color"] != rgba(edited["outline_color"]):
            faults.append(f"the field being edited arrives with {around}, the engine rings it "
                          f"{edited['outline']} {edited['outline_color']}")
        if len(rings) != len(around):
            faults.append(f"{len(rings) - len(around)} rings arrive around what is not edited")
        self.no_faults(faults, "a form with every common control")

    def test_what_a_secret_field_holds_never_leaves_its_machine(self):
        """A password and a card's number: no character of either is handed over, in any
        item, only a dot for each."""
        result = self.probe("--items", "--settle-ms", "1500", path="/controls", size=LARGE,
                            within=LARGE_WITHIN_MS, expect=("Every control",))
        self.handed_over(result, "a form with secrets", within=LARGE_WITHIN_MS)
        carried = json.dumps(result["items"], ensure_ascii=False)
        for secret in (PASSWORD, CARD):
            for at in range(len(secret) - 3):
                self.assertNotIn(secret[at:at + 4], carried,
                                 "a part of a secret was handed over")
        self.assertIn(DOT * len(PASSWORD), carried, "the password is shown as its dots")

    def test_text_in_both_directions_and_emoji_arrive_where_the_engine_set_them(self):
        """A line written right to left, one that mixes both directions and one with emoji
        arrive run by run in the engine's boxes; a field written right to left is drawn
        from its right."""
        items, measured = self.looks("/directions", expect=("Two directions",))
        faults = text_faults(items, measured)
        field = {control["id"]: control for control in measured["controls"]}["rtl"]
        held = [item for item in items if item["k"] == "text" and item["part"] & SLOT
                and inside(item["r"], field["rect"])]
        if [item["text"] for item in held] != [field["value"]]:
            faults.append(f"the field holds {field['value']!r} and arrives as "
                          f"{[item['text'] for item in held]}")
        elif not held[0]["part"] & SLOT_END:
            faults.append("a field written right to left arrives drawn from its left")
        self.no_faults(faults, "text in both directions")

    def test_a_zoomed_page_arrives_at_the_size_the_engine_drew_it(self):
        """A page zoomed in is laid out larger than its styles say: its text arrives at the
        size the engine set it, in the engine's boxes."""
        items, measured = self.looks("/styled", "--zoom", "2", expect=("The harbour at dawn",),
                                     size=LARGE)
        self.assertGreater(measured["zoom"], 1.3, "the page was not zoomed")
        self.no_faults(text_faults(items, measured), "a zoomed article")


if __name__ == "__main__":
    unittest.main(argv=sys.argv[:1], verbosity=2)
