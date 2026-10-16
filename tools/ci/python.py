#!/usr/bin/env python3
"""Run a pinned CI helper with stdlib precedence and fresh bytecode lookup.

Invoke with python3 -I: candidate cwd, PYTHONPATH and user site hooks must never
enter interpreter startup. Local helper imports remain available after stdlib.
"""
import runpy
from importlib.machinery import PathFinder, SourceFileLoader
from importlib.util import spec_from_file_location
import sys
import tempfile
from pathlib import Path


class StandardLibraryOnly:
    """Absent platform-specific stdlib modules must not fall back to tools."""
    def __init__(self, trusted_paths):
        self.trusted_paths = list(trusted_paths)

    def find_spec(self, fullname, path=None, target=None):
        if fullname.partition('.')[0] not in sys.stdlib_module_names:
            return None
        spec = PathFinder.find_spec(fullname, self.trusted_paths if path is None else path, target)
        if spec is None:
            raise ModuleNotFoundError('standard-library module unavailable: ' + fullname, name=fullname)
        return spec



class TrustedLocalModules:
    """Load reviewed local imports from exact source files, never package lookup."""
    sources = {
        'ci': 'ci/__init__.py',
        'ci.resolve_builder': 'ci/resolve_builder.py',
        'ci.affected_packages': 'ci/affected_packages.py',
        'ci.workspace_graph': 'ci/workspace_graph.py',
        'ci.runner_labels': 'ci/runner_labels.py',
        'app_release': 'apps/app_release.py',
        'app_package_isa': 'apps/app_package_isa.py',
        'app_channel_manifest': 'apps/app_channel_manifest.py',
        'canonical_app_verifier': 'apps/canonical_app_verifier.py',
        'create_app_receipt': 'apps/create_app_receipt.py',
        'final_app_receipt': 'apps/final_app_receipt.py',
        'assemble_app_collection': 'apps/assemble_app_collection.py',
        'app_packaging_contract': 'app_packaging_contract.py',
        'app_packaging_plan': 'app_packaging_plan.py',
        'app_packaging_qualification': 'app_packaging_qualification.py',
        'app_packaging_product': 'app_packaging_product.py',
        'app_packaging_coordinator': 'app_packaging_coordinator.py',
        'app_packaging_service': 'app_packaging_service.py',
        'accept_app_artifacts': 'accept_app_artifacts.py',
        'assemble_verified_apps': 'assemble_verified_apps.py',
        'canonical_verifier_bundle': 'canonical_verifier_bundle.py',
    }

    def __init__(self, directory):
        self.directory = directory

    def find_spec(self, fullname, path=None, target=None):
        relative = self.sources.get(fullname)
        if relative is None:
            return None
        source = self.directory / relative
        if not source.is_file():
            raise ModuleNotFoundError('pinned local helper unavailable: ' + fullname, name=fullname)
        return spec_from_file_location(
            fullname, source, loader=SourceFileLoader(fullname, str(source)),
            submodule_search_locations=[str(source.parent)] if fullname == 'ci' else None,
        )


def main():
    if not sys.flags.isolated:
        raise SystemExit('ci/python.py requires python3 -I')
    if len(sys.argv) < 2:
        raise SystemExit('usage: python3 -I tools/ci/python.py tools/<helper>.py [args]')
    tools = Path(__file__).resolve().parents[1]
    script = Path(sys.argv[1]).resolve()
    if script.parent not in (tools, tools / 'ci', tools / 'apps') or script.suffix != '.py' or not script.is_file():
        raise SystemExit('CI helper must be a Python source file in tools, tools/ci or tools/apps')
    # -B alone does not prevent reading candidate-supplied __pycache__ files.
    # Use an empty private cache namespace and prohibit writes for this process.
    with tempfile.TemporaryDirectory(prefix='ci-python-') as cache:
        sys.pycache_prefix = cache
        sys.dont_write_bytecode = True
        # Builtin/Frozen importers keep precedence. In particular, subprocess
        # probes msvcrt on Linux: absence must stay ImportError, not tools/msvcrt.
        sys.meta_path.insert(sys.meta_path.index(PathFinder), StandardLibraryOnly(sys.path))
        sys.meta_path.insert(sys.meta_path.index(PathFinder), TrustedLocalModules(tools))
        sys.path.append(str(tools))
        sys.argv = [str(script), *sys.argv[2:]]
        runpy.run_path(str(script), run_name='__main__')


if __name__ == '__main__':
    main()
