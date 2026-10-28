#!/usr/bin/env python3
"""Capture Settings search in the window bar's field; default scenario uses a 2x native window.

No screenshot/backend overrides: all observed startup, search, and document
state must satisfy settings_effects. A busy backend may cause a failed capture;
no initial document changes are discarded as harmless warmup.
"""
from native_app_capture import main as capture_main


def main():
    return capture_main('settings')


if __name__ == '__main__':
    raise SystemExit(main())
