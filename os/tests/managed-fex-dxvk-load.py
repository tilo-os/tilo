#!/usr/bin/env python3
"""Run genuine PE DLL-load fixtures through Tilo's managed Wine32/Wine64.

Run in the same disposable, offline/capability-dropped ARM container as the
managed Wine smoke test, with the verified guest rootfs/resource mounts. The
helper must provision DXVK; this driver never copies DLLs or sets overrides.
Provide a dedicated writable, empty /run/tilo mount. Prefixes remain there as
capsule/prefix-win32 and capsule/prefix-win64 for inspection. A timed-out prefix
stays at capsule/prefix until the enclosing disposable container is torn down.
Passing proves native DLL loading and export lookup, not GPU rendering.
All three cases are required: PE32 in Win32, PE64 in Win64, then PE32 in
that same Win64 prefix. WoW64 must load the x32 payload from syswow64.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

EXPORTS = {
    "d3d8.dll": "Direct3DCreate8",
    "d3d9.dll": "Direct3DCreate9",
    "d3d10core.dll": "D3D10CoreCreateDevice",
    "d3d11.dll": "D3D11CreateDevice",
    "dxgi.dll": "CreateDXGIFactory1",
}


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def native_load_logged(lines, module_path):
    for line in lines:
        if ':loaddll:' not in line or not re.search(r":\s*native\s*$", line, flags=re.IGNORECASE):
            continue
        logged = re.search(r'Loaded L"([^"]+)"', line)
        # Wine's debugstr_w escapes each backslash; GetModuleFileNameA returns
        # ordinary Windows separators. Decode only the escaping needed here.
        if logged and logged[1].replace("\\\\", "\\").lower() == module_path.lower():
            return True
    return False


def assess(output, bits, prefix, dxvk, prefix_bits=None):
    prefix_bits = bits if prefix_bits is None else prefix_bits
    if (prefix_bits, bits) not in ((32, 32), (64, 64), (64, 32)):
        raise ValueError("unsupported prefix/PE architecture pair")
    lines = [line.strip() for line in output.splitlines()]
    results = []
    for dll, export in EXPORTS.items():
        module_prefix = f"TILO_DXVK_MODULE_{bits} {dll} "
        module_paths = [line[len(module_prefix):] for line in lines if line.startswith(module_prefix)]
        module_path = module_paths[0] if len(module_paths) == 1 else ""
        directories = "syswow64" if prefix_bits == 64 and bits == 32 else "system32"
        match = re.fullmatch(r"c:\\windows\\(" + directories + r")\\" + re.escape(dll),
                             module_path, flags=re.IGNORECASE)
        native = bool(match) and native_load_logged(lines, module_path)
        selected = prefix / "drive_c/windows" / match[1].lower() / dll if match else None
        source = dxvk / f"x{bits}" / dll
        hashes_match = bool(selected and selected.is_file() and source.is_file()
                            and digest(selected) == digest(source))
        results.append({"dll": dll, "export": export, "module_path": module_path,
                        "export_found": f"TILO_DXVK_EXPORT_{bits} {dll} {export}" in lines,
                        "native_load_logged": native, "payload_hash_matches": hashes_match})
    passed = f"TILO_DXVK_LOAD_PASS_{bits}" in lines and all(
        entry["export_found"] and entry["native_load_logged"] and entry["payload_hash_matches"]
        for entry in results)
    return passed, results


def prepare_capsule_root(run_tilo):
    """Require dedicated empty state; never remove or reuse another test's paths."""
    run_tilo = Path(run_tilo)
    if run_tilo.is_symlink() or (run_tilo.exists() and not run_tilo.is_dir()):
        raise ValueError("/run/tilo must be a real, dedicated directory")
    if run_tilo.exists() and any(run_tilo.iterdir()):
        raise ValueError("/run/tilo must be empty; existing state is preserved")
    run_tilo.mkdir(exist_ok=True)
    (run_tilo / "capsule").mkdir()
    return run_tilo / "capsule/prefix"


