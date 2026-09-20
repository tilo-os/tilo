#!/usr/bin/env python3
"""Capture the production Scratchpad app through the shared native runner."""
from native_app_capture import main as capture_main, window_metadata


def main():
    return capture_main('scratchpad')


if __name__ == '__main__':
    raise SystemExit(main())
