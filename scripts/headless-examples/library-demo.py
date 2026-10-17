#!/usr/bin/env python3
"""Demonstrates the TiloClient library.

Same flow as full-loop.py, but using the higher-level TiloClient API
instead of hand-rolling the socket protocol. Useful as a template when
building AI-driven automation, regression suites, or interactive probes.

Run it (no args needed):
  ./library-demo.py
"""
from tilo_client import TiloClient

with TiloClient.spawn_headless() as tilo:
    # Discover what's clickable:
    targets = tilo.list_targets()
    print(f"compositor has {len(targets)} addressable widgets")
    for t in targets[:5]:
        print(f"  {t.name}  centre={t.center}")

    # Drive the UI:
    tilo.modifiers(logo=True)
    tilo.key("s")               # Super+S → strip switcher
    tilo.modifiers()
    tilo.click("strip_switcher")
    tilo.key("Escape")          # close the switcher again
    tilo.modifiers(logo=True)
    tilo.key("Space")           # Super+Space → machine-wide search
    tilo.modifiers()            # release modifiers
    tilo.type_text("Files")     # type into palette

    # Capture state:
    png_path = tilo.snapshot("/tmp/library-demo-palette.png")
    print(f"wrote {png_path}")

    # Read live state:
    state = tilo.state_dump()
    print(f"chrome.overlays = {state['chrome']['overlays']}")
    workspaces = state['workspace_tree'].get('workspaces', [])
    print(f"{len(workspaces)} workspace(s) in the live tree")

    # Tear down (Escape closes the palette):
    tilo.key("Escape")
    tilo.snapshot("/tmp/library-demo-final.png")
    print("done — /tmp/library-demo-{palette,final}.png written")
