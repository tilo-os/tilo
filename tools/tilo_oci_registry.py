#!/usr/bin/env python3
"""Serve an extracted OCI image layout through a local read-only Registry V2 API."""

from __future__ import annotations

import argparse
import hashlib
from http import HTTPStatus
import http.server
import json
from pathlib import Path
import re
import shutil
import sys
import threading
from typing import Any
from urllib.parse import unquote, urlsplit


DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
TAG = re.compile(r"[A-Za-z0-9_][A-Za-z0-9._-]{0,127}")
REPOSITORY = re.compile(r"[a-z0-9]+(?:[._-][a-z0-9]+)*(?:/[a-z0-9]+(?:[._-][a-z0-9]+)*)*")
JSON_LIMIT = 16 * 1024 * 1024
OCI_INDEX = "application/vnd.oci.image.index.v1+json"
OCI_MANIFEST = "application/vnd.oci.image.manifest.v1+json"


def json_bytes(value: Any) -> bytes:
    return json.dumps(value, separators=(",", ":")).encode("utf-8") + b"\n"


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


class Layout:
    def __init__(self, root: Path):
        self.root = root.resolve(strict=True)
        marker = self.root / "oci-layout"
        index_path = self.root / "index.json"
        if not marker.is_file() or not index_path.is_file():
            raise ValueError(f"not an extracted OCI image layout: {self.root}")
        marker_value = self._read_json(marker)
        if marker_value.get("imageLayoutVersion") != "1.0.0":
            raise ValueError("unsupported OCI layout version")
        self.index = self._read_json(index_path)
        if self.index.get("schemaVersion") != 2 or not isinstance(self.index.get("manifests"), list):
            raise ValueError("invalid OCI index.json")

    @staticmethod
    def _read_json(path: Path) -> dict[str, Any]:
        if path.stat().st_size > JSON_LIMIT:
            raise ValueError(f"JSON metadata exceeds {JSON_LIMIT} bytes: {path}")
        value = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(value, dict):
            raise ValueError(f"JSON metadata is not an object: {path}")
        return value

    def blob_path(self, digest: str) -> Path:
        if not DIGEST.fullmatch(digest):
            raise ValueError(f"invalid digest: {digest}")
        path = self.root / "blobs" / "sha256" / digest.removeprefix("sha256:")
        if not path.is_file() or path.is_symlink():
            raise FileNotFoundError(digest)
        return path

    def descriptors(self) -> list[dict[str, Any]]:
        return [value for value in self.index["manifests"] if isinstance(value, dict)]

    def tags(self) -> dict[str, str]:
        result: dict[str, str] = {}
        for descriptor in self.descriptors():
            name = descriptor.get("annotations", {}).get("org.opencontainers.image.ref.name")
            digest = descriptor.get("digest")
            if isinstance(name, str) and TAG.fullmatch(name) and isinstance(digest, str) and DIGEST.fullmatch(digest):
                result[name] = digest
        return result

    def resolve(self, reference: str) -> tuple[str, str]:
        if DIGEST.fullmatch(reference):
            digest = reference
            descriptor = next((item for item in self.descriptors() if item.get("digest") == digest), None)
            media_type = descriptor.get("mediaType") if descriptor else None
        elif TAG.fullmatch(reference):
            descriptor = next(
                (
                    item
                    for item in self.descriptors()
                    if item.get("annotations", {}).get("org.opencontainers.image.ref.name") == reference
                ),
                None,
            )
            if descriptor is None:
                raise FileNotFoundError(reference)
            digest = descriptor.get("digest")
            media_type = descriptor.get("mediaType")
            if not isinstance(digest, str) or not DIGEST.fullmatch(digest):
                raise ValueError(f"tag has an invalid descriptor digest: {reference}")
        else:
            raise ValueError(f"invalid manifest reference: {reference}")
        path = self.blob_path(digest)
        data = path.read_bytes()
        if len(data) > JSON_LIMIT:
            raise ValueError("manifest exceeds metadata size limit")
        if sha256_bytes(data) != digest:
            raise ValueError(f"manifest content does not match {digest}")
        value = json.loads(data)
        if not isinstance(value, dict) or value.get("schemaVersion") != 2:
            raise ValueError(f"invalid manifest document: {digest}")
        if not isinstance(media_type, str):
            media_type = value.get("mediaType")
        if not isinstance(media_type, str):
            media_type = OCI_INDEX if "manifests" in value else OCI_MANIFEST
        return digest, media_type


def parse_range(value: str | None, size: int) -> tuple[int, int] | None:
    if not value:
        return None
    match = re.fullmatch(r"bytes=(\d*)-(\d*)", value)
    if not match or not any(match.groups()):
        raise ValueError("invalid byte range")
    start_text, end_text = match.groups()
    if not start_text:
        count = int(end_text)
        if count < 1:
            raise ValueError("invalid byte range")
        start, end = max(0, size - count), size - 1
    else:
        start = int(start_text)
        end = min(int(end_text), size - 1) if end_text else size - 1
    if start >= size or end < start:
        raise ValueError("unsatisfiable byte range")
    return start, end


