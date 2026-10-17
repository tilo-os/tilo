#!/usr/bin/env python3
"""Host CLI for the installer's shared offline OCI transport checks."""
import importlib.util
from pathlib import Path

source = Path(__file__).with_name("installer-target-transport.py")
spec = importlib.util.spec_from_file_location("tilo_installer_target_transport", source)
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
verify = transport.verify
install = transport.install
main = transport.main

if __name__ == "__main__":
    main()
