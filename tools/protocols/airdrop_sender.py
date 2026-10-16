#!/usr/bin/env python3
"""Bounded AirDrop wire sender; release-unavailable Files lab capability.

The broker must approve the endpoint and certificate fingerprint per attempt and
pass a read-only regular-file descriptor. No discovery, AWDL setup, Apple account
credentials, trust persistence, receiver, or source path reopening lives here.
Wire documentation/reference: seemoo-lab/OpenDrop (GPL-3.0), client.py/server.py.
This independent implementation imports only Python's standard library.
"""

import argparse
import fcntl
import gzip
import hashlib
import hmac
import http.client
import ipaddress
import json
import os
import plistlib
import secrets
import signal
import ssl
import stat

MAX_FILE_BYTES = 16 * 1024 * 1024
MAX_RESPONSE_BYTES = 64 * 1024
TIMEOUT_SECONDS = 15
UNSAFE_REVIEW_CHARACTERS = "\u061c\u200e\u200f\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069"


class TransferError(Exception):
    """A failed boundary or protocol step; no retry silently changes authority."""


def endpoint(address, port):
    # Literal IPs avoid DNS/redirect substitutions after explicit peer selection.
    try:
        parsed = ipaddress.ip_address(address)
    except ValueError as error:
        raise TransferError("receiver must be a literal IP address") from error
    if parsed.is_unspecified or parsed.is_multicast or not 1 <= port <= 65535:
        raise TransferError("invalid receiver endpoint")
    return str(parsed)


def fingerprint(value):
    if len(value) != 64:
        raise TransferError("a per-attempt SHA-256 certificate pin is required")
    try:
        result = bytes.fromhex(value)
    except ValueError as error:
        raise TransferError("invalid certificate pin") from error
    if len(result) != 32:
        raise TransferError("invalid certificate pin")
    return result


def tls_context():
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    # AirDrop uses self-signed leaf certificates. PinnedConnection replaces CA
    # validation with a mandatory exact, explicitly approved leaf digest check
    # before *each* connection can send an HTTP request, including reconnects.
    context.verify_mode = ssl.CERT_NONE
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    return context


class PinnedConnection(http.client.HTTPSConnection):
    def __init__(self, address, port, pin):
        super().__init__(endpoint(address, port), port, timeout=TIMEOUT_SECONDS,
                         context=tls_context())
        self.pin = fingerprint(pin)

    def connect(self):
        super().connect()
        actual = hashlib.sha256(self.sock.getpeercert(binary_form=True)).digest()
        if not hmac.compare_digest(actual, self.pin):
            self.close()
            raise TransferError("receiver certificate changed or was not approved")


def inspect_peer(address, port):
    """TLS-only observation for a broker's consent prompt; grants no authority."""
    connection = http.client.HTTPSConnection(endpoint(address, port), port,
                                            timeout=TIMEOUT_SECONDS, context=tls_context())
    try:
        connection.connect()
        return hashlib.sha256(connection.sock.getpeercert(binary_form=True)).hexdigest()
    finally:
        connection.close()


def snapshot(fd, name):
    if not name or name in (".", "..") or "/" in name or "\\" in name:
        raise TransferError("source name must be one portable basename")
    if any(ord(char) < 32 or ord(char) == 127 or char in UNSAFE_REVIEW_CHARACTERS
           for char in name) or len(name.encode()) > 255:
        raise TransferError("invalid source basename")
    before = os.fstat(fd)
    if not stat.S_ISREG(before.st_mode):
        raise TransferError("source descriptor must be a regular file")
    if fcntl.fcntl(fd, fcntl.F_GETFL) & os.O_ACCMODE != os.O_RDONLY:
        raise TransferError("source descriptor must be read-only")
    if before.st_size > MAX_FILE_BYTES:
        raise TransferError("source exceeds the 16 MiB adapter limit")
    data = bytearray()
    while len(data) < before.st_size:
        chunk = os.pread(fd, min(64 * 1024, before.st_size - len(data)), len(data))
        if not chunk:
            raise TransferError("source changed while staging")
        data.extend(chunk)
    after = os.fstat(fd)
    if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
            after.st_size, after.st_mtime_ns, after.st_ctime_ns):
        raise TransferError("source changed while staging")
    return bytes(data)


