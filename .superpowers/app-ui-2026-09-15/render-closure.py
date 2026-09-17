from pathlib import Path
import json
repo = Path(__file__).resolve().parents[2]
source = repo / "docs/superpowers/audits/2026-09-15-all-app-ui-review/closure-register.json"
rows = json.loads(source.read_text())
ids = [finding["id"] for row in rows for finding in row["findings"]]
assert len(ids) == len(set(ids)), "Finding IDs must be unique"
lines = ["# App UI closure register", "", "Baseline review completed before implementation; corrected scale evidence is separately recorded. Counts track recorded findings, including repeated symptoms from separate reviews; they are not a UI quality score.", "", "Assigned work: [Shared](../../plans/2026-09-15-measured-ui-components.md), [Composition](../../plans/2026-09-15-app-composition.md), [Text/geometry](../../plans/2026-09-15-app-text-geometry.md). Task numbers identify implementation ownership; final verification includes actual app screenshots and relevant interactions.", "", "| App | Review | Open findings | Assigned work | Closure |", "|---|---|---|---|---|"]
for row in rows:
    count = sum(finding["status"] != "closed" for finding in row["findings"])
    lines.append(f"| {row['app']} | [{row['baseline_status']}]({row['review']}) | {count} | {row.get('assigned_work', 'Final verification')} | {row['closure']} |")
source.with_suffix(".md").write_text("\n".join(lines) + "\n")
