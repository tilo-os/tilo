#!/usr/bin/env python3
"""Execute pinned Arity through an isolated production compositor and launcher.

Requires already built binaries and tesseract. Never builds, installs, grants
permissions, or contacts a real desktop. The catalogue is a developer fixture,
not proof of signed Tilo package admission.
"""
import argparse
import ctypes
import ctypes.util
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time
import tomllib

from qualify_apk import digest, pinned, verify_runtime
from execution_host import add_architecture, native_architecture

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts/headless-examples"))
from _control import call, require_ok

APK_SHA = "1928e65ced8cbe78be2ff3cb4c321e9e75d138e8e30ea1368fbb772a86827d1d"
SIGNER_SHA = "830e955c73d02aa077c270fcb3e089a556976afe8e9ab3f9b39c4616fdbd9a6d"


def ocr_result(path, rect, height=64, scale=1.0, mode=11):
    """Read only the APK result/expression area, retaining the original PNG."""
    tess_name = ctypes.util.find_library("tesseract")
    lept_name = ctypes.util.find_library("leptonica") or ctypes.util.find_library("lept")
    if not tess_name or not lept_name:
        raise RuntimeError("Tesseract and Leptonica shared libraries are required")
    tess, lept = ctypes.CDLL(tess_name), ctypes.CDLL(lept_name)
    for library, name, arguments, result in (
        (lept, "pixRead", [ctypes.c_char_p], ctypes.c_void_p),
        (lept, "pixDestroy", [ctypes.POINTER(ctypes.c_void_p)], None),
        (lept, "boxCreate", [ctypes.c_int] * 4, ctypes.c_void_p),
        (lept, "boxDestroy", [ctypes.POINTER(ctypes.c_void_p)], None),
        (lept, "pixClipRectangle", [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p], ctypes.c_void_p),
        (lept, "pixAddBorderGeneral", [ctypes.c_void_p] + [ctypes.c_int] * 4 + [ctypes.c_uint32], ctypes.c_void_p),
        (lept, "pixScale", [ctypes.c_void_p, ctypes.c_float, ctypes.c_float], ctypes.c_void_p),
        (tess, "TessBaseAPICreate", [], ctypes.c_void_p),
        (tess, "TessBaseAPIInit3", [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p], ctypes.c_int),
        (tess, "TessBaseAPISetImage2", [ctypes.c_void_p, ctypes.c_void_p], None),
        (tess, "TessBaseAPISetPageSegMode", [ctypes.c_void_p, ctypes.c_int], None),
        (tess, "TessBaseAPISetRectangle", [ctypes.c_void_p] + [ctypes.c_int] * 4, None),
        (tess, "TessBaseAPIGetUTF8Text", [ctypes.c_void_p], ctypes.c_void_p),
        (tess, "TessDeleteText", [ctypes.c_void_p], None),
        (tess, "TessBaseAPIDelete", [ctypes.c_void_p], None),
    ):
        function = getattr(library, name)
        function.argtypes, function.restype = arguments, result
    image = ctypes.c_void_p(lept.pixRead(os.fsencode(path)))
    api = tess.TessBaseAPICreate()
    text = None
    box = ctypes.c_void_p(lept.boxCreate(round(rect["x"]), round(rect["y"]), min(round(rect["w"]), 400), height))
    cropped = ctypes.c_void_p(lept.pixClipRectangle(image, box, None))
    padded = ctypes.c_void_p(lept.pixAddBorderGeneral(cropped, 20, 20, 20, 20, 0))
    scaled = ctypes.c_void_p(lept.pixScale(padded, scale, scale))
    try:
        if not image or not scaled or not api or tess.TessBaseAPIInit3(api, None, b"eng"):
            raise RuntimeError("OCR image or English model unavailable")
        tess.TessBaseAPISetImage2(api, scaled)
        tess.TessBaseAPISetPageSegMode(api, mode)
        text = tess.TessBaseAPIGetUTF8Text(api)
        if not text:
            raise RuntimeError("OCR returned no result")
        return ctypes.string_at(text).decode("utf-8")
    finally:
        if text:
            tess.TessDeleteText(text)
        if api:
            tess.TessBaseAPIDelete(api)
        if image:
            lept.pixDestroy(ctypes.byref(image))
        if padded:
            lept.pixDestroy(ctypes.byref(padded))
        if scaled:
            lept.pixDestroy(ctypes.byref(scaled))
        if cropped:
            lept.pixDestroy(ctypes.byref(cropped))
        if box:
            lept.boxDestroy(ctypes.byref(box))


def wait_for(check, description, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.2)
    raise RuntimeError("timeout: " + description)


