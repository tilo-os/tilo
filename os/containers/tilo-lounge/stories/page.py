"""The user stories' results page: every story green or red, in one page.

The page is the work list, not the verdict. A story is green only when it
completed, every step within its budget, and the model playing a first-time
user found nothing to reject; that model is an aid to whoever watches the
recordings, never proof. A red story says the step where it broke, with that
step's screenshots, why, and what the pages and the services said; every
story run shows its videos (the steps as captions), its steps' screenshots
and timings, and the model's verdict. A story nobody drives yet is never
green: it is listed as not yet automated, or as needing credentials, with
what walking it still takes.

usage: python3 -I page.py RUN_DIR [STORY_DIR…]
Writes RUN_DIR/index.html. Each story's results are read from RUN_DIR/<id>/
(story.json, judge.json and the files they name), or from the STORY_DIRs
given, for stories whose results were fetched elsewhere; every link in the
page is relative to RUN_DIR, so the directory can be served or published
as it is.
"""

import html
import json
import os
import pathlib
import sys
from datetime import datetime, timezone

HERE = pathlib.Path(__file__).parent


def esc(text) -> str:
    return html.escape(str(text), quote=True)


def seconds(ms) -> str:
    """A time as a person reads it: milliseconds under a second, else seconds."""
    return f"{round(ms)} ms" if ms < 1000 else f"{ms / 1000:.1f} s"


def verdict_of(story: dict | None, judged: dict | None, entry: dict) -> tuple[str, str]:
    """The story's state, and its one-line reason."""
    if story is None:
        if entry.get("credentials"):
            return "credentials", "Needs credentials: " + entry["credentials"]
        if entry.get("driver"):
            return "missing", "Automated, but this run has no result for it."
        return "pending", "Not yet automated."
    if story.get("untested"):
        return "untested", "Not tested: " + story["untested"]
    if not story["completed"]:
        stopped = next((step for step in story.get("steps", []) if step.get("status") == "failed"), None)
        broke = ({"index": stopped["index"], "title": stopped["title"], "why": stopped.get("why")}
                 if stopped else story.get("brokeAt"))
        where = f"step {broke['index']}, {broke['title']}" if broke else "before its first step"
        return "red", f"Broke at {where}: {(broke or {}).get('why') or story.get('error') or 'no reason given'}"
    if not story["withinBudget"]:
        broke = story.get("brokeAt")
        if broke:
            return "red", f"Over budget at step {broke['index']}, {broke['title']}: {broke['why']}"
        over = [entry for entry in story.get("measures", []) if not entry["ok"]]
        return "red", "Over its web targets: " + "; ".join(
            f"{entry['name']} {entry['value']} (target {entry['target']})" for entry in over)
    if judged and judged.get("applicable") is False:
        return "green", "Completed, every step and cost within its budget. Nothing on screen to watch: this story is measured, not seen."
    if not judged or not judged.get("judged"):
        # No model looked: the steps passed, and only a person watching the
        # recording says more. Never green, never red for that alone.
        return "unjudged", "Every step and cost within its budget. Not judged: " + (
            (judged or {}).get("summary") or "no model looked at the recording.").removeprefix("Not judged: ") + " Watch the recording."
    if not judged.get("accept"):
        return "red", "The judge rejected it: " + judged.get("summary", "")
    return "green", "Completed within budget, and the model found nothing to reject. Watch the recording: a person’s look is the real test."


LABELS = {
    "green": "Green",
    "red": "Red",
    "pending": "Not yet automated",
    "credentials": "Needs credentials",
    "unjudged": "Passed, not judged",
    "missing": "No result",
    "untested": "Not tested",
}


