#!/usr/bin/env python3
"""Prove the booted image decodes H.264, and that a board's codec block decodes it.

    os/tests/h264-boot-check.py            # generic images: software is the answer
    os/tests/h264-boot-check.py --require-hardware

Runs inside a booted guest with the image's own `GStreamer`. It generates a
short H.264 clip, pushes it through `decodebin` in a `gst-launch-1.0` pipeline,
and reads the decoder the pipeline actually selected out of the log — not out of
a configuration file, because the question is what the image does, not what it
says.

What counts as a pass is what the machine really has:

* Hardware. When the kernel exposes a V4L2 codec block or a VA-API driver (or
  the caller passed `--require-hardware`), a hardware decoder must be the one
  selected. A software decoder here is a failure: it is the case the boards
  care about, and the fan spinning up is the symptom.
* Software. On a machine with no video hardware the same pipeline must decode
  the clip with a software element, and the check prints which one. Skipping
  instead would make "no decoder at all" and "decodes in software" read
  identically.

H.264 is the stream the image decodes but does not encode, so the encoder half
is opt-in: Cisco's `openh264enc` comes from the downloaded `media-codecs`
system extension, and a stock image has none. When no encoder is installed the
clip cannot exist and the check fails with that reason: it has no subject, and a
check that cannot observe its subject must fail, never report a decode it did
not perform.

Exit 0 pass, 1 fail (a decoder that cannot place, or no subject at all).
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# 320x240 is inside every H.264 decoder's minimum and a multiple of 16, the
# codec's macroblock grid, so no decoder crops a partial block to take it.
WIDTH, HEIGHT, FRAMES, FPS = 320, 240, 60, 30

# Encoders that write H.264. The image itself carries none of them: both come
# from the opt-in `media-codecs` system extension, which is exactly why their
# absence is a legitimate answer rather than a broken image.
ENCODERS = ("openh264enc", "x264enc")

# The element-name prefix each family's decoders use, and which side of the
# hardware/CPU line it falls on. A decoder whose prefix is in neither table is
# one this check cannot place, and it says so rather than guessing.
HARDWARE = {
    "v4l2": "the SoC's V4L2 codec block",
    "vah264": "VA-API (an Intel or AMD GPU)",
    "vaapih264": "VA-API (an Intel or AMD GPU)",
    "nvh264": "NVIDIA NVDEC",
    "vulkanh264": "Vulkan video",
}
SOFTWARE = {
    "avdec_h264": "libavcodec on the CPU",
    "openh264dec": "Cisco openh264 on the CPU",
}


class Failure(Exception):
    """The image failed the thing this check claims to prove."""


def run(argv: list[str], *, timeout: int = 120) -> subprocess.CompletedProcess[str]:
    # English output: the messages this check reads are parsed, and a translated
    # `gst-launch-1.0` would leave the parse silently matching nothing. The
    # element-factory debug log is where the autoplugger states its choice.
    return subprocess.run(
        argv, capture_output=True, text=True, timeout=timeout,
        env={**os.environ, "GST_DEBUG": "GST_ELEMENT_FACTORY:5", "LANG": "C", "LC_ALL": "C"},
    )


def have(element: str) -> bool:
    return run(["gst-inspect-1.0", element], timeout=60).returncode == 0


def make_clip(directory: Path) -> tuple[Path, str]:
    """Generate the H.264 subject, or say why no image could."""
    encoder = next((name for name in ENCODERS if have(name)), None)
    if encoder is None:
        raise Failure(
            "no H.264 encoder is installed, so this check has no subject to decode; "
            "install the media-codecs system extension to give it one"
        )
    for element in ("h264parse", "mp4mux", "qtdemux"):
        if not have(element):
            raise Failure(
                f"{element} is missing from the image's GStreamer, so an H.264 clip "
                "cannot be written or demuxed"
            )
    clip = directory / "h264-aac.mp4"
    written = run([
        "gst-launch-1.0", "-q",
        "videotestsrc", f"num-buffers={FRAMES}", "!",
        f"video/x-raw,format=I420,width={WIDTH},height={HEIGHT},framerate={FPS}/1", "!",
        encoder, "!",
        "h264parse", "!", "mp4mux", "!",
        "filesink", f"location={clip}",
    ])
    if written.returncode != 0 or not clip.exists() or clip.stat().st_size == 0:
        raise Failure(f"{encoder} could not write the H.264 clip: {written.stderr.strip()}")
    return clip, encoder


def kernel_codec_blocks() -> list[str]:
    """Kernel video codec nodes, as the drivers name their cards."""
    blocks = []
    for node in sorted(Path("/sys/class/video4linux").glob("video*/name")):
        try:
            blocks.append(node.read_text().strip())
        except OSError:
            continue
    return blocks


def vaapi_driver_present() -> bool:
    """A libva driver for a device the kernel reports, the way the image sees one."""
    drivers = sorted(Path("/usr/lib64/dri").glob("*_drv_video.so")) + sorted(
        Path("/usr/lib/dri").glob("*_drv_video.so"))
    if not drivers:
        return False
    # Loading it is the only proof it can drive this GPU. `vainfo` ships with
    # libva-utils, which the image carries for exactly this.
    if shutil.which("vainfo") is None:
        return True
    probe = run(["vainfo"], timeout=60)
    return probe.returncode == 0 and "VAProfileH264" in probe.stdout


def decoders_selected(log: str) -> list[str]:
    """The decoder elements the pipeline created, in the order it created them.

    `GST_ELEMENT_FACTORY` logs one `creating element "<name>"` per element the
    autoplugger instantiates, which is the only statement about the pipeline
    that comes from the pipeline. The demuxer and parser the check wired itself
    are named here so they are not mistaken for the answer.
    """
    created = re.findall(r'creating element "([^"]+)"', log)
    return [name for name in created if is_decoder(name)]


def decode(clip: Path) -> list[str]:
    """Push the clip through `decodebin` and report the decoders it created.

    `decodebin` rather than a named element, because what is being proved is the
    choice the image makes by itself: a hard-wired `v4l2h264dec` would test the
    element, not the ranking that has to put it above the CPU decoder.
    """
    pipeline = run([
        "gst-launch-1.0",
        # No leading `-q`: the element factory logs the autoplugger's choice,
        # and `-q` would drop every log line but errors.
        "filesrc", f"location={clip}", "!",
        "qtdemux", "!", "h264parse", "!", "decodebin", "!", "fakesink", "sync=false",
    ])
    log = pipeline.stdout + pipeline.stderr
    if pipeline.returncode != 0:
        raise Failure(f"the H.264 pipeline exited {pipeline.returncode}: {log.strip()[-2000:]}")
    if "Got EOS" not in log:
        raise Failure(f"the H.264 pipeline never reached the end of the clip: {log.strip()[-2000:]}")
    chosen = decoders_selected(log)
    if not chosen:
        raise Failure(
            "the pipeline reached the end of the clip but created no decoder element, "
            f"so nothing decoded it: {log.strip()[-2000:]}"
        )
    return chosen


def family_of(element: str) -> str | None:
    """The prefix a decoder element is known by, or `None` when it is neither a
    hardware nor a CPU decoder this check can place."""
    lowered = element.lower()
    return next((prefix for prefix in (*HARDWARE, *SOFTWARE)
                 if lowered.startswith(prefix)), None)


def describe(element: str) -> str:
    """What family a decoder belongs to, in a person's words."""
    return {**HARDWARE, **SOFTWARE}.get(family_of(element) or "", "a decoder this check cannot place")