class Journey:
    def __init__(self, socket, output, package, profile):
        self.socket, self.output, self.package, self.profile = socket, output, package, profile
        self.evidence = []

    def request(self, op, **fields):
        response = call(str(self.socket), {"op": op, **fields})
        if "err" in response:
            raise RuntimeError(str(response["err"]))
        return response

    def state(self):
        return self.request("state_dump")["state"]

    def workspace(self):
        tree = self.state()["workspace_tree"]
        return next(w for w in tree["workspaces"] if w["id"] == tree["active"])

    def tabs(self):
        return [t for s in self.workspace()["strips"]
                for g in s["window_groups"] for t in g["tabs"]]

    def invoke(self, provider, action):
        answer = self.request("actions_invoke", provider=provider, id=action)["answer"]
        update = answer["receipt"]["update"]
        if update["state"] != "succeeded":
            raise RuntimeError("action failed: " + json.dumps(update))

    def launch(self):
        def visible():
            actions = self.request("actions_enumerate", query="")["answer"]["actions"]
            return any(a.get("id") == self.package for a in actions)
        wait_for(visible, "application catalogue readiness")
        self.invoke("applications", self.package)
        return wait_for(self.frame, "APK mapped and presented")

    def frame(self):
        ids = {t["tile"]["Foreign"]["surface_id"] for t in self.tabs()
               if "Foreign" in t["tile"]}
        frames = self.request("foreign_frames")["foreign_frames"]["frames"]
        return next((f for f in frames if f["surface_id"] in ids
                     and f.get("presented") and f.get("frame_seq", 0) > 0
                     and not f.get("last_error")), None)

    def snapshot(self, name):
        path = self.output / (name + ".png")
        require_ok(self.request("snapshot", path=str(path)))
        rect = wait_for(self.frame, "OCR surface")["rect"]
        text = ocr_result(path, rect, height=42, mode=7)
        expression_rect = {**rect, "y": rect["y"] + 43}
        text += ocr_result(path, expression_rect, height=20, scale=2.0, mode=7)
        (self.output / (name + ".txt")).write_text(text)
        return " ".join(re.sub(r"\s+", "", line) for line in text.splitlines())

    def result(self, name, expression, answer):
        def shown():
            text = self.snapshot(name)
            return expression in text and re.search(r"(?<!\d)" + answer + r"(?!\d)", text)
        wait_for(shown, "APK rendered " + expression + " = " + answer, timeout=30)
        self.evidence.append({"step": name, "expression": expression, "answer": answer,
                              "png_sha256": digest(self.output / (name + ".png")),
                              "frame": self.frame(), "room": self.workspace()["room"]})

    def type_expression(self, expression):
        frame = wait_for(self.frame, "input surface")
        x, y = frame["rect"]["x"], frame["rect"]["y"]
        require_ok(self.request("click_at", x=x + 29, y=y + 53))
        require_ok(self.request("type", text=expression))

    def close(self):
        self.invoke("actions", "tab.close")
        wait_for(lambda: not self.tabs(), "tab close")

    def history(self, room):
        base = self.profile / "data/tilo-rooms" / room / self.package
        paths = list(base.rglob("history"))
        if len(paths) != 1:
            return b""
        return paths[0].read_bytes()

    def execute(self):
        first = self.workspace()
        self.launch()
        self.type_expression("123+456")
        self.result("calculation", "123+456", "579")
        self.close()
        wait_for(lambda: b"123+456" in self.history(first["room"]), "Android onPause saved history")
        self.launch()
        self.result("reopened", "123+456", "579")
        self.close()
        wheel = next(t for t in self.request("list_targets")["targets"] if t["name"] == "wheel_disc")
        x, y, _, h = wheel["rect"]
        require_ok(self.request("click_at", x=x + 24, y=y + h - 34))
        second = wait_for(lambda: self.workspace() if self.workspace()["id"] != first["id"] else None,
                          "new workspace")
        self.launch()
        fresh = self.snapshot("second-workspace-fresh")
        if "123+456" in fresh or "579" in fresh or b"123+456" in self.history(second["room"]):
            raise RuntimeError("first workspace state leaked into second")
        self.type_expression("234+567")
        self.result("second-workspace-calculation", "234+567", "801")
        self.close()
        wait_for(lambda: b"234+567" in self.history(second["room"]), "second workspace history")
        if b"234+567" in self.history(first["room"]) or b"123+456" in self.history(second["room"]):
            raise RuntimeError("workspace histories are not separate")
        self.invoke("actions", "workspace.previous")
        wait_for(lambda: self.workspace()["id"] == first["id"], "return to first workspace")
        self.launch()
        self.result("first-workspace-restored", "123+456", "579")
        self.close()
        return first, second


