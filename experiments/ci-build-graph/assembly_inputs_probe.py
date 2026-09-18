#!/usr/bin/env python3
"""Exercise the production RPM-root extractor on temporary recipe variants.

No builds, package resolution, publication or product file changes. A direct-root
set is ONE snapshot input, not a complete artifact compatibility/cache key.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    root = args.root.resolve()
    spec = importlib.util.spec_from_file_location("rpm_roots", root / "os/dev/derive-rpm-package-set.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    recipe = (root / "os/Containerfile").read_text()
    cases = {"baseline": recipe,
             "config_copy_only": recipe + "\nCOPY os/etc/tilo/desktop.toml /tmp/ci-probe-desktop.toml\n",
             "arm64_package_input": recipe + "\n# tilo-rpm-snapshot-package[aarch64]: qos-ci-fixture-package\n"}
    rows = {}
    with tempfile.TemporaryDirectory(prefix="qos-rpm-input-probe-") as temporary:
        path = Path(temporary) / "Containerfile"
        for arch in ["x86_64", "aarch64"]:
            unchanged = {package for source in module.SOURCES if source != "os/Containerfile"
                         for package in module.packages(root / source, arch)}
            rows[arch] = {}
            for name, text in cases.items():
                path.write_text(text)
                roots = unchanged | set(module.packages(path, arch))
                rows[arch][name] = {"recipe_text_sha256": digest(text), "root_count": len(roots),
                                    "roots_sha256": digest("\n".join(sorted(roots)) + "\n")}
            base = rows[arch]["baseline"]
            assert base["recipe_text_sha256"] != rows[arch]["config_copy_only"]["recipe_text_sha256"]
            assert base["roots_sha256"] == rows[arch]["config_copy_only"]["roots_sha256"]
            assert (base["roots_sha256"] == rows[arch]["arm64_package_input"]["roots_sha256"]) == (arch == "x86_64")
    print(json.dumps({"scope": "production direct-RPM-root extraction only; not full snapshot equivalence",
                      "results": rows}, indent=2))


if __name__ == "__main__":
    main()
