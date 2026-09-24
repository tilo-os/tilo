#!/usr/bin/env python3
"""Exercise the real local image-analysis gateway after Settings has started it."""

import argparse
import base64
import hashlib
import json
import socket
import struct
import sys

# A tiny valid PNG keeps this acceptance probe bounded while still forcing the
# vLLM multimodal image path. It is not a model fixture or an expected answer.
IMAGE = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9JfZQAAAAASUVORK5CYII="
)


def write_frame(stream, value):
    body = json.dumps(value, separators=(",", ":")).encode()
    stream.sendall(struct.pack("<I", len(body)) + body)


def read_exact(stream, size):
    result = bytearray()
    while len(result) < size:
        part = stream.recv(size - len(result))
        if not part:
            raise RuntimeError("gateway closed its response")
        result.extend(part)
    return bytes(result)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--socket", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--model", required=True)
    args = parser.parse_args()

    request = {
        "version": 3,
        "verb": "analyze_image",
        "profile_id": args.profile,
        "model_id": args.model,
        "image": {
            "media_type": "image/png",
            "data_base64": base64.b64encode(IMAGE).decode(),
            "source_content_hash": hashlib.sha256(IMAGE).hexdigest(),
        },
    }
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.connect(args.socket)
        # This socket is an explicitly developer-created gateway listener.
        # Normal desktop calls receive this handoff only from the launcher.
        write_frame(stream, {"protocol": 1, "app_id": "so.tilo.photos"})
        write_frame(stream, request)
        length = struct.unpack("<I", read_exact(stream, 4))[0]
        response = json.loads(read_exact(stream, length))
    if not response.get("ok"):
        raise RuntimeError(response.get("error", "gateway rejected the probe"))
    provider = json.loads(response["output"])
    content = provider["choices"][0]["message"]["content"]
    analysis = json.loads(content)
    required = {"description", "labels", "pets", "ocr"}
    if not required.issubset(analysis):
        raise RuntimeError("model response did not satisfy the Photos analysis schema")
    print(json.dumps(analysis, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (KeyError, TypeError, ValueError, OSError, RuntimeError) as error:
        print(f"local vision smoke failed: {error}", file=sys.stderr)
        sys.exit(1)