def story_section(entry: dict, base: str | None, story: dict | None, judged: dict | None) -> tuple[str, str]:
    state, reason = verdict_of(story, judged, entry)
    link = (lambda path: esc(f"{base}/{path}")) if base is not None else None
    parts = [f'<section class="story" id="{esc(entry["id"])}" data-state="{state}">']
    parts.append('<header class="story-head">')
    parts.append(f'<span class="pill pill-{state}">{LABELS[state]}</span>')
    parts.append(f'<h2>{esc(entry["title"])}</h2>')
    parts.append("</header>")
    parts.append(f'<p class="intent">{esc(entry["intent"])}</p>')
    parts.append(f'<p class="reason reason-{state}">{esc(reason)}</p>')

    if story is None:
        parts.append('<ol class="planned">' + "".join(f"<li>{esc(step)}</li>" for step in entry.get("steps", [])) + "</ol>")
        if entry.get("needs"):
            parts.append(f'<p class="needs"><b>Still needs:</b> {esc(entry["needs"])}</p>')
        parts.append("</section>")
        return state, "\n".join(parts)

    broke = story.get("brokeAt")
    if broke:
        step = next(item for item in story["steps"] if item["index"] == broke["index"])
        parts.append('<div class="broke">')
        parts.append(f'<h3>Where it broke: step {step["index"]}, {esc(step["title"])}</h3>')
        parts.append('<div class="shots">' + "".join(
            f'<figure><a href="{link(shot["file"])}"><img src="{link(shot["file"])}" alt="{esc(shot["person"])}’s screen as step {step["index"]} ended" loading="lazy"></a>'
            f'<figcaption>{esc(shot["person"])}</figcaption></figure>' for shot in step.get("shots", [])) + "</div>")
        if step.get("complaints"):
            parts.append('<pre class="said">' + esc("\n".join(step["complaints"])) + "</pre>")
        evidence = step.get("evidence") or {}
        if evidence:
            parts.append('<p class="evidence">' + " · ".join(
                f'<a href="{link(path)}">{esc(name)}</a>' for name, path in
                [("the pages’ console", evidence.get("console")), ("the services’ log", evidence.get("server"))] if path) + "</p>")
        parts.append("</div>")

    if story.get("videos"):
        parts.append('<div class="videos">' + "".join(
            f'<figure><video src="{link(video["file"])}" controls preload="metadata" muted>'
            + (f'<track kind="captions" srclang="en" label="Steps" src="{link(video["captions"])}" default>' if video.get("captions") else "")
            + '</video>'
            f'<figcaption>{esc(video["person"])}’s screen</figcaption></figure>' for video in story["videos"]) + "</div>")

    parts.append('<div class="table-wrap"><table class="steps"><thead><tr><th>Step</th><th class="num">Took</th><th class="num">Budget</th><th>Result</th></tr></thead><tbody>')
    for step in story["steps"]:
        cls = {"ok": "ok", "over budget": "over", "failed": "failed"}[step["status"]]
        why = f'<div class="why">{esc(step["why"])}</div>' if step.get("why") else ""
        parts.append(f'<tr class="{cls}"><td>{step["index"]}. {esc(step["title"])}{why}</td>'
                     f'<td class="num">{seconds(step["ms"])}</td><td class="num">{seconds(step["budgetMs"])}</td>'
                     f'<td><span class="dot dot-{cls}"></span>{esc(step["status"])}</td></tr>')
    reached = len(story["steps"])
    for index, title in enumerate(entry.get("steps", [])[reached:], start=reached + 1):
        parts.append(f'<tr class="unreached"><td>{index}. {esc(title)}</td><td class="num">–</td><td class="num">–</td><td>not reached</td></tr>')
    parts.append("</tbody></table></div>")

    if story.get("measures"):
        parts.append('<h3>Costs against their targets</h3><div class="table-wrap"><table class="steps"><thead><tr><th>Measure</th><th class="num">Measured</th><th class="num">Target</th><th>Who</th></tr></thead><tbody>')
        for entry in story["measures"]:
            cls = "ok" if entry["ok"] else "over"
            value = "–" if entry["value"] is None else entry["value"]
            parts.append(f'<tr class="{cls}"><td><span class="dot dot-{cls}"></span>{esc(entry["name"])}</td>'
                         f'<td class="num">{esc(value)}</td><td class="num">{esc(entry["target"])}</td><td>{esc(entry.get("detail", ""))}</td></tr>')
        parts.append("</tbody></table></div>")

    if story.get("sizes"):
        parts.append(scale_table(story))
    if story.get("logs"):
        parts.append('<p class="evidence">' + " · ".join(
            f'<a href="{link(entry["file"])}">{esc(entry["name"])}</a>' for entry in story["logs"]) + "</p>")

    if story.get("timings"):
        parts.append('<dl class="timings">' + "".join(
            f"<div><dt>{esc(name)}</dt><dd>{seconds(ms)}</dd></div>" for name, ms in story["timings"].items()) + "</dl>")

    if judged and judged.get("applicable") is not False:
        parts.append('<div class="judge">')
        model = f' <span class="model">({esc(judged.get("model", ""))})</span>' if judged.get("model") else ""
        word = "accepts" if judged.get("accept") else ("rejects" if judged.get("judged") else "did not judge")
        parts.append(f"<h3>A model playing a first-time user {word} it{model}</h3>")
        parts.append('<p class="aid">An aid for whoever watches the recording, never proof that the story is good.</p>')
        parts.append(f"<p>{esc(judged.get('summary', ''))}</p>")
        if judged.get("problems"):
            parts.append("<ul>" + "".join(
                f'<li class="sev-{esc(problem["severity"])}"><b>{esc(problem["severity"])} · {esc(problem["kind"])}</b>'
                f'{" · step " + str(problem["step"]) if problem.get("step") else ""}: {esc(problem["what"])}</li>'
                for problem in judged["problems"]) + "</ul>")
        parts.append("</div>")

    parts.append('<details class="all-shots"><summary>Every step’s screenshots</summary>')
    for step in story["steps"]:
        parts.append(f'<h4>{step["index"]}. {esc(step["title"])}</h4><div class="shots">' + "".join(
            f'<figure><a href="{link(shot["file"])}"><img src="{link(shot["file"])}" alt="{esc(shot["person"])}’s screen as step {step["index"]} ended" loading="lazy"></a>'
            f'<figcaption>{esc(shot["person"])}</figcaption></figure>' for shot in step.get("shots", [])) + "</div>")
    parts.append("</details>")
    parts.append(f'<p class="ran">Ran {esc(story["at"])} in {story["seconds"]} s.</p>')
    parts.append("</section>")
    return state, "\n".join(parts)


