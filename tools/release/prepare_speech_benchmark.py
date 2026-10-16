#!/usr/bin/env python3
"""Prepare bounded public FLEURS probes on a build machine; never record audio.

Three primary test utterances per language, plus unique long-speech sources,
are read from pinned public
archives. Audio is kept outside the repository; its source and derived receipts
are recorded. These deterministic read-speech probes are not a population sample.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import shutil
import tarfile
import urllib.request
import wave

REVISION = "70bb2e84b976b7e960aa89f1c648e09c59f894dd"
BASE = "https://huggingface.co/datasets/google/fleurs/resolve/" + REVISION
LANGUAGES = [("en_us", "en"), ("de_de", "de"), ("fr_fr", "fr"),
             ("es_419", "es"), ("ar_eg", "ar"), ("cmn_hans_cn", "zh")]
MODELS = {
    "tiny": (77691713, "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21"),
    "base": (147951465, "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe"),
    "small-q5_1": (190085487, "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb"),
}


class HttpsOnly(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, url):
        if not url.startswith("https://"):
            raise ValueError("insecure artifact redirect")
        return super().redirect_request(request, response, code, message, headers, url)


def get(url):
    if not url.startswith("https://"):
        raise ValueError("insecure public artifact URL")
    response = urllib.request.build_opener(HttpsOnly()).open(url, timeout=120)
    if not response.url.startswith("https://"):
        response.close()
        raise ValueError("insecure artifact redirect")
    return response


def bounded_bytes(url, limit):
    with get(url) as response:
        data = response.read(limit + 1)
    if len(data) > limit:
        raise ValueError("public metadata exceeds bound")
    return data


class BoundedArchive:
    def __init__(self, response):
        self.response = response
        self.remaining = 32 * 1024 * 1024

    def read(self, size):
        if size < 0 or size > self.remaining:
            raise ValueError("archive prefix exceeds bound")
        result = self.response.read(size)
        self.remaining -= len(result)
        return result


def pcm16(data):
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise ValueError("fixture is not WAV")
    offset, fmt, samples = 12, None, None
    while offset + 8 <= len(data):
        tag = data[offset:offset + 4]
        size = struct.unpack_from("<I", data, offset + 4)[0]
        offset += 8
        if offset + size > len(data):
            raise ValueError("truncated WAV")
        payload = data[offset:offset + size]
        if tag == b"fmt ":
            fmt = struct.unpack_from("<HHIIHH", payload)
        elif tag == b"data":
            samples = payload
        offset += size + size % 2
    if fmt is None or samples is None or fmt[1] != 1 or fmt[2] != 16000:
        raise ValueError("expected 16 kHz mono fixture")
    if fmt[0] == 1 and fmt[5] == 16:
        result = samples
    elif fmt[0] == 1 and fmt[5] == 32:
        result = b"".join(struct.pack("<h", value[0] >> 16)
                          for value in struct.iter_unpack("<i", samples))
    elif fmt[0] == 3 and fmt[5] == 32:
        result = b"".join(struct.pack("<h", max(-32768, min(32767, round(value[0] * 32768))))
                          for value in struct.iter_unpack("<f", samples))
    else:
        raise ValueError("unsupported public WAV encoding")
    if not result or len(result) > 120 * 16000 * 2:
        raise ValueError("fixture audio exceeds session bound")
    output = io.BytesIO()
    with wave.open(output, "wb") as wav:
        wav.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
        wav.writeframes(result)
    return output.getvalue(), len(result) / 32000


def stage_language(destination, config, language, count):
    tsv_url = BASE + "/data/" + config + "/test.tsv"
    metadata = bounded_bytes(tsv_url, 2 * 1024 * 1024)
    references = {}
    for line in metadata.decode().splitlines():
        columns = line.split("\t")
        if len(columns) != 7:
            raise ValueError("unexpected pinned FLEURS schema")
        references[columns[1]] = (columns[0], columns[2])
    cases = []
    archive_url = BASE + "/data/" + config + "/audio/test.tar.gz"
    with get(archive_url) as response:
        with tarfile.open(fileobj=BoundedArchive(response), mode="r|gz") as archive:
            total = 0
            for index, member in enumerate(archive):
                total += member.size
                if index > 128 or member.size > 4 * 1024 * 1024 or total > 16 * 1024 * 1024:
                    raise ValueError("uncompressed archive prefix exceeds bound")
                name = Path(member.name).name
                if not member.isfile() or name not in references:
                    continue
                if member.size > 4 * 1024 * 1024:
                    raise ValueError("audio member exceeds bound")
                extracted = archive.extractfile(member)
                if extracted is None:
                    raise ValueError("audio member unavailable")
                data = extracted.read(member.size + 1)
                if len(data) != member.size:
                    raise ValueError("audio member size mismatch")
                converted, seconds = pcm16(data)
                case_id = config + "-" + str(len(cases))
                filename = case_id + ".wav"
                (destination / filename).write_bytes(converted)
                source_id, reference = references[name]
                cases.append({"id": case_id, "language": language, "file": filename,
                              "size": len(converted), "sha256": hashlib.sha256(converted).hexdigest(),
                              "reference": reference, "duration_seconds": seconds,
                              "source_id": source_id, "source_file": name, "primary": len(cases) < 3,
                              "source_sha256": hashlib.sha256(data).hexdigest(),
                              "transcription_sha256": hashlib.sha256(metadata).hexdigest()})
                if len(cases) == count:
                    break
    if len(cases) != count:
        raise ValueError("insufficient observable public fixtures")
    print(config, [round(case["duration_seconds"], 2) for case in cases], flush=True)
    return cases


def stage_model(destination, name, cache):
    size, sha = MODELS[name]
    url = ("https://huggingface.co/ggerganov/whisper.cpp/resolve/"
           "5359861c739e955e79d9a303bcbc70fb988958b1/ggml-" + name + ".bin")
    if cache is not None and (cache / (name + ".bin")).exists():
        existing = cache / (name + ".bin")
        if existing.stat().st_size != size or hashlib.sha256(existing.read_bytes()).hexdigest() != sha:
            raise ValueError("cached model receipt mismatch")
        shutil.copyfile(existing, destination / (name + ".bin"))
        return {"file": name + ".bin", "size": size, "sha256": sha, "url": url, "license": "MIT"}
    digest, count = hashlib.sha256(), 0
    with get(url) as response, (destination / (name + ".bin")).open("xb") as output:
        while chunk := response.read(65536):
            count += len(chunk)
            if count > size:
                raise ValueError("model exceeds receipt")
            digest.update(chunk)
            output.write(chunk)
    if count != size or digest.hexdigest() != sha:
        raise ValueError("model receipt mismatch")
    return {"file": name + ".bin", "size": size, "sha256": sha, "url": url, "license": "MIT"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--model-cache", type=Path)
    parser.add_argument("--model", action="append", choices=MODELS,
                        help="stage only the selected model(s); default stages all review candidates")
    args = parser.parse_args()
    os.umask(0o077)
    destination = args.destination
    destination.mkdir(mode=0o700, parents=True, exist_ok=False)
    manifest = {"dataset": "google/fleurs", "revision": REVISION, "license": "CC-BY-4.0",
                "attribution": "Google FLEURS dataset authors; pinned public test split",
                "source": "https://huggingface.co/datasets/google/fleurs/blob/" + REVISION + "/README.md",
                "selection": "first three archive members per language; five additional unique long-speech sources for English/French/Mandarin",
                "transform": "16 kHz mono PCM16; source PCM32 shifted or float32 rounded and clamped",
                "models": {}, "cases": []}
    for config, language in LANGUAGES:
        manifest["cases"].extend(stage_language(destination, config, language, 8 if language in ["en", "fr", "zh"] else 3))
    for name in dict.fromkeys(args.model or MODELS):
        manifest["models"][name] = stage_model(destination, name, args.model_cache)
    (destination / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    print("Public probes prepared; no model installed, signed or published; no microphone used.")


if __name__ == "__main__":
    main()
