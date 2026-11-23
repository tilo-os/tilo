#!/usr/bin/env python3
"""Offline, checksum-before-extraction native build. Never uses a system NTFS ABI."""
import argparse, io, json, os, pathlib, re, subprocess, tarfile
from source_artifact import PIN, SHA, source_bytes
ROOT = pathlib.Path(__file__).resolve().parent
def build(output, fixture=False):
    archive = source_bytes()
    output = pathlib.Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    src = output / ("ntfs-3g-" + PIN)
    if not src.exists():
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            # Trusted exact hash is required even on Python without extraction filters.
            tar.extractall(output)
    env = os.environ.copy()
    env["PATH"] = "/opt/homebrew/opt/libtool/libexec/gnubin:/opt/homebrew/bin:" + env["PATH"]
    env["LC_ALL"] = "C"
    env["ACLOCAL_PATH"] = "/opt/homebrew/share/aclocal" + (":" + env["ACLOCAL_PATH"] if env.get("ACLOCAL_PATH") else "")
    def run(args):
        subprocess.run(args, cwd=src, env=env, check=True)
    if not (src / ".tilo-bootstrap-ok").exists():
        run(["sh", "autogen.sh"])
        (src / ".tilo-bootstrap-ok").touch()
    compiler = env.get("TILO_NTFS_CC", "cc")
    archiver = env.get("TILO_NTFS_AR", "ar")
    # Cargo links this static archive into PIEs and shared test artifacts.
    # Compile every vendored object as position independent so stricter
    # linkers do not reject architecture-specific absolute relocations.
    env["CFLAGS"] = " ".join(filter(None, [env.get("CFLAGS"), "-fPIC"]))
    env["CC"] = compiler; env["AR"] = archiver
    env["RANLIB"] = env.get("TILO_NTFS_RANLIB", "ranlib")
    settings={"cc":compiler,"ar":archiver,"host":env.get("TILO_NTFS_HOST"),"ranlib":env["RANLIB"],"cflags":env["CFLAGS"],"fixture":fixture}
    stamp=src / ".tilo-config.json"
    if stamp.exists() and json.loads(stamp.read_text()) != settings:
        raise RuntimeError("native build configuration changed; use a new output directory")
    if not stamp.exists():
        flags = ["--disable-ntfs-3g", "--disable-mount-helper", "--disable-plugins", "--disable-crypto", "--disable-nfconv", "--disable-shared", "--enable-static"]
        flags += (["--enable-ntfsprogs"] if fixture else ["--disable-ntfsprogs", "--disable-device-default-io-ops"])
        if env.get("TILO_NTFS_HOST"): flags += ["--host="+env["TILO_NTFS_HOST"]]
        run(["./configure", *flags])
        stamp.write_text(json.dumps(settings,sort_keys=True))
    run(["make", "-j2"])
    if not fixture:
        symbols=subprocess.check_output([env.get("TILO_NTFS_NM","nm"),"-g",str(src / "libntfs-3g/.libs/libntfs-3g.a")],env=env,text=True)
        if not re.search(r"(?m)^(?:[0-9a-fA-F]+\s+)?[Tt]\s+_?ntfs_device_mount$",symbols):
            raise RuntimeError("pinned reader library is missing native parser symbols")
        if re.search(r"ntfs_device_(unix|win32|default)_io_ops",symbols):
            raise RuntimeError("forbidden default device adapter in reader library")
    return src
if __name__ == "__main__":
    ap = argparse.ArgumentParser(); ap.add_argument("--output", required=True); ap.add_argument("--fixture-tools", action="store_true")
    args = ap.parse_args(); print(build(args.output, args.fixture_tools))
