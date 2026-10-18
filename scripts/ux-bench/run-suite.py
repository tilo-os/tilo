#!/usr/bin/env python3
"""Run isolated desktop UX journeys and retain an aggregate review report."""
import argparse
import copy
import html
import json
from pathlib import Path
import subprocess
import sys


def read_result(out, returncode, log):
    try:
        report = json.loads((out / "report.json").read_text())
        if not (out / "index.html").is_file():
            raise ValueError("HTML report missing")
        if returncode not in (0, 1) or report.get("passed") is not (returncode == 0):
            raise ValueError(f"exit {returncode} disagrees with report or is an execution error")
        if report["passed"] and (not report.get("frames") or report.get("failures")):
            raise ValueError("passing run lacks checkpoint evidence or contains failures")
        for frame in report.get("frames", []):
            for evidence in (frame["screenshot"], frame["name"] + ".inspection.json", frame["name"] + ".resources.json"):
                if not (out / evidence).is_file():
                    raise ValueError(f"missing checkpoint evidence {evidence}")
        return {"status": "pass" if report["passed"] else "fail",
                "failures": report["failures"], "counts": report["counts"]}
    except (OSError, ValueError, KeyError, TypeError) as error:
        return {"status": "error", "failures": [str(error), log[-4000:]], "counts": {}}


# Authored from the actual first H in Home and first A in the shortcut hint.
# These fixed glyph interiors exclude icons/borders and are reviewed in the
# task3 matrix screenshots. Other variants preserve the original pixel probes.
# 2026-10-18: the launchpad no longer centres beside a workspace wheel
# (retired for the strip switcher); it centres in the whole display. The hint
# is drawn from the first card's left edge, so every probe moved with it:
# 1280 wide, the 900 px grid went from x 316 to 190 (-126); 800 wide (compact),
# the grid grew from 500 to 752 px and went from x 276 to 24 (-252). Rows are
# unchanged (the column count and card height are the same).
VARIANT_PIXEL_PROBES = {
    "compact": {"App shortcut hint": ([27, 591], [24, 590])},
    "large-text": {
        "Workspace name": ([139, 17], [150, 35]),
        "App shortcut hint": ([196, 598], [190, 598]),
    },
}


def variants(scenario, matrix):
    yield "baseline", scenario
    if not matrix:
        return
    for name, overrides in [
        ("compact", {"viewport": [800, 700]}),
        ("light", {"theme": "light"}),
        ("high-contrast", {"theme": "high-contrast"}),
        ("retina", {"scale": 2}),
        ("large-text", {"text_scale": 2}),
    ]:
        case = copy.deepcopy(scenario)
        case.update(overrides)
        for step in case["steps"]:
            for check in step.get("checks", []):
                if check["kind"] == "contrast":
                    for label, (foreground, background) in VARIANT_PIXEL_PROBES.get(name, {}).items():
                        if check["label"].startswith(label):
                            check.update(foreground=foreground, background=background)
                    check["label"] = check["label"].split(", 1280")[0] + " — " + name

        # PixelDelta is in physical pixels: hold logical gesture distance fixed.
        if name == "retina":
            for step in case["steps"]:
                if step["op"] == "wheel" and step["unit"] == "physical_pixels":
                    ratio = 2 / scenario.get("scale", 1)
                    step["delta"] = [value * ratio for value in step["delta"]]
        yield name, case


def write_summary(out, results, gaps):
    summary = {"schema_version": 1, "passed": not gaps and all(r["status"] == "pass" for r in results),
               "runs": results, "coverage_gaps": gaps}
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    rows = []
    for result in results:
        counts = result["counts"]
        failures = "".join(f"<li>{html.escape(f)}</li>" for f in result["failures"])
        rows.append(f'<section><h2><span class="{result["status"]}">{result["status"].upper()}</span> '
                    f'<a href="{result["directory"]}/index.html">{html.escape(result["name"])}</a></h2>'
                    f'<p>{counts.get("clicks", "?")} clicks · {counts.get("keys", "?")} keys · '
                    f'{counts.get("wheel_events", "?")} wheel events</p><ul>{failures}</ul></section>')
    gap_html = "".join(f"<li>{html.escape(g)}</li>" for g in gaps)
    document = '''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Tilo UX bench results</title><style>body{font:16px/1.5 system-ui;color:#202530;background:#f5f6f8;margin:0}main{max-width:1100px;margin:auto;padding:32px}section{background:white;padding:20px 28px;border:1px solid #d1d7e0;border-radius:12px;margin:20px 0}h1{font-size:36px}h2{font-size:20px}li{overflow-wrap:anywhere;margin:8px 0}.fail,.error{color:#a21532}.pass{color:#126539}a{color:#154f9d}</style><main><h1>Desktop UX bench</h1>
<p>Measured user journeys, layout, contrast, and scrolling. Open a run for screenshots, target overlays, expected/observed measurements, and its input trace.</p>'''
    document += f'<p>{sum(r["status"] == "pass" for r in results)} passed · {sum(r["status"] == "fail" for r in results)} UX failures · {sum(r["status"] == "error" for r in results)} execution errors</p>'
    document += "".join(rows) + f'<section><h2>Coverage limits</h2><ul>{gap_html}<li>Native hardware delivery and perceived ease require live review. Passing numbers alone do not prove a pleasant experience.</li></ul></section></main></html>'
    (out / "index.html").write_text(document)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/ux-bench"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--matrix", action="store_true", help="also exercise compact/light/high contrast/Retina/200%% text")
    parser.add_argument("scenarios", nargs="*", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"build the benchmark first: {binary}")
    if args.out.exists():
        parser.error("output directory already exists; choose a new one to preserve evidence")
    scenarios = args.scenarios or sorted(Path(__file__).parent.glob("*.json"))
    if not scenarios:
        parser.error("no scenarios")
    args.out.mkdir(parents=True)
    out = args.out.resolve()
    (out / "scenarios").mkdir()
    results, gaps = [], []
    for source in scenarios:
        scenario = json.loads(source.read_text())
        cases = list(variants(scenario, args.matrix))
        for variant, case in cases:
            directory = f"{len(results):03d}-{source.stem}-{variant}"
            scenario_file = out / "scenarios" / f"{directory}.json"
            scenario_file.write_text(json.dumps(case, indent=2) + "\n")
            try:
                process = subprocess.run([str(binary), "--scenario", str(scenario_file), "--out", str(out / directory)],
                                         capture_output=True, text=True, timeout=120)
                result = read_result(out / directory, process.returncode, process.stdout + process.stderr)
                (out / f"{directory}.log").write_text(process.stdout + process.stderr)
            except subprocess.TimeoutExpired:
                result = {"status": "error", "failures": ["run exceeded 120 seconds"], "counts": {}}
            result.update(name=f"{case['name']} — {variant}", directory=directory)
            results.append(result)
            print(f"{result['status'].upper()}: {result['name']}", flush=True)
            write_summary(out, results, gaps)
    summary = write_summary(out, results, gaps)
    print(out / "index.html")
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