def scale_table(story: dict) -> str:
    """A story run at several workspace sizes: each step's time at each, and
    how much it grew against how much the workspace grew."""
    sizes = story["sizes"]
    head = "".join(f'<th class="num">{esc(size["label"])}</th>' for size in sizes)
    rows = []
    for growth in story.get("growth", []):
        cls = "ok" if growth["ok"] else "over"
        cells = "".join(f'<td class="num">{seconds(ms) if ms is not None else "–"}</td>' for ms in growth["ms"])
        rows.append(f'<tr class="{cls}"><td><span class="dot dot-{cls}"></span>{esc(growth["title"])}</td>{cells}'
                    f'<td class="num">{growth["ratio"] if growth["ratio"] is not None else "–"}×</td>'
                    f'<td class="num">{growth["sizeRatio"]}×</td><td class="num">{growth["allowed"]}×</td></tr>')
    return ('<h3>Across workspace sizes</h3><div class="table-wrap"><table class="steps"><thead><tr><th>Step</th>'
            f'{head}<th class="num">Time grew</th><th class="num">Workspace grew</th><th class="num">Allowed</th></tr></thead><tbody>'
            + "".join(rows) + "</tbody></table></div>")


STYLE = """
/* Layout: one column of story sheets under a status tally, each sheet a table of its steps. */
:root {
  --bg: #f6f7f9; --sheet: #ffffff; --fg: #1b1f27; --muted: #5d6675; --line: #dde1e8;
  --green: #17803d; --green-bg: #e3f4e8; --red: #b4232a; --red-bg: #fbe6e6;
  --amber: #8a5a00; --amber-bg: #fbf0d9; --grey-bg: #eceef2; --accent: #3355cc;
  --font-body: "IBM Plex Sans", system-ui, sans-serif; --font-head: "Space Grotesk", system-ui, sans-serif;
  --font-mono: "IBM Plex Mono", ui-monospace, monospace;
}
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) {
  --bg: #12151b; --sheet: #1a1e26; --fg: #e6e9ef; --muted: #9aa3b2; --line: #2c323d;
  --green: #4fc77a; --green-bg: #173322; --red: #ff7b7f; --red-bg: #3a1a1c;
  --amber: #e2b250; --amber-bg: #352a12; --grey-bg: #262b35; --accent: #8ea6ff; color-scheme: dark; } }
:root[data-theme="dark"] {
  --bg: #12151b; --sheet: #1a1e26; --fg: #e6e9ef; --muted: #9aa3b2; --line: #2c323d;
  --green: #4fc77a; --green-bg: #173322; --red: #ff7b7f; --red-bg: #3a1a1c;
  --amber: #e2b250; --amber-bg: #352a12; --grey-bg: #262b35; --accent: #8ea6ff; color-scheme: dark; }
body { background: var(--bg); color: var(--fg); font: 15px/1.5 var(--font-body); margin: 0; }
main { max-width: 1080px; margin: 0 auto; padding-block: 32px 64px;
  padding-left: max(16px, env(safe-area-inset-left, 0px)); padding-right: max(16px, env(safe-area-inset-right, 0px));
  display: grid; gap: 24px; }
h1, h2, h3 { font-family: var(--font-head); text-wrap: balance; margin: 0; }
h1 { font-size: 1.9rem; } h2 { font-size: 1.3rem; } h3 { font-size: 1.05rem; } h4 { font-size: .9rem; margin: 12px 0 6px; color: var(--muted); }
a { color: var(--accent); }
.lede { color: var(--muted); margin: 6px 0 0; max-width: 70ch; }
.tally { display: flex; flex-wrap: wrap; gap: 8px; }
.tally a { text-decoration: none; }
.story { background: var(--sheet); border: 1px solid var(--line); border-radius: 10px; padding: 20px; display: grid; gap: 14px; min-width: 0; }
.story[data-state="red"] { border-left: 4px solid var(--red); }
.story[data-state="green"] { border-left: 4px solid var(--green); }
.story[data-state="unjudged"] { border-left: 4px solid var(--amber); }
.story-head { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
.pill { font: 600 .72rem/1 var(--font-body); letter-spacing: .06em; text-transform: uppercase; padding: 5px 9px; border-radius: 999px; }
.pill-green { background: var(--green-bg); color: var(--green); }
.pill-red { background: var(--red-bg); color: var(--red); }
.pill-credentials, .pill-unjudged { background: var(--amber-bg); color: var(--amber); }
.pill-pending, .pill-missing, .pill-untested { background: var(--grey-bg); color: var(--muted); }
.intent { margin: 0; max-width: 75ch; }
.reason { margin: 0; font-weight: 600; }
.reason-red { color: var(--red); } .reason-green { color: var(--green); } .reason-credentials, .reason-unjudged { color: var(--amber); }
.reason-pending, .reason-missing, .reason-untested { color: var(--muted); font-weight: 500; }
.planned { margin: 0; padding-left: 1.4em; color: var(--muted); }
.needs { margin: 0; color: var(--muted); }
.broke { display: grid; gap: 10px; padding: 14px; border-radius: 8px; background: var(--red-bg); }
.shots, .videos { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 300px), 1fr)); gap: 10px; }
figure { margin: 0; display: grid; gap: 4px; min-width: 0; }
figure img, figure video { width: 100%; max-width: 100%; border: 1px solid var(--line); border-radius: 6px; background: var(--grey-bg); }
figcaption { font-size: .8rem; color: var(--muted); }
.said { font: .78rem/1.45 var(--font-mono); white-space: pre-wrap; overflow-x: auto; margin: 0; padding: 10px; background: var(--sheet); border-radius: 6px; }
.evidence { margin: 0; font-size: .9rem; }
.table-wrap { overflow-x: auto; }
table.steps { width: 100%; border-collapse: collapse; font-size: .92rem; }
.steps th { text-align: left; font-weight: 600; color: var(--muted); font-size: .75rem; letter-spacing: .05em; text-transform: uppercase; border-bottom: 1px solid var(--line); padding: 6px 8px; }
.steps td { border-bottom: 1px solid var(--line); padding: 7px 8px; vertical-align: top; }
.num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; font-family: var(--font-mono); font-size: .85rem; }
.steps tr.failed td, .steps tr.over td { background: var(--red-bg); }
.steps tr.unreached td { color: var(--muted); }
.why { color: var(--red); font-size: .85rem; margin-top: 2px; overflow-wrap: anywhere; }
.dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; margin-right: 6px; vertical-align: middle; }
.dot-ok { background: var(--green); } .dot-over, .dot-failed { background: var(--red); }
.timings { display: flex; flex-wrap: wrap; gap: 8px 20px; margin: 0; }
.timings div { display: grid; }
.timings dt { font-size: .75rem; color: var(--muted); }
.timings dd { margin: 0; font: 600 1rem var(--font-mono); font-variant-numeric: tabular-nums; }
.judge { border-top: 1px solid var(--line); padding-top: 12px; display: grid; gap: 6px; }
.judge p { margin: 0; max-width: 75ch; }
.judge .aid { color: var(--muted); font-size: 0.9em; }
.judge ul { margin: 0; padding-left: 1.2em; display: grid; gap: 4px; }
.model { font: 400 .8rem var(--font-mono); color: var(--muted); }
.sev-blocker b, .sev-major b { color: var(--red); } .sev-minor b { color: var(--amber); }
details summary { cursor: pointer; color: var(--accent); }
.ran { margin: 0; font-size: .8rem; color: var(--muted); }
:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
"""


