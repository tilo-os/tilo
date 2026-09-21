#!/usr/bin/env python3
"""Offline, checksum-before-extraction native build. Never uses a system NTFS ABI."""
import argparse, hashlib, json, os, pathlib, re, subprocess, tarfile
ROOT = pathlib.Path(__file__).resolve().parent
PIN = "d327833ec1d5eb1358b6f2c37139f10a3460944d"
SHA = "b90caed8fade648f119c767bd310419be56a2b0014b16e6b2c68b50bc6c7e9cd"
PIC_BUILD_LAYOUT = "pic-v1"
def build(output, fixture=False):
    archive = ROOT / "vendor/ntfs-3g-source.tar.gz"
    if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA:
        raise RuntimeError("NTFS source checksum mismatch")
    output = pathlib.Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    build_root = output / PIC_BUILD_LAYOUT
    build_root.mkdir(parents=True, exist_ok=True)
    src = build_root / ("ntfs-3g-" + PIN)
    if not src.exists():
        with tarfile.open(archive) as tar:
            # Trusted exact hash is required even on Python without extraction filters.
            tar.extractall(build_root)
    env = os.environ.copy()
    env["PATH"] = "/opt/homebrew/opt/libtool/libexec/gnubin:/opt/homebrew/bin:" + env["PATH"]
    env["LC_ALL"] = "C"
    env["ACLOCAL_PATH"] = "/opt/homebrew/share/aclocal" + (":" + env["ACLOCAL_PATH"] if env.get("ACLOCAL_PATH") else "")
    def run(args):
        subprocess.run(args, cwd=src, env=env, check=True)
    if not (src / ".qos-bootstrap-ok").exists():
        run(["sh", "autogen.sh"])
        (src / ".qos-bootstrap-ok").touch()
    compiler = env.get("QOS_NTFS_CC", "cc")
    archiver = env.get("QOS_NTFS_AR", "ar")
    env["CC"] = compiler; env["AR"] = archiver
    env["RANLIB"] = env.get("QOS_NTFS_RANLIB", "ranlib")
    settings={"cc":compiler,"ar":archiver,"host":env.get("QOS_NTFS_HOST"),"ranlib":env["RANLIB"],"pic":True,"fixture":fixture}
    stamp=src / ".qos-config.json"
    if stamp.exists() and json.loads(stamp.read_text()) != settings:
        raise RuntimeError("native build configuration changed; use a new output directory")
    if not stamp.exists():
        # Libtool's PIC mode keeps Autoconf defaults and caller-provided CFLAGS intact.
        flags = ["--disable-ntfs-3g", "--disable-mount-helper", "--disable-plugins", "--disable-crypto", "--disable-nfconv", "--disable-shared", "--enable-static", "--with-pic"]
        flags += (["--enable-ntfsprogs"] if fixture else ["--disable-ntfsprogs", "--disable-device-default-io-ops"])
        if env.get("QOS_NTFS_HOST"): flags += ["--host="+env["QOS_NTFS_HOST"]]
        run(["./configure", *flags])
        stamp.write_text(json.dumps(settings,sort_keys=True))
    run(["make", "-j2"])
    if not fixture:
        symbols=subprocess.check_output([env.get("QOS_NTFS_NM","nm"),"-g",str(src / "libntfs-3g/.libs/libntfs-3g.a")],env=env,text=True)
        if not re.search(r"(?m)^(?:[0-9a-fA-F]+\s+)?[Tt]\s+_?ntfs_device_mount$",symbols):
            raise RuntimeError("pinned reader library is missing native parser symbols")
        if re.search(r"ntfs_device_(unix|win32|default)_io_ops",symbols):
            raise RuntimeError("forbidden default device adapter in reader library")
    return src
if __name__ == "__main__":
    ap = argparse.ArgumentParser(); ap.add_argument("--output", required=True); ap.add_argument("--fixture-tools", action="store_true")
    args = ap.parse_args(); print(build(args.output, args.fixture_tools))