def is_decoder(element: str) -> bool:
    """Whether the autoplugger created `element` to turn video into pictures.

    `dec` in the name covers every family's spelling (`avdec_h264`,
    `v4l2slh264dec`, `vtdec_hw`); the containers the check wires itself are
    excluded by what they are, not by their names.
    """
    return "dec" in element.lower() and not element.lower().startswith(
        ("decodebin", "typefind", "videodecoder"))


def is_software(element: str) -> bool:
    return family_of(element) in SOFTWARE


def is_hardware(element: str) -> bool:
    return family_of(element) in HARDWARE


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--require-hardware", action="store_true",
        help="fail unless a hardware decoder decoded the clip (board images with a codec block)",
    )
    args = parser.parse_args()

    for tool in ("gst-launch-1.0", "gst-inspect-1.0"):
        if shutil.which(tool) is None:
            raise Failure(f"{tool} is not installed, so nothing can be observed")

    with tempfile.TemporaryDirectory(prefix="tilo-h264-") as directory:
        clip, encoder = make_clip(Path(directory))
        chosen = decode(clip)
        hardware = [name for name in chosen if is_hardware(name)]
        software = [name for name in chosen if is_software(name)]
        blocks = kernel_codec_blocks()
        vaapi = vaapi_driver_present()

        print(f"encoded {WIDTH}x{HEIGHT} H.264 with {encoder}")
        print(f"kernel video codec nodes: {', '.join(blocks) if blocks else 'none'}")
        print(f"VA-API driver: {'present' if vaapi else 'none'}")
        print(f"decoders the pipeline created: {', '.join(chosen)}")

        if args.require_hardware or blocks or vaapi:
            if not hardware:
                exposed = ", ".join(blocks) if blocks else (
                    "a VA-API driver" if vaapi else "the caller's --require-hardware")
                raise Failure(
                    f"this machine exposes H.264 video hardware ({exposed}) but the pipeline "
                    f"decoded with {', '.join(chosen)}. The hardware decoder is missing from "
                    "the image or ranked below the CPU one."
                )
            print(f"hardware decode: {', '.join(hardware)} ({describe(hardware[0])})")
        if software:
            print(f"software decode: {', '.join(software)} ({describe(software[0])})")
        if not hardware and not software:
            raise Failure(
                f"none of the decoders the pipeline created ({', '.join(chosen)}) is one this "
                "check can tell hardware from CPU by"
            )
        return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failure as failure:
        print(f"FAIL: {failure}", file=sys.stderr)
        sys.exit(1)
