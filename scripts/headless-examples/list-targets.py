#!/usr/bin/env python3
"""List every clickable widget the compositor knows about.

Usage:
  ./list-targets.py [SOCKET]

Each entry is `name (x, y, w, h)`. Use the `name` directly with
`click_named` in the other examples — names that contain `[N]` are
indexed siblings (`Container[0]`, `Container[1]`, ...). The bare name
(`Container`) resolves to the first sibling when there's only one;
prefer the indexed form when in doubt.
"""
from _control import call, socket_path_from_argv

resp = call(socket_path_from_argv(), {"op": "list_targets"})
for entry in resp["targets"]:
    name = entry["name"]
    x, y, w, h = entry["rect"]
    print(f"  {name}  ({x:.0f}, {y:.0f}, {w:.0f}, {h:.0f})")
print(f"\n{len(resp['targets'])} targets total")