def archive(name, data):
    # CPIO newc: one regular file, no directory traversal, links, owner ids,
    # timestamps or source paths. Four-byte alignment is relative to the header.
    def entry(path, payload, mode, inode):
        filename = path.encode() + b"\0"
        fields = (inode, mode, 0, 0, 1, 0, len(payload), 0, 0, 0, 0, len(filename), 0)
        header = b"070701" + b"".join(f"{field:08x}".encode() for field in fields)
        prefix = header + filename
        return prefix + b"\0" * (-len(prefix) % 4) + payload + b"\0" * (-len(payload) % 4)
    return gzip.compress(entry("./" + name, data, stat.S_IFREG | 0o600, 1)
                         + entry("TRAILER!!!", b"", 0, 0), mtime=0)


def stage(source_fd, staged_fd, name):
    """Copy one approved descriptor without network access; the host seals it.

    Running staging as a bounded child also lets the host stop a stalled FUSE
    hydration before presenting a consent sheet. No source path is reopened.
    """
    metadata = os.fstat(staged_fd)
    if (not stat.S_ISREG(metadata.st_mode) or metadata.st_size != 0
            or fcntl.fcntl(staged_fd, fcntl.F_GETFL) & os.O_ACCMODE != os.O_RDWR):
        raise TransferError("staging descriptor must be an empty writable file")
    data = snapshot(source_fd, name)
    written = 0
    while written < len(data):
        count = os.pwrite(staged_fd, data[written:written + 64 * 1024], written)
        if count <= 0:
            raise TransferError("staging write failed")
        written += count
    return {"bytes": len(data), "content_hash": hashlib.sha256(data).hexdigest()}


def post(connection, path, body, chunked=False):
    headers = {"User-Agent": "AirDrop/1.0", "Connection": "keep-alive",
               "Content-Type": "application/x-cpio" if chunked else "application/octet-stream"}
    if chunked:
        headers["Transfer-Encoding"] = "chunked"
    connection.request("POST", path, body=iter([body]) if chunked else body,
                       headers=headers, encode_chunked=chunked)
    response = connection.getresponse()
    content = response.read(MAX_RESPONSE_BYTES + 1)
    if len(content) > MAX_RESPONSE_BYTES:
        raise TransferError("receiver response exceeds protocol limit")
    if response.status != 200:
        raise TransferError("receiver refused " + path)
    return content


def send(address, port, pin, fd, name):
    """Exchange a single bounded immutable object with a pinned consenting peer."""
    connection = PinnedConnection(address, port, pin)
    data = snapshot(fd, name)
    try:
        reply = plistlib.loads(post(connection, "/Discover", plistlib.dumps({}, fmt=plistlib.FMT_BINARY)))
        if not isinstance(reply, dict) or not isinstance(reply.get("ReceiverComputerName"), str):
            raise TransferError("receiver did not advertise an AirDrop identity")
        request = {"SenderComputerName": "Tilo", "SenderModelName": "Tilo",
                   "BundleID": "com.apple.finder", "SenderID": secrets.token_hex(6),
                   "ConvertMediaFormats": False, "Files": [{"FileName": name,
                   "FileType": "public.content", "FileBomPath": "./" + name,
                   "FileIsDirectory": False, "ConvertMediaFormats": 0}]}
        # No content upload unless the exact pinned peer positively answers Ask.
        post(connection, "/Ask", plistlib.dumps(request, fmt=plistlib.FMT_BINARY))
        post(connection, "/Upload", archive(name, data), chunked=True)
        return {"bytes": len(data), "content_hash": hashlib.sha256(data).hexdigest()}
    finally:
        connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("address")
    parser.add_argument("port", type=int)
    sub = parser.add_subparsers(dest="operation", required=True)
    sub.add_parser("inspect")
    transfer = sub.add_parser("send")
    transfer.add_argument("--certificate-sha256", required=True)
    transfer.add_argument("--source-fd", type=int, required=True)
    transfer.add_argument("--name", required=True)
    staging = sub.add_parser("stage")
    staging.add_argument("--source-fd", type=int, required=True)
    staging.add_argument("--staged-fd", type=int, required=True)
    staging.add_argument("--name", required=True)
    args = parser.parse_args()
    def expired(_signal, _frame):
        raise TransferError("attempt exceeded the adapter deadline")
    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, 60)
    try:
        if args.operation == "stage":
            result = stage(args.source_fd, args.staged_fd, args.name)
        elif args.operation == "inspect":
            result = {"certificate_sha256": inspect_peer(args.address, args.port)}
        else:
            result = send(args.address, args.port, args.certificate_sha256,
                          args.source_fd, args.name)
        print(json.dumps(result))
    except (TransferError, OSError, http.client.HTTPException,
            plistlib.InvalidFileException, ValueError):
        # Remote text, file names and local paths never enter diagnostics.
        print(json.dumps({"error": "AirDrop adapter attempt failed"}))
        raise SystemExit(1)
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)


if __name__ == "__main__":
    main()
