#!/usr/bin/env python3
"""Verify a generated CEF macOS bundle, including after repeated builds."""
from pathlib import Path
import subprocess
import sys

bundle=Path(sys.argv[1]).resolve(strict=True)
broken=[str(path.relative_to(bundle)) for path in bundle.rglob('*') if path.is_symlink() and not path.exists()]
assert not broken, f'Broken framework symlinks: {broken}'
subprocess.run(['codesign','--verify','--deep','--strict',str(bundle)],check=True)
print('Framework symlinks and strict recursive code signature verified')
