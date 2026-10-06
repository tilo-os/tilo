#!/usr/bin/env python3
"""Stage the review-only speech release inputs. Never sign, publish or install.

Run on the authorized remote builder. All URLs and receipts come from checked-in
review inputs; HTTPS redirects remain HTTPS. Exact size/digest are verified
before promoting each artifact. The existing production catalogue is untouched.
"""
import argparse
import hashlib
import json
import shutil
from pathlib import Path
import tempfile
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


class HttpsOnly(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise ValueError("non-HTTPS artifact redirect")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def stage(destination, url, size, sha256):
    if urllib.parse.urlsplit(url).scheme != "https":
        raise ValueError("artifact must use HTTPS")
    destination.parent.mkdir(parents=True, exist_ok=True)
    opener = urllib.request.build_opener(HttpsOnly())
    with tempfile.NamedTemporaryFile(dir=destination.parent) as temporary:
        digest = hashlib.sha256()
        count = 0
        with opener.open(url, timeout=180) as response:
            while chunk := response.read(min(65536, size + 1 - count)):
                count += len(chunk)
                if count > size:
                    raise ValueError("artifact exceeds reviewed size")
                digest.update(chunk)
                temporary.write(chunk)
        if count != size or digest.hexdigest() != sha256:
            raise ValueError("artifact receipt mismatch")
        temporary.flush()
        shutil.copyfile(temporary.name, destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    destination = args.destination
    destination.mkdir(parents=True, exist_ok=False)
    source_path = ROOT / "dist/local-models/catalog-source-speech-candidate.json"
    source = json.loads(source_path.read_text())
    if (source["key_id"] != "tilo-local-model-release-2026-10"
            or source["runtimes"] or source["recommendation"]
            or [m["id"] for m in source["models"]] != ["u2net-subject", "whisper-tiny-multilingual"]):
        raise ValueError("unexpected speech review candidate")
    subject, speech = source["models"]
    current = json.loads((ROOT / "dist/local-models/catalog-source.json").read_text())
    if (subject != current["models"][0]
            or subject["artifact"]["file"] != "artifacts/u2net.onnx"
            or speech["artifact"]["file"] != "artifacts/ggml-tiny.bin"):
        raise ValueError("unexpected artifact paths or changed subject release")
    receipt = json.loads((ROOT / "dist/local-models/whisper-tiny/upstream-receipt.json").read_text())
    if (speech["artifact"]["sha256"] != receipt["sha256"]
            or speech["artifact"]["size_bytes"] != receipt["size_bytes"]
            or speech["revision"] != receipt["revision"]
            or speech["artifact_format"] != "whisper_ggml"):
        raise ValueError("speech candidate disagrees with reviewed upstream")
    stage(destination / subject["artifact"]["file"], subject["artifact"]["url"],
          subject["artifact"]["size_bytes"], subject["artifact"]["sha256"])
    stage(destination / "artifacts/u2net-LICENSE", subject["license_url"], 11357,
          "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4")
    stage(destination / speech["artifact"]["file"], receipt["url"], receipt["size_bytes"], receipt["sha256"])
    notice = ROOT / "dist/local-models/whisper-tiny/LICENSE"
    if (len(notice.read_bytes()) != receipt["license_size_bytes"]
            or hashlib.sha256(notice.read_bytes()).hexdigest() != receipt["license_sha256"]):
        raise ValueError("upstream MIT notice receipt mismatch")
    (destination / "artifacts/whisper-LICENSE").write_bytes(notice.read_bytes())
    (destination / "source.json").write_bytes(source_path.read_bytes())
    print("Review candidate prepared; signing, publication and image activation remain separate.")


if __name__ == "__main__":
    main()