class Registry(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address: tuple[str, int], layout: Layout, repository: str):
        super().__init__(address, Handler)
        self.layout = layout
        self.repository = repository


class Handler(http.server.BaseHTTPRequestHandler):
    server: Registry
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:
        self._serve(head=False)

    def do_HEAD(self) -> None:
        self._serve(head=True)

    def _headers(self, status: HTTPStatus, length: int, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Docker-Distribution-Api-Version", "registry/2.0")
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(length))

    def _error(self, status: HTTPStatus, code: str, message: str, *, head: bool) -> None:
        body = json_bytes({"errors": [{"code": code, "message": message}]})
        self._headers(status, len(body), "application/json")
        self.end_headers()
        if not head:
            self.wfile.write(body)

    def _serve(self, *, head: bool) -> None:
        path = unquote(urlsplit(self.path).path)
        if path == "/v2/":
            self._headers(HTTPStatus.OK, 0, "application/json")
            self.end_headers()
            return
        prefix = f"/v2/{self.server.repository}/"
        if not path.startswith(prefix):
            self._error(HTTPStatus.NOT_FOUND, "NAME_UNKNOWN", "repository not found", head=head)
            return
        suffix = path[len(prefix):]
        if suffix == "tags/list":
            body = json_bytes({"name": self.server.repository, "tags": sorted(self.server.layout.tags())})
            self._headers(HTTPStatus.OK, len(body), "application/json")
            self.end_headers()
            if not head:
                self.wfile.write(body)
            return
        if suffix.startswith("manifests/"):
            reference = suffix.removeprefix("manifests/")
            try:
                digest, media_type = self.server.layout.resolve(reference)
                manifest = self.server.layout.blob_path(digest)
            except (FileNotFoundError, ValueError, json.JSONDecodeError):
                self._error(HTTPStatus.NOT_FOUND, "MANIFEST_UNKNOWN", "manifest not found", head=head)
                return
            self._headers(HTTPStatus.OK, manifest.stat().st_size, media_type)
            self.send_header("Docker-Content-Digest", digest)
            self.send_header("ETag", f'"{digest}"')
            self.end_headers()
            if not head:
                with manifest.open("rb") as source:
                    shutil.copyfileobj(source, self.wfile, 1024 * 1024)
            return
        if suffix.startswith("blobs/"):
            digest = suffix.removeprefix("blobs/")
            try:
                blob = self.server.layout.blob_path(digest)
            except (FileNotFoundError, ValueError):
                self._error(HTTPStatus.NOT_FOUND, "BLOB_UNKNOWN", "blob not found", head=head)
                return
            try:
                selected = parse_range(self.headers.get("Range"), blob.stat().st_size)
            except ValueError:
                self._error(HTTPStatus.REQUESTED_RANGE_NOT_SATISFIABLE, "RANGE_INVALID", "invalid range", head=head)
                return
            size = blob.stat().st_size
            start, end = selected if selected else (0, size - 1)
            length = end - start + 1
            self._headers(HTTPStatus.PARTIAL_CONTENT if selected else HTTPStatus.OK, length, "application/octet-stream")
            self.send_header("Docker-Content-Digest", digest)
            self.send_header("Accept-Ranges", "bytes")
            if selected:
                self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
            self.end_headers()
            if not head:
                with blob.open("rb") as source:
                    source.seek(start)
                    remaining = length
                    while remaining:
                        block = source.read(min(1024 * 1024, remaining))
                        if not block:
                            break
                        self.wfile.write(block)
                        remaining -= len(block)
            return
        self._error(HTTPStatus.NOT_FOUND, "UNSUPPORTED", "unsupported registry operation", head=head)

    def log_message(self, message: str, *values: object) -> None:
        print(f"registry: {self.address_string()} {message % values}", file=sys.stderr)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--layout", type=Path, required=True)
    result.add_argument("--repository", default="tilo/local")
    result.add_argument("--reference", default="latest", help="tag or digest to resolve at startup")
    result.add_argument("--host", default="127.0.0.1")
    result.add_argument("--port", type=int, default=5000)
    result.add_argument("--resolve-only", action="store_true")
    return result


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if not REPOSITORY.fullmatch(args.repository):
            raise ValueError(f"invalid repository name: {args.repository}")
        if not 0 <= args.port <= 65535:
            raise ValueError("port must be between 0 and 65535")
        layout = Layout(args.layout)
        digest, _ = layout.resolve(args.reference)
        print(digest, flush=True)
        if args.resolve_only:
            return 0
        with Registry((args.host, args.port), layout, args.repository) as server:
            host, port = server.server_address[:2]
            print(f"serving http://{host}:{port}/v2/{args.repository}@{digest}", file=sys.stderr, flush=True)
            server.serve_forever()
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"tilo-oci-registry: {error}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
