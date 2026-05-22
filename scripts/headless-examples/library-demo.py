#!/usr/bin/env python3
"""Demonstrates the NiriosClient library.

Same flow as full-loop.py, but using the higher-level NiriosClient API
instead of hand-rolling the socket protocol. Useful as a template when
building AI-driven automation, regression suites, or interactive probes.

Run it (no args needed):
  ./library-demo.py
"""
from nirios_client import NiriosClient

with NiriosClient.spawn_headless() as nirios:
    # Discover what's clickable:
    targets = nirios.list_targets()
    print(f"compositor has {len(targets)} addressable widgets")
    for t in targets[:5]:
        print(f"  {t.name}  centre={t.center}")

    # Drive the UI:
    nirios.click("wheel_disc")
    nirios.modifiers(ctrl=True)
    nirios.key("k")               # Ctrl+K → palette
    nirios.modifiers()            # release modifiers
    nirios.type_text("Files")     # type into palette

    # Capture state:
    png_path = nirios.snapshot("/tmp/library-demo-palette.png")
    print(f"wrote {png_path}")

    # Read live state:
    state = nirios.state_dump()
    print(f"chrome.overlays = {state['chrome']['overlays']}")
    workspaces = state['workspace_tree'].get('workspaces', [])
    print(f"{len(workspaces)} workspace(s) in the live tree")

    # Tear down (Escape closes the palette):
    nirios.key("Escape")
    nirios.snapshot("/tmp/library-demo-final.png")
    print("done — /tmp/library-demo-{palette,final}.png written")
