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
that changes reaches what was handed over while the change is new, and a page
nothing changes costs no frame, watched or not: the probe turns the app's loop as
its runner does, a frame for each wake of the engine, and counts.
No personal profile, display or network beyond that loopback.
"""
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
    """A large table: eight columns of what a ledger holds."""
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

# One picture larger than anything the workspace carries in one piece.
POSTER = (b"<!doctype html><title>Poster</title><h1>One large picture</h1>"
          b"<img src='/poster.png' width=800 height=550><p>Under the picture.</p>")

HTML = "text/html; charset=utf-8"
PAGES = {
    "/": (HTML, FORM),
    "/article": (HTML, article()),
    "/hero.png": ("image/png", png(900, 500, 1)),
    "/long-form": (HTML, long_form()),
    "/table": (HTML, table(2000)),
    "/ledger": (HTML, table(12000)),
    "/ticking": (HTML, TICKING),
    "/poster": (HTML, POSTER),
    "/poster.png": ("image/png", png(1600, 1100, 2)),
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

    def log_message(self, *_):
        pass


class PageShare(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f"http://127.0.0.1:{cls.server.server_port}"

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
                  f"{json.dumps({k: v for k, v in result.items() if k != 'texts'})}", flush=True)
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
        return result

    def test_a_long_article_is_handed_over_with_its_photographs(self):
        result = self.large("/article", "The long way round",
                            "The last paragraph of the article.", "a long article")
        self.assertGreaterEqual(result["images"], 1, result)

    def test_a_long_form_is_handed_over(self):
        self.large("/long-form", "Register a vessel", "Final declaration", "a long form")

    def test_a_large_table_is_handed_over(self):
        self.large("/table", "Shipments", "End of the shipments.", "a table of 2000 rows")

    def test_a_ledger_of_twelve_thousand_rows_is_handed_over(self):
        self.large("/ledger", "Shipments", "End of the shipments.", "a table of 12000 rows")

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


if __name__ == "__main__":
    unittest.main(argv=sys.argv[:1], verbosity=2)
