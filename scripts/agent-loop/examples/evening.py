#!/usr/bin/env python3
"""A representative agent painting session in Canvas, replayed the same way
every time so two builds' costs compare: an evening landscape (sky, hills,
a rise, an oak, two figures, a moon) in about 35 actions and 10 looks.

It does what a model does through MCP: read the tool list and the app's
root page, resolve the schemas it needs, then paint, look and correct.
Arguments it would not know to tune are left at their defaults.

  scripts/agent-loop/session.py start apps/canvas
  scripts/agent-loop/examples/evening.py            # prints what it cost
  scripts/agent-loop/session.py usage canvas --calls
  scripts/agent-loop/session.py stop canvas
"""
import argparse
import base64
import json
import math
import pathlib
import random
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import session as loop  # noqa: E402
import usage as cost  # noqa: E402

INK = "#140f0c"


class Agent:
    def __init__(self, out: pathlib.Path):
        self.session = loop.Session("canvas")
        if not self.session.state_file.exists():
            sys.exit("no canvas session; start one: scripts/agent-loop/session.py start apps/canvas")
        before = set(loop.usage_logs(self.session))
        self.client = loop.McpClient(self.session)
        self.client.request("tools/list", {})
        deadline = time.monotonic() + 30
        while not (set(loop.usage_logs(self.session)) - before) and time.monotonic() < deadline:
            time.sleep(0.1)
        self.log = max(set(loop.usage_logs(self.session)) - before)
        self.out = out
        self.out.mkdir(parents=True, exist_ok=True)
        self.looks = 0
        self.artwork = None

    def find_artwork(self) -> None:
        apps, _ = self.client.tool("tilo_list_apps", {})
        deadline = time.monotonic() + 120
        while True:
            page, _ = self.client.tool("tilo_query", {"app_id": "so.tilo.canvas"})
            found = [r["resource"] for r in page.get("resources", []) if r.get("kind") == "canvas.artwork"]
            if found:
                self.artwork = found[0]
                return
            if time.monotonic() > deadline:
                sys.exit(f"the canvas did not publish its artwork: {page}")
            time.sleep(2)

    def resolve(self, action: str) -> None:
        self.client.tool("tilo_resolve", {"app_id": "so.tilo.canvas", "resource": self.artwork, "action": action})

    def invoke(self, action: str, arguments=None):
        value, images = self.client.tool("tilo_invoke", {
            "app_id": "so.tilo.canvas", "resource": self.artwork, "action": action, "arguments": arguments or {},
        })
        state = value.get("update", {}).get("state")
        if state != "succeeded":
            print(f"{action}: {json.dumps(value)[:400]}", file=sys.stderr)
        for image in images:
            self.looks += 1
            extension = {"image/jpeg": "jpg", "image/webp": "webp"}.get(image.get("mimeType"), "png")
            (self.out / f"look-{self.looks:02d}.{extension}").write_bytes(base64.b64decode(image["data"]))
        return value.get("update", {}).get("output", {}).get("value", value), images

    def close(self) -> dict:
        self.client.close()
        self.client.process.wait(timeout=10)
        return cost.summarise(cost.read_log(self.log))


