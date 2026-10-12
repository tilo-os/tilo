#!/usr/bin/env python3
"""Observe the real projector's same-connection DeviceAuth gate on loopback.

The independent receiver provides an ephemeral self-signed RSA TLS leaf and a
DeviceAuth error. The production binary must challenge first, disconnect and
publish no readiness, control or media URL. It changes no trust store or device.
--legacy-transport-only explicitly selects the historical unauthenticated
LOAD/HLS/STOP witness for a baseline binary; never use it to qualify DeviceAuth.
Requires a prebuilt tilo-projector and openssl in the isolated execution host.
"""

import argparse
import contextlib
import json
import hashlib
import os
from pathlib import Path
import signal
import socket
import ssl
import struct
import subprocess
import tempfile
import threading
import time
import urllib.request
import urllib.parse
import urllib.error

PREFIX = "urn:x-cast:com.google.cast."


def varint(value):
    encoded = bytearray()
    while value > 127:
        encoded.append((value & 127) | 128)
        value >>= 7
    encoded.append(value)
    return bytes(encoded)


def read_varint(data, offset):
    result = 0
    for shift in range(0, 70, 7):
        assert offset < len(data), "truncated protobuf integer"
        byte = data[offset]
        offset += 1
        result |= (byte & 127) << shift
        if byte < 128:
            return result, offset
    raise AssertionError("oversized protobuf integer")


def fields_from_proto(data):
    fields, offset = {}, 0
    while offset < len(data):
        tag, offset = read_varint(data, offset)
        if tag & 7 == 0:
            value, offset = read_varint(data, offset)
        else:
            assert tag & 7 == 2, "unexpected Cast protobuf wire type"
            size, offset = read_varint(data, offset)
            assert offset + size <= len(data), "truncated Cast protobuf field"
            value, offset = data[offset:offset + size], offset + size
        fields[tag >> 3] = value
    return fields


def decode(data):
    fields = fields_from_proto(data)
    assert fields[1] == 0, "Cast v2 envelope required"
    if fields[5] == 1:
        assert fields[4] == (PREFIX + "tp.deviceauth").encode()
        assert fields[2] == b"sender-0" and fields[3] == b"receiver-0"
        assert 6 not in fields, "binary auth must not contain a string payload"
        auth = fields_from_proto(fields[7])
        assert set(auth) == {1}, "DeviceAuth request must contain only a challenge"
        challenge = fields_from_proto(auth[1])
        assert challenge[1] == 1 and challenge[3] == 1, "explicit RSA PKCS1/SHA256 required"
        assert len(challenge[2]) == 16, "fresh 16-byte nonce required"
        return fields[2].decode(), fields[4].decode(), {"type": "DEVICE_AUTH_CHALLENGE"}
    assert fields[5] == 0, "known Cast payload type required"
    return fields[2].decode(), fields[4].decode(), json.loads(fields[6])


def send_auth_error(stream, destination):
    data = b"\x08\x00"
    for tag, value in ((2, "receiver-0"), (3, destination), (4, PREFIX + "tp.deviceauth")):
        encoded = value.encode()
        data += varint(tag << 3 | 2) + varint(len(encoded)) + encoded
    # DeviceAuthMessage.error -> AuthError.NO_TLS. This receiver cannot prove
    # a manufacturer identity; the production controller must reject it.
    error = b"\x1a\x02\x08\x01"
    data += b"\x28\x01\x3a" + varint(len(error)) + error
    stream.sendall(struct.pack(">I", len(data)) + data)


def send(stream, destination, namespace, payload):
    data = b"\x08\x00"
    for tag, value in ((2, "receiver-0"), (3, destination), (4, namespace)):
        encoded = value.encode()
        data += varint(tag << 3 | 2) + varint(len(encoded)) + encoded
    encoded = json.dumps(payload).encode()
    data += b"\x28\x00\x32" + varint(len(encoded)) + encoded
    stream.sendall(struct.pack(">I", len(data)) + data)


def receive(stream, size):
    data = bytearray()
    while len(data) < size:
        part = stream.recv(size - len(data))
        if not part:
            raise EOFError("Cast sender disconnected")
        data.extend(part)
    return bytes(data)


def projector_events(path):
    """The service events the `connect` client printed, one JSON value per line."""
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def has_event(path, kind, timeout=0.0):
    deadline = time.monotonic() + timeout
    while True:
        if any(isinstance(event, dict) and kind in event for event in projector_events(path)):
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)