def create_prefix(prefix, fixture):
    prefix.mkdir()
    (prefix / "drive_c").mkdir()
    (prefix / "dosdevices").mkdir()
    # Production CapsuleStore creates this mapping too. Precreating drive_c
    # without it prevents Wine from supplying its usual new-prefix C: mapping.
    (prefix / "dosdevices/c:").symlink_to("../drive_c", target_is_directory=True)
    shutil.copyfile(fixture, prefix / "drive_c/tilo-dxvk-load.exe")


def run_cases(helper, fixtures, dxvk, output_dir, prefix):
    """Keep one prefix and HOME per Wine mode, including both Win64 cases."""
    results = []
    for prefix_bits, pe_widths in ((32, (32,)), (64, (64, 32))):
        create_prefix(prefix, fixtures[prefix_bits])
        group_results = []
        timed_out = False
        with tempfile.TemporaryDirectory(prefix=f"tilo-dxvk-load-{prefix_bits}-") as temporary:
            home = Path(temporary)
            environment = os.environ | {
                "HOME": str(home), "XDG_RUNTIME_DIR": str(home),
                "WINEPREFIX": str(prefix), "WINEARCH": f"win{prefix_bits}",
                "WINEDEBUG": "+loaddll",
            }
            environment.pop("WINEDLLOVERRIDES", None)
            for bits in pe_widths:
                # Only the test executable changes. The managed helper owns all
                # prefix initialization, DLL provisioning and native overrides.
                shutil.copyfile(fixtures[bits], prefix / "drive_c/tilo-dxvk-load.exe")
                case = f"wine{prefix_bits}-pe{bits}"
                try:
                    process = subprocess.run(
                        [str(helper), f"wine{prefix_bits}", r"C:\tilo-dxvk-load.exe"],
                        env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                        stderr=subprocess.STDOUT, timeout=240, text=True, errors="replace")
                    output, code = process.stdout, process.returncode
                except subprocess.TimeoutExpired as error:
                    output = error.stdout or b""
                    if isinstance(output, bytes):
                        output = output.decode(errors="replace")
                    code, timed_out = None, True
                (output_dir / f"{case}-load.log").write_text(output)
                passed, dlls = assess(output, bits, prefix, dxvk, prefix_bits)
                group_results.append({
                    "case": case, "mode": f"wine{prefix_bits}",
                    "prefix_architecture": f"win{prefix_bits}", "pe_bits": bits,
                    "fixture_sha256": digest(fixtures[bits]),
                    "exit_code": code, "timed_out": timed_out, "dlls": dlls,
                    "passed": code == 0 and not timed_out and passed})
                if timed_out:
                    break  # The disposable container owns orphan cleanup.
            retained = prefix if timed_out else prefix.with_name(f"prefix-win{prefix_bits}")
            if not timed_out:
                if retained.exists() or retained.is_symlink():
                    raise ValueError(f"refusing to replace existing retained prefix: {retained}")
                prefix.rename(retained)
            for result in group_results:
                result["retained_prefix"] = str(retained)
                results.append(result)
                print(json.dumps(result), flush=True)
        if timed_out:
            break
    (output_dir / "result.json").write_text(json.dumps(results, indent=2) + "\n")
    return 0 if len(results) == 3 and all(item["passed"] for item in results) else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", required=True, type=Path)
    parser.add_argument("--exe32", required=True, type=Path)
    parser.add_argument("--exe64", required=True, type=Path)
    parser.add_argument("--dxvk", type=Path, default=Path("/opt/tilo/x86/usr/share/tilo/dxvk"))
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    helper = args.helper.resolve(strict=True)
    fixtures = {32: args.exe32.resolve(strict=True), 64: args.exe64.resolve(strict=True)}
    args.output_dir.mkdir(parents=True, exist_ok=True)
    if any(args.output_dir.iterdir()):
        parser.error("output directory must be empty")
    try:
        prefix = prepare_capsule_root(Path("/run/tilo"))
    except (ValueError, OSError) as error:
        parser.error(str(error))
    return run_cases(helper, fixtures, args.dxvk, args.output_dir, prefix)


if __name__ == "__main__":
    raise SystemExit(main())