def build(run_dir: pathlib.Path, extra: list[pathlib.Path]) -> str:
    stories = json.loads((HERE / "stories.json").read_text())["stories"]
    found = {path.name: path for path in extra}
    sections, states = [], []
    for entry in stories:
        where = found.get(entry["id"], run_dir / entry["id"])
        story = judged = None
        base = None
        if (where / "story.json").is_file():
            story = json.loads((where / "story.json").read_text())
            if (where / "judge.json").is_file():
                judged = json.loads((where / "judge.json").read_text())
            base = os.path.relpath(where, run_dir)
        state, section = story_section(entry, base, story, judged)
        states.append((state, entry))
        sections.append(section)
    # Red first, then green, then what is not automated yet.
    rank = {"red": 0, "untested": 1, "missing": 2, "unjudged": 3, "green": 4, "credentials": 5, "pending": 6}
    ordered = [section for _, section in sorted(zip([rank[s] for s, _ in states], sections), key=lambda pair: pair[0])]
    counts = {state: sum(1 for s, _ in states if s == state) for state in rank}
    tally = "".join(f'<span class="pill pill-{state}">{counts[state]} {LABELS[state].lower()}</span>'
                    for state in ["green", "unjudged", "red", "untested", "missing", "credentials", "pending"] if counts[state])
    when = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    revision = os.environ.get("T_COMMIT") or os.environ.get("SOURCE_REVISION") or ""
    return f"""<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>Tilo User Stories</title>
<meta name="claude:viewport-fit-sides" content="cover">
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;600&family=IBM+Plex+Sans:wght@400;500;600&family=Space+Grotesk:wght@500;600&display=swap">
<style>{STYLE}</style>
<main>
<header>
<h1>Tilo user stories</h1>
<p class="lede">What people set out to do with Tilo, walked end to end in real browsers on a fresh stack, each step timed against its budget, and the recording shown to a model playing a first-time user who has never seen the code. This page is the work list, not the verdict: a story is green only when it finished, every step within its budget, and the model found nothing to reject, and even green is only as good as the recording looks to a person watching it. A story nobody drives yet is never green. Run {esc(when)}{' on ' + esc(revision[:12]) if revision else ''}.</p>
</header>
<nav class="tally">{tally}</nav>
{''.join(ordered)}
</main>
"""


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: python3 -I page.py RUN_DIR [STORY_DIR…]", file=sys.stderr)
        return 2
    run_dir = pathlib.Path(sys.argv[1])
    extra = [pathlib.Path(path) for path in sys.argv[2:]]
    (run_dir / "index.html").write_text(build(run_dir, extra))
    # Every story's timings.json in one, for machines comparing runs.
    found = {path.name: path for path in extra}
    stories = json.loads((HERE / "stories.json").read_text())["stories"]
    combined = []
    for entry in stories:
        timings = found.get(entry["id"], run_dir / entry["id"]) / "timings.json"
        if timings.is_file():
            combined.append(json.loads(timings.read_text()))
    (run_dir / "timings.json").write_text(json.dumps({"stories": combined}, indent=2))
    print(f"the results page: {run_dir / 'index.html'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
