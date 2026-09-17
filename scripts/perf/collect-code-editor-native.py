#!/usr/bin/env python3
"""Capture the production Code Editor app through the shared native runner."""
from native_app_capture import main as capture_main


def main():
    return capture_main('code-editor')


if __name__ == '__main__':
    raise SystemExit(main())
