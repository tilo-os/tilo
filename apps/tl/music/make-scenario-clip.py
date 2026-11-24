#!/usr/bin/env python3
"""Write the tone the "a song playing" scenario plays (`media/first-light.wav`).

A soft 440 Hz tone for a second and a half, faded in and out so it starts and stops cleanly. It is
generated here rather than downloaded, so the package ships nothing it did not make and carries no
licence but its own. Eight-bit mono at 11 025 Hz keeps it about 16 KiB.

Run from this directory: `python3 make-scenario-clip.py`.
"""

import math
import struct
import wave
from pathlib import Path

RATE = 11_025
SECONDS = 1.5
FREQUENCY = 440.0
# Quiet: a demo that plays by itself must not startle anyone.
AMPLITUDE = 0.18
FADE = 0.05


def sample(at: float) -> int:
    edge = min(1.0, at / FADE, (SECONDS - at) / FADE)
    value = AMPLITUDE * edge * math.sin(2 * math.pi * FREQUENCY * at)
    return int(round(127.5 + 127.0 * value))


def main() -> None:
    frames = bytearray(sample(index / RATE) for index in range(int(RATE * SECONDS)))
    out = Path(__file__).parent / "media" / "first-light.wav"
    out.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(out), "wb") as clip:
        clip.setnchannels(1)
        clip.setsampwidth(1)
        clip.setframerate(RATE)
        clip.writeframes(bytes(frames))
    print(f"{out}: {out.stat().st_size} bytes")


if __name__ == "__main__":
    main()