def mix(a: str, b: str, t: float) -> str:
    a = [int(a[i : i + 2], 16) for i in (1, 3, 5)]
    b = [int(b[i : i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * t):02x}" for x, y in zip(a, b))


def sky_strokes(rng: random.Random) -> list:
    stops = [(0, "#232a3b"), (240, "#4b5466"), (420, "#8a8278"), (540, "#c99a63"), (610, "#e3bd83")]

    def colour(y):
        for (y0, c0), (y1, c1) in zip(stops, stops[1:]):
            if y <= y1:
                return mix(c0, c1, max(0.0, (y - y0) / (y1 - y0)))
        return stops[-1][1]

    strokes, y = [], 0
    while y < 640:
        x = -40
        while x < 1240:
            length = rng.uniform(120, 260)
            points = [[x, y + rng.uniform(-4, 4), 0.8], [x + length / 2, y + rng.uniform(-4, 4), 1.0],
                      [x + length, y + rng.uniform(-4, 4), 0.7]]
            strokes.append({"points": points, "size": rng.uniform(26, 34), "colour": colour(y + rng.uniform(-10, 10)), "wet": True})
            x += length * rng.uniform(0.6, 0.85)
        y += 16
    return strokes


def earth_strokes(rng: random.Random, count: int) -> list:
    strokes = []
    for _ in range(count):
        x = rng.uniform(-40, 1180)
        y = rng.uniform(640 + 0.00018 * (x - 430) ** 2 + 14, 895)
        length = rng.uniform(70, 150)
        slope = 0.00036 * (x - 430)
        strokes.append({"points": [[x, y, 0.6], [x + length, y + slope * length, 0.8]], "size": rng.uniform(8, 16),
                        "colour": rng.choice(["#2a221c", "#2f261e", "#18130f"]), "opacity": 0.6, "wet": True})
    return strokes


def oak_strokes(rng: random.Random) -> list:
    strokes = [{"points": [[418, 652, 1.0], [430, 600, 1.0], [436, 540, 0.95], [428, 470, 0.85]], "size": 26, "colour": INK}]

    def branch(x, y, angle, length, size, depth):
        if depth == 0 or size < 1.2:
            return
        points, a = [[x, y, 1.0]], angle
        for i in range(4):
            a = min(a + rng.uniform(-0.4, 0.4), -0.15)
            x += math.cos(a) * length / 4
            y += math.sin(a) * length / 4
            points.append([x, y, 1.0 - 0.6 * (i + 1) / 4])
        strokes.append({"points": points, "size": size, "colour": INK, "params": {"load": 1.4}})
        for _ in range(rng.choice([2, 2, 3])):
            px, py = points[max(1, int(rng.uniform(0.55, 1.0) * 4))][:2]
            branch(px, py, a + rng.uniform(0.3, 1.0) * rng.choice([-1, 1]), length * rng.uniform(0.45, 0.7), size * 0.6, depth - 1)

    for angle, length, size in [(-1.95, 170, 13), (-1.35, 150, 12), (-2.55, 140, 11), (-0.75, 130, 9)]:
        branch(428, 480, angle, length, size, 5)
    return strokes


def paint(agent: Agent) -> None:
    rng = random.Random(7)
    agent.find_artwork()
    agent.resolve("paint")
    agent.resolve("draw")
    agent.invoke("media")
    agent.invoke("set-artwork", {"title": "Two figures contemplating the moon", "width": 1200, "height": 900,
                                 "texture": "linen", "ground_colour": "#b9a88a"})
    for layer in ("sky", "land", "rise", "oak", "figures", "moon"):
        agent.invoke("add-layer", {"id": layer, "name": layer.title(), "medium": "oil"})
    sky = sky_strokes(rng)
    for i in range(0, len(sky), 150):
        agent.invoke("paint", {"layer": "sky", "strokes": sky[i : i + 150]})
    agent.invoke("look")
    agent.invoke("draw", {"layer": "land", "marks": [{"id": "far-hills", "shape": {"kind": "path", "d":
        "M 0 614 C 120 600 220 606 330 601 C 470 594 560 607 700 603 C 840 598 960 593 1080 603 C 1140 608 1180 605 1200 606 L 1200 760 L 0 760 Z"},
        "fill": "#3a3640", "brush": 22, "wet": True}]})
    agent.invoke("draw", {"layer": "rise", "marks": [{"id": "rise", "shape": {"kind": "path", "d":
        "M 0 700 C 160 660 300 640 430 638 C 560 636 700 652 820 676 C 960 706 1100 722 1200 732 L 1200 900 L 0 900 Z"},
        "fill": "#1f1915", "brush": 34, "angle": -3}]})
    agent.invoke("paint", {"layer": "rise", "strokes": earth_strokes(rng, 90)})
    agent.invoke("look")
    agent.invoke("look", {"region": [0, 560, 1200, 340]})
    oak = oak_strokes(rng)
    for i in range(0, len(oak), 200):
        agent.invoke("paint", {"layer": "oak", "strokes": oak[i : i + 200]})
    figures = []
    for fx, h in [(620, 46), (645, 42)]:
        base = 642 + 0.00018 * (fx - 430) ** 2
        figures.append({"points": [[fx - 1, base, 1.0], [fx, base - h * 0.5, 1.0], [fx + 1, base - h * 0.78, 0.75]], "size": 12, "colour": "#100c0a"})
        figures.append({"points": [[fx + 1, base - h * 0.74, 1.0], [fx + 2, base - h * 0.95, 1.0]], "size": 7, "colour": "#100c0a"})
    agent.invoke("paint", {"layer": "figures", "strokes": figures})
    agent.invoke("draw", {"layer": "moon", "marks": [
        {"id": "moon", "shape": {"kind": "ellipse", "cx": 905, "cy": 300, "rx": 17, "ry": 17}, "fill": "#efe6cc", "brush": 4},
        {"id": "star", "shape": {"kind": "ellipse", "cx": 760, "cy": 250, "rx": 2.5, "ry": 2.5}, "fill": "#f3ecd8", "brush": 3}]})
    agent.invoke("look")
    agent.invoke("look", {"region": [300, 330, 450, 340]})
    agent.invoke("look", {"region": [860, 260, 90, 80], "size": 512})
    # The texture strokes on the rise came out too busy: list them, remove them, paint fewer.
    listed, _ = agent.invoke("inspect", {"layer": "rise"})
    marks = listed.get("marks_bottom_first") or listed.get("marks") or []
    texture = [mark["id"] for mark in marks if mark.get("id") != "rise"]
    agent.invoke("remove-marks", {"ids": texture} if texture else {"layer": "rise"})
    agent.invoke("paint", {"layer": "rise", "strokes": earth_strokes(rng, 60)})
    agent.invoke("look")
    agent.invoke("edit-marks", {"ids": ["moon"], "move_by": [-10, 6]})
    agent.invoke("look", {"region": [840, 240, 140, 120], "size": 512})
    agent.invoke("inspect")
    agent.invoke("edit-layer", {"id": "land", "opacity": 0.9})
    agent.invoke("look")
    # A second look before deciding: nothing changed in between.
    agent.invoke("look")
    agent.invoke("look", {"grid": True})
    agent.invoke("history", {"last": 20})


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default=str(loop.TARGET / "agent-runs" / f"evening-{time.strftime('%Y%m%d-%H%M%S')}"))
    parser.add_argument("--json", action="store_true", help="print the summary as JSON")
    args = parser.parse_args()
    agent = Agent(pathlib.Path(args.out))
    try:
        paint(agent)
    finally:
        summary = agent.close()
    (agent.out / "usage.json").write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary) if args.json else f"{agent.out}\n{cost.format_summary(summary)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