def checked_output(path):
    """Keep run receipts and private profiles outside tracked source."""
    output = path.resolve()
    if output.is_relative_to(REPO) and not output.is_relative_to(REPO / "target"):
        raise ValueError("Android run output must be under target/ or outside the checkout")
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    add_architecture(parser)
    parser.add_argument("--payload", type=Path, required=True)
    parser.add_argument("--runtime-receipt", type=Path, required=True)
    parser.add_argument("--runtime-receipt-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runtime-parent", type=Path, required=True)
    args = parser.parse_args()
    native_architecture(args.architecture)
    output = checked_output(args.output)
    payload = args.payload.resolve(strict=True)
    pinned(payload / "app.apk", APK_SHA)
    report = json.loads((payload / "qualification.json").read_text())
    if not report.get("signature_verified") or report.get("signer_sha256") != SIGNER_SHA:
        raise RuntimeError("payload is not the signature-qualified Arity fixture")
    if report["runtime_receipt_sha256"] != args.runtime_receipt_sha256:
        raise RuntimeError("qualification and runtime receipt disagree")
    verify_runtime(payload / "runtime", args.runtime_receipt, args.runtime_receipt_sha256, args.architecture)
    package = tomllib.loads((payload / "tilo.toml").read_text())["package"]["id"]
    if package != "so.tilo.android.qualify.h" + APK_SHA:
        raise RuntimeError("unexpected qualification identity")
    for binary in ("shell", "tilo-launcher"):
        if not os.access(REPO / "target/debug" / binary, os.X_OK):
            raise RuntimeError("missing prebuilt " + binary)
    subprocess.run(["tesseract", "--version"], capture_output=True, check=True, timeout=10)
    parent = args.runtime_parent.resolve(strict=True)
    if parent.stat().st_uid != os.geteuid() or parent.stat().st_mode & 0o077:
        raise RuntimeError("runtime parent must be owned by this user and private")
    output.mkdir(mode=0o700)
    profile = output / "profile"
    catalogue = profile / "data/tilo-apps"
    catalogue.mkdir(parents=True)
    (catalogue / "registry.toml").write_text(
        '[[entry]]\nid = ' + json.dumps(package) + '\nversion = "0.0.0"\npayload_root = '
        + json.dumps(str(payload)) + '\nmanifest_path = ' + json.dumps(str(payload / "tilo.toml")) + '\n')
    with tempfile.TemporaryDirectory(prefix="tilo-android-", dir=parent) as runtime:
        socket = Path(runtime) / "control.sock"
        env = {**os.environ, "XDG_RUNTIME_DIR": runtime, "TILO_HEADLESS_PROFILE": str(profile),
               "TILO_APPS_DATA_DIR": str(catalogue), "TILO_LAUNCHER_BIN": str(REPO / "target/debug/tilo-launcher"),
               "LP_NUM_THREADS": "2", "RAYON_NUM_THREADS": "2"}
        log = output / "compositor.log"
        with log.open("w") as stream:
            process = subprocess.Popen([str(REPO / "scripts/headless-examples/spawn-headless.sh"), str(socket)],
                                       cwd=REPO, env=env, stdout=stream, stderr=stream, start_new_session=True)
            try:
                wait_for(lambda: socket.is_socket() if process.poll() is None else
                         (_ for _ in ()).throw(RuntimeError("compositor exited; inspect compositor.log")),
                         "control socket")
                journey = Journey(socket, output, package, profile)
                first, second = journey.execute()
                canary = profile / "data/tilo-rooms" / first["room"] / package / "private-canary"
                canary.write_text("qualification canary\n")
                probe_env = {**env, "XDG_DATA_HOME": str(profile / "data"),
                             "XDG_CONFIG_HOME": str(profile / "config"), "XDG_CACHE_HOME": str(profile / "cache"),
                             "XDG_STATE_HOME": str(profile / "state"), "RUST_LOG": "error"}
                probe = subprocess.run([str(REPO / "target/debug/tilo-launcher"), "run", "--manifest",
                                        str(payload / "tilo.toml"), "--payload-root", str(payload), "--no-services",
                                        "--workspace-id", second["room"], "--open", str(canary)],
                                       env=probe_env, capture_output=True, text=True, timeout=30, check=False)
                (output / "confinement.log").write_text(probe.stderr)
                (output / "confinement.json").write_text(probe.stdout)
                if probe.returncode:
                    raise RuntimeError("confinement probe failed; inspect confinement.log")
                confinement = json.loads(probe.stdout)
                expected = {"file_read": "denied", "file_write": "denied", "network": "denied", "account": "synthetic"}
                if confinement != expected or canary.read_text() != "qualification canary\n":
                    raise RuntimeError("confinement probe failed")
                if not re.search(r"landlock: fully enforced .*net_restricted=true", probe.stderr) or "no_new_privs: true" not in probe.stderr:
                    raise RuntimeError("confinement did not enforce mandatory Landlock")
                result = {"schema": 1, "execution": "headless production Tilo compositor, " + args.architecture + " software graphics",
                          "completed_at_utc": datetime.now(timezone.utc).isoformat(),
                          "production_binaries": {binary: digest(REPO / "target/debug" / binary)
                                                  for binary in ("shell", "tilo-launcher")},
                          "apk_sha256": APK_SHA, "runtime_receipt_sha256": args.runtime_receipt_sha256,
                          "evidence": journey.evidence, "first_room": first["room"], "second_room": second["room"],
                          "confinement": confinement, "signed_tilo_admission": False, "real_device": False}
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=10)
        if socket.exists():
            raise RuntimeError("control socket leaked after compositor cleanup")
    result["owned_compositor_stopped"] = True
    (output / "journey.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
