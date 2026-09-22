#!/usr/bin/env python3
"""Capture native Files scrolling through its production directory route."""
from native_app_capture import main as capture_main


def main():
    return capture_main('file-explorer')


if __name__ == '__main__':
    raise SystemExit(main())
