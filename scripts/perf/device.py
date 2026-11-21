#!/usr/bin/env python3
"""A device reached through tilo-vm, as a check of its latency drives it.

The hardware latency runs (device_latency_bench.py, drm_hardware_checks.py)
speak to a running device slot through `tilo-vm`: a shell on it, its desktop's
control socket, and kernel input into its seat. Nothing here starts or installs
anything on the device.
"""
import json
import os
import subprocess


class Device:
    def __init__(self, options):
        self.options = options
        self.env = dict(os.environ, TILO_VM=options.device)

    def run(self, arguments):
        result = subprocess.run(
            [self.options.tool, *arguments], env=self.env,
            capture_output=True, text=True, timeout=60)
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or result.stdout.strip())
        return result.stdout

    def control(self, arguments):
        return json.loads(self.run(
            ["control", "--slot", self.options.slot, *arguments]))

    def execute(self, arguments):
        return self.run(["slot", "exec", "--slot", self.options.slot,
                         "--timeout", "30", "--", *arguments])