def file_sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    if not __debug__:
        raise RuntimeError("proof requires enabled Python assertions")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--projector", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--legacy-transport-only", action="store_true")
    args = parser.parse_args()
    binary = args.projector.resolve()
    if not binary.is_file():
        parser.error("missing prebuilt projector")
    with tempfile.TemporaryDirectory(prefix="tilo-cast-proof-") as temporary:
        root = Path(temporary)
        cert, key = root / "cert.pem", root / "key.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", str(key), "-out", str(cert), "-days", "1",
                        "-subj", "/CN=isolated-cast-fixture"], check=True, capture_output=True, timeout=15)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(cert, key)
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        listener.settimeout(20)
        loaded, disconnected = threading.Event(), threading.Event()
        errors, observed, fetched, tls = [], [], [], []
        session = root / "session"
        def receiver():
            try:
                raw, _ = listener.accept()
                raw.settimeout(15)
                with context.wrap_socket(raw, server_side=True) as stream:
                    tls.append(stream.version())
                    while True:
                        size = struct.unpack(">I", receive(stream, 4))[0]
                        assert 0 < size <= 65536, "bounded Cast message"
                        source, namespace, payload = decode(receive(stream, size))
                        kind = payload["type"]
                        observed.append((namespace, kind))
                        if not args.legacy_transport_only:
                            assert observed == [(PREFIX + "tp.deviceauth", "DEVICE_AUTH_CHALLENGE")], "no control or media before authentication"
                            send_auth_error(stream, source)
                            continue
                        assert kind != "DEVICE_AUTH_CHALLENGE", "legacy witness cannot qualify a DeviceAuth binary"
                        if namespace == PREFIX + "tp.heartbeat" and kind == "PING":
                            send(stream, source, namespace, {"type": "PONG"})
                        elif namespace == PREFIX + "receiver" and kind in ("GET_STATUS", "LAUNCH"):
                            # Wait for production session initialization before the
                            # fixture provides HLS bytes. Never fabricate ready.
                            session.mkdir(exist_ok=True)
                            (session / "live.m3u8").write_bytes(b"#EXTM3U\nsegment-00001.ts\n")
                            (session / "segment-00001.ts").write_bytes(b"fixture MPEG-TS bytes")
                            send(stream, source, namespace, {"type": "RECEIVER_STATUS",
                                "requestId": payload.get("requestId", 0), "status": {"applications": [{
                                    "appId": "CC1AD845", "sessionId": "fixture-session",
                                    "transportId": "fixture-transport"}]}})
                        elif namespace == PREFIX + "media" and kind == "GET_STATUS":
                            send(stream, source, namespace, {"type": "MEDIA_STATUS", "requestId": payload.get("requestId", 0), "status": []})
                        elif namespace == PREFIX + "media" and kind == "LOAD":
                            media = payload["media"]
                            assert media["contentType"] == "application/vnd.apple.mpegurl"
                            url = media["contentId"]
                            parsed = urllib.parse.urlsplit(url)
                            assert parsed.hostname == "127.0.0.1" and parsed.path.endswith("/live.m3u8")
                            token = parsed.path.split("/")[1]
                            assert len(token) >= 32 and all(c in "0123456789abcdef" for c in token)
                            with urllib.request.urlopen(url, timeout=3) as response:
                                assert response.read() == b"#EXTM3U\nsegment-00001.ts\n"
                            with urllib.request.urlopen(url.replace("live.m3u8", "segment-00001.ts"), timeout=3) as response:
                                assert response.read() == b"fixture MPEG-TS bytes"
                            for denied in ("/wrong/live.m3u8", f"/{token}/session.pid", f"/{token}/../session.pid"):
                                try:
                                    urllib.request.urlopen(urllib.parse.urlunsplit(parsed._replace(path=denied)), timeout=3)
                                except urllib.error.HTTPError as error:
                                    assert error.code in (400, 404)
                                else:
                                    raise AssertionError("private runtime file or guessed stream URL was exposed")
                            fetched.append("tokenized playlist and segment; private files denied")
                            loaded.set()
            except EOFError:
                disconnected.set()
            except BaseException as error:
                errors.append(error)
        thread = threading.Thread(target=receiver, daemon=True)
        thread.start()
        env = os.environ.copy()
        env.update(TILO_PROJECTOR_RUNTIME_DIR=str(session), XDG_RUNTIME_DIR=str(root))
        log = (root / "projector.log").open("wb")
        events = root / "events.jsonl"
        events_log = events.open("wb")
        # The production service, then a control-socket client that owns the
        # session for as long as it stays connected.
        service = subprocess.Popen([str(binary), "serve"], env=env, stdout=log, stderr=log,
            start_new_session=True)
        process = None
        try:
            control_socket = session / "control.sock"
            deadline = time.monotonic() + 10
            while not control_socket.exists() and service.poll() is None and time.monotonic() < deadline:
                time.sleep(0.05)
            assert control_socket.exists(), "projector service must listen on its control socket"
            process = subprocess.Popen([str(binary), "connect", "--protocol", "chromecast",
                "--address", "127.0.0.1", "--port", str(listener.getsockname()[1]),
                "--device", "isolated fixture"], env=env, stdout=events_log, stderr=log,
                start_new_session=True)
            deadline = time.monotonic() + 20
            while not (loaded.is_set() if args.legacy_transport_only else disconnected.is_set()) and time.monotonic() < deadline and process.poll() is None and not errors:
                time.sleep(0.1)
            assert not errors, errors
            if not args.legacy_transport_only:
                assert disconnected.wait(5), "authentication failure must close the same TLS connection"
                assert process.wait(timeout=10) != 0, "authentication failure must fail the session"
                thread.join(timeout=5)
                assert not thread.is_alive() and not errors, errors
                assert observed == [(PREFIX + "tp.deviceauth", "DEVICE_AUTH_CHALLENGE")]
                assert not loaded.is_set() and not fetched, "no media URL or HLS fetch after failed authentication"
                assert not has_event(events, "Connected"), "failed authentication must not report streaming"
                assert has_event(events, "Ended"), "failed session must end"
                report = {"schema": 2, "artifact_sha256": file_sha256(binary), "tls": tls, "messages": observed,
                          "fetched": fetched, "receiver_authenticated": False, "device_auth_challenge_observed": True,
                          "control_or_media_before_authentication": False, "media_url_disclosed": False,
                          "physical_receiver": False, "media_decoded": False, "clean_disconnect": True}
                args.evidence.parent.mkdir(parents=True, exist_ok=True)
                args.evidence.write_text(json.dumps(report, indent=2) + "\n")
                print("PASS real projector DeviceAuth challenge, fail-closed disconnect, no readiness/control/media URL")
                return
            assert loaded.is_set(), "production LOAD and protected HLS fetch must be observed"
            assert has_event(events, "Connected", timeout=5), "real projector must report the session connected"
            subprocess.run([str(binary), "disconnect"], env=env, stdout=log, stderr=log, check=True, timeout=15)
            assert process.wait(timeout=15) == 0, "production projector must stop successfully"
            assert disconnected.wait(5), "TLS receiver must observe sender disconnect"
            thread.join(timeout=5)
            assert not thread.is_alive() and not errors, errors
            assert {"Ended": {"device_id": "Google Cast:127.0.0.1", "error": None}} in projector_events(events), \
                "the session must end without an error"
            assert not {"ready", "stop", "error", "session.pid"} & {p.name for p in session.glob("*")}, \
                "session state travels as events, never as runtime files"
            report = {"schema": 1, "artifact_sha256": file_sha256(binary), "tls": tls, "messages": observed, "fetched": fetched,
                      "receiver_authenticated": False,
                      "device_auth_challenge_observed": any(namespace == PREFIX + "tp.deviceauth" for namespace, _ in observed),
                      "physical_receiver": False,
                      "media_decoded": False, "clean_disconnect": True}
            args.evidence.parent.mkdir(parents=True, exist_ok=True)
            args.evidence.write_text(json.dumps(report, indent=2) + "\n")
            print("PASS real Cast TLS control, tokenized HLS isolation and clean disconnect; endpoint authentication unqualified")
        except BaseException:
            log.flush()
            print("OBSERVATIONS", {"tls": tls, "messages": observed, "errors": [str(e) for e in errors],
                "process": process.poll() if process else None, "service": service.poll(),
                "files": sorted(p.name for p in session.glob("*"))})
            print("SESSION EVENTS", projector_events(events))
            print((root / "projector.log").read_text(errors="replace")[-4000:])
            raise
        finally:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                for child in (process, service):
                    if child is None:
                        continue
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(child.pid, sig)
                for child in (process, service):
                    if child is None:
                        continue
                    with contextlib.suppress(subprocess.TimeoutExpired):
                        child.wait(timeout=5)
            listener.close()
            thread.join(timeout=20)
            log.close()
            events_log.close()
            assert not thread.is_alive(), "fixture receiver must be reaped"


if __name__ == "__main__":
    main()
