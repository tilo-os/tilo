#!/usr/bin/env python3
"""Stage the review-only speech release inputs. Never sign, publish or install.

Run on the authorized remote builder. All URLs and receipts come from checked-in
review inputs; HTTPS redirects remain HTTPS. Exact size/digest are verified
before promoting each artifact. The existing production catalogue is untouched.
"""
import argparse
import hashlib
import json
import os
import shutil
from pathlib import Path
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
# A flaky host must not decide a run: a 5xx or a connection error is retried a few times,
# with backoff, and the last failure names the URL and what answered.
TRIES = 5
BACKOFF_SECONDS = 1
LONGEST_BACKOFF_SECONDS = 30
# Where verified artifacts are kept, by their digest, so a later run fetches nothing.
# On t this is under the build directory a run keeps, which a retried run reuses.
CACHE_ENV = "TILO_LOCAL_MODEL_CACHE"


class DownloadError(RuntimeError):
    """The artifact could not be fetched: the URL and what answered are in the message."""


def cache_root() -> Path:
    where = os.environ.get(CACHE_ENV)
    if where:
        return Path(where)
    target = os.environ.get("CARGO_TARGET_DIR")
    return Path(target) / "local-model-artifacts" if target else ROOT / "target" / "local-model-artifacts"


def verified(cache: Path, size: int, sha256: str) -> Path | None:
    """The cached copy of the artifact, when its bytes are the receipt's."""
    candidate = cache / sha256
    try:
        if candidate.stat().st_size != size:
            return None
        if hashlib.sha256(candidate.read_bytes()).hexdigest() != sha256:
            return None
    except FileNotFoundError:
        return None
    return candidate


def transport(error: Exception) -> str | None:
    """Why `error` is a transport failure to retry, or None when it is not retryable."""
    if isinstance(error, urllib.error.HTTPError):
        return f"HTTP {error.code}" if error.code >= 500 else None
    if isinstance(error, urllib.error.URLError):
        return f"connection error: {error.reason}"
    if isinstance(error, (TimeoutError, ConnectionError)):
        return f"connection error: {error}"
    return None


class HttpsOnly(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise ValueError("non-HTTPS artifact redirect")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def fetch(url, size, sha256, cache: Path) -> Path:
    """`url`'s bytes, checked against the receipt, kept in `cache` once they verify."""
    cache.mkdir(parents=True, exist_ok=True)
    opener = urllib.request.build_opener(HttpsOnly())
    reason = "no answer"
    delay = BACKOFF_SECONDS
    for attempt in range(1, TRIES + 1):
        try:
            with tempfile.NamedTemporaryFile(dir=cache) as temporary:
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
                    # A receipt mismatch is the host's bytes, not its mood: ask no again.
                    raise ValueError("artifact receipt mismatch")
                temporary.flush()
                kept = cache / sha256
                shutil.copyfile(temporary.name, kept)
                return kept
        except ValueError:
            raise
        except Exception as error:  # noqa: BLE001 - what a transport may raise, read below
            why = transport(error)
            if why is None:
                raise
            reason = why
            if attempt < TRIES:
                time.sleep(delay)
                delay = min(delay * 2, LONGEST_BACKOFF_SECONDS)
    raise DownloadError(f"{url} could not be fetched after {TRIES} tries: {reason}")


def stage(destination, url, size, sha256):
    if urllib.parse.urlsplit(url).scheme != "https":
        raise ValueError("artifact must use HTTPS")
    cache = cache_root()
    kept = verified(cache, size, sha256) or fetch(url, size, sha256, cache)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(kept, destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--source", type=Path,
                        default=ROOT / "dist/local-models/catalog-source-speech-candidate.json")
    args = parser.parse_args()
    destination = args.destination
    destination.mkdir(parents=True, exist_ok=False)
    source_path = args.source
    source = json.loads(source_path.read_text())
    if (source["key_id"] != "tilo-local-model-release-2026-10"
            or source["runtimes"] or source["recommendation"]
            or source["schema"] != 1 or source["production"] is not True
            or [m["id"] for m in source["models"]] != ["u2net-subject", "whisper-tiny-multilingual",
                                                        "whisper-base-multilingual",
                                                        "whisper-small-q5_1-multilingual"]):
        raise ValueError("unexpected speech review candidate")
    subject, *speech_models = source["models"]
    current = json.loads((ROOT / "dist/local-models/catalog-source.json").read_text())
    if (subject != current["models"][0]
            or subject["artifact"]["file"] != "artifacts/u2net.onnx"):
        raise ValueError("unexpected artifact paths or changed subject release")
    receipts = []
    for speech, package in zip(speech_models, ["whisper-tiny", "whisper-base", "whisper-small-q5_1"]):
        receipt = json.loads((ROOT / f"dist/local-models/{package}/upstream-receipt.json").read_text())
        base = f"https://code.tilo.so/api/packages/tilo/generic/{package}/{receipt['revision']}"
        if (speech["artifact"] != {"file": f"artifacts/{receipt['file']}",
                                  "url": f"{base}/{receipt['file']}",
                                  "sha256": receipt["sha256"], "size_bytes": receipt["size_bytes"]}
                or speech["revision"] != receipt["revision"]
                or speech["artifact_format"] != "whisper_ggml"
                or speech["license_url"] != f"{base}/LICENSE"
                or speech["license_id"] != "MIT" or speech["requires_acceptance"] is not True
                or speech["installed_size_bytes"] != receipt["size_bytes"]
                or speech["allowed_runtimes"] or speech["context_window"]
                or any(speech[key] for key in ["vision", "face_recognition", "segmentation"])
                or speech["required_accelerator_mib"]
                or speech["required_memory_mib"] != (2048 if package == "whisper-small-q5_1" else 1024)):
            raise ValueError("speech candidate disagrees with reviewed upstream")
        notice = ROOT / f"dist/local-models/{package}/LICENSE"
        if (len(notice.read_bytes()) != receipt["license_size_bytes"]
                or hashlib.sha256(notice.read_bytes()).hexdigest() != receipt["license_sha256"]):
            raise ValueError("upstream MIT notice receipt mismatch")
        receipts.append((speech, package, receipt, notice))
    stage(destination / subject["artifact"]["file"], subject["artifact"]["url"],
          subject["artifact"]["size_bytes"], subject["artifact"]["sha256"])
    stage(destination / "artifacts/u2net-LICENSE", subject["license_url"], 11357,
          "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4")
    for speech, package, receipt, notice in receipts:
        stage(destination / speech["artifact"]["file"], receipt["url"], receipt["size_bytes"], receipt["sha256"])
        (destination / f"artifacts/{package}-LICENSE").write_bytes(notice.read_bytes())
    (destination / "source.json").write_bytes(source_path.read_bytes())
    print("Review candidate prepared; signing, publication and image activation remain separate.")


if __name__ == "__main__":
    main()
