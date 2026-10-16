#!/usr/bin/env python3
"""Mandatory, isolated exchange with a pinned GPL-3.0 OpenDrop receiver.

Run in a disposable Python environment with libarchive-c, zeroconf, ifaddr,
Pillow and system libarchive/openssl. Pass an unmodified checkout at the exact
revision below. No interfaces are configured and no service is advertised.
The upstream auto-accepting receiver is a test oracle, never a shipped service.
"""

import argparse
import contextlib
import hashlib
import http.server
import importlib.util
import os
from pathlib import Path
import ssl
import subprocess
import sys
import tempfile
import threading
from types import SimpleNamespace
import unittest

REVISION = "11fe7ba7861093b302bc0637e8cb10adf2d29337"
ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("airdrop_sender", ROOT / "tools/protocols/airdrop_sender.py")
sender = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sender)


class AirDropTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="tilo-airdrop-fixture-")
        cls.directory = Path(cls.temp.name)
        cls.contexts = []
        cls.pins = []
        for tag in ("first", "second"):
            cert, key = cls.directory / f"{tag}.pem", cls.directory / f"{tag}.key"
            subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                            "-keyout", str(key), "-out", str(cert), "-days", "1",
                            "-subj", "/CN=isolated-fixture"], check=True, capture_output=True)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            context.load_cert_chain(cert, key)
            cls.contexts.append(context)
            cls.pins.append(hashlib.sha256(ssl.PEM_cert_to_DER_cert(cert.read_text())).hexdigest())

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    @contextlib.contextmanager
    def receiver(self, mode="accept", on_ask=None):
        from opendrop.server import AirDropServerHandler
        observed = []
        errors = []
        rotation = {"active": False}

        class Handler(AirDropServerHandler):
            config = SimpleNamespace(debug=False, record_data=None,
                                     computer_name="Isolated OpenDrop", computer_model="fixture")

            def do_POST(self):
                observed.append(self.path)
                if self.path == "/Ask" and on_ask is not None:
                    on_ask()
                if self.path == "/Ask" and mode == "decline":
                    self.rfile.read(int(self.headers["Content-Length"]))
                    self.send_response(403)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                if mode == "oversize":
                    self.rfile.read(int(self.headers["Content-Length"]))
                    self.send_response(200)
                    self.send_header("Content-Length", str(sender.MAX_RESPONSE_BYTES + 1))
                    self.end_headers()
                    self.wfile.write(b"x" * (sender.MAX_RESPONSE_BYTES + 1))
                    return
                # Positive exchange delegates all wire decoding/extraction to
                # the unmodified pinned upstream production handler.
                super().do_POST()

        def choose_certificate(socket, _name, _context):
            if rotation["active"]:
                socket.context = self.contexts[1]

        class Server(http.server.HTTPServer):
            def handle_error(self, _request, _address):
                error = sys.exc_info()[1]
                if not isinstance(error, (BrokenPipeError, ConnectionResetError)):
                    errors.append(error)

        self.contexts[0].set_servername_callback(choose_certificate)
        server = Server(("127.0.0.1", 0), Handler)
        server.socket = self.contexts[0].wrap_socket(server.socket, server_side=True)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield server.server_port, observed, rotation
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
            self.assertFalse(thread.is_alive(), "fixture receiver must stop")
            self.contexts[0].set_servername_callback(None)
            self.assertEqual(errors, [], "unexpected receiver failures must fail the fixture")

    @contextlib.contextmanager
    def source(self, content=b"real compatible bytes\0\xff", write=False):
        with tempfile.TemporaryDirectory(dir=self.directory) as temp:
            path = Path(temp) / "source"
            path.write_bytes(content)
            fd = os.open(path, os.O_RDWR if write else os.O_RDONLY)
            try:
                yield fd, content
            finally:
                os.close(fd)

    def test_actual_opendrop_extracts_exact_file_after_consent(self):
        with tempfile.TemporaryDirectory(dir=self.directory) as destination:
            previous = os.getcwd()
            os.chdir(destination)
            try:
                with self.source() as (fd, content), self.receiver() as (port, observed, _):
                    pin = sender.inspect_peer("127.0.0.1", port)
                    self.assertEqual(pin, self.pins[0])
                    self.assertEqual(observed, [], "inspection sends no HTTP data")
                    result = sender.send("127.0.0.1", port, pin, fd, "report.bin")
                    self.assertEqual(Path("report.bin").read_bytes(), content)
                    self.assertEqual(observed, ["/Discover", "/Ask", "/Upload"])
                    self.assertEqual(result["content_hash"], hashlib.sha256(content).hexdigest())
                    self.assertEqual(sorted(os.listdir()), ["report.bin"])
            finally:
                os.chdir(previous)

    def test_wrong_pin_sends_no_identity_name_or_content(self):
        with self.source() as (fd, _), self.receiver() as (port, observed, _):
            with self.assertRaises(sender.TransferError):
                sender.send("127.0.0.1", port, self.pins[1], fd, "report.bin")
            self.assertEqual(observed, [])

    def test_source_mutation_after_ask_does_not_change_staged_bytes(self):
        source = self.directory / "mutable-source"
        original = b"immutable approved snapshot"
        source.write_bytes(original)
        fd = os.open(source, os.O_RDONLY)
        previous = os.getcwd()
        try:
            with tempfile.TemporaryDirectory(dir=self.directory) as destination:
                os.chdir(destination)
                with self.receiver(on_ask=lambda: source.write_bytes(b"subsequent edit")) as (port, _, _):
                    sender.send("127.0.0.1", port, self.pins[0], fd, "snapshot.bin")
                    self.assertEqual(Path("snapshot.bin").read_bytes(), original)
                    self.assertEqual(source.read_bytes(), b"subsequent edit")
        finally:
            os.chdir(previous)
            os.close(fd)
            source.unlink()

    def test_reconnect_revalidates_certificate(self):
        with self.receiver() as (port, observed, rotation):
            connection = sender.PinnedConnection("127.0.0.1", port, self.pins[0])
            try:
                sender.post(connection, "/Discover", b"bplist00")
                connection.close()
                rotation["active"] = True
                with self.assertRaises(sender.TransferError):
                    sender.post(connection, "/Discover", b"bplist00")
                self.assertEqual(observed, ["/Discover"])
            finally:
                connection.close()

    def test_receiver_decline_prevents_upload(self):
        with self.source() as (fd, _), self.receiver("decline") as (port, observed, _):
            with self.assertRaisesRegex(sender.TransferError, "/Ask"):
                sender.send("127.0.0.1", port, self.pins[0], fd, "report.bin")
            self.assertEqual(observed, ["/Discover", "/Ask"])

    def test_bounded_child_staging_keeps_approved_bytes_for_opendrop(self):
        import fcntl
        import json
        payload = b"immutable broker-approved bytes"
        staged = os.memfd_create("isolated-airdrop-stage", os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
        readonly = None
        source_path = self.directory / "broker-stage-source"
        source_path.write_bytes(payload)
        source = os.open(source_path, os.O_RDONLY | os.O_CLOEXEC)
        previous = os.getcwd()
        try:
            command = [sys.executable, "-I", str(ROOT / "tools/protocols/airdrop_sender.py"),
                       "127.0.0.1", "1", "stage", "--source-fd", str(source),
                       "--staged-fd", str(staged), "--name=report.bin"]
            reply = subprocess.run(command, pass_fds=(source, staged), check=True,
                                   capture_output=True, text=True, timeout=10)
            self.assertEqual(json.loads(reply.stdout)["bytes"], len(payload))
            fcntl.fcntl(staged, fcntl.F_ADD_SEALS, fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW
                        | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL)
            readonly = os.open(f"/proc/self/fd/{staged}", os.O_RDONLY | os.O_CLOEXEC)
            source_path.write_bytes(b"later unapproved source contents")
            with tempfile.TemporaryDirectory(dir=self.directory) as destination:
                os.chdir(destination)
                with self.receiver() as (port, observed, _):
                    sender.send("127.0.0.1", port, self.pins[0], readonly, "report.bin")
                    self.assertEqual(observed, ["/Discover", "/Ask", "/Upload"])
                    self.assertEqual(Path("report.bin").read_bytes(), payload)
        finally:
            os.chdir(previous)
            os.close(source)
            if readonly is not None:
                os.close(readonly)
            os.close(staged)

    def test_staging_refuses_unapproved_writable_source_and_nonempty_destination(self):
        with tempfile.TemporaryFile() as output, self.source(write=True) as (source, _):
            with self.assertRaisesRegex(sender.TransferError, "read-only"):
                sender.stage(source, output.fileno(), "report.bin")
        with tempfile.TemporaryFile() as output, self.source() as (source, _):
            output.write(b"existing")
            output.flush()
            with self.assertRaisesRegex(sender.TransferError, "empty writable"):
                sender.stage(source, output.fileno(), "report.bin")

    def test_oversized_response_stops_before_ask(self):
        with self.source() as (fd, _), self.receiver("oversize") as (port, observed, _):
            with self.assertRaisesRegex(sender.TransferError, "protocol limit"):
                sender.send("127.0.0.1", port, self.pins[0], fd, "report.bin")
            self.assertEqual(observed, ["/Discover"])

    def test_descriptor_and_archive_boundaries_fail_before_network(self):
        with self.source(write=True) as (fd, _):
            with self.assertRaisesRegex(sender.TransferError, "read-only"):
                sender.send("127.0.0.1", 1, self.pins[0], fd, "report.bin")
        with self.source() as (fd, _):
            for name in ("../escape", "/absolute", "a\\b", "..", "bad\nname", "x" * 256,
                         "spoof\u202etxt.exe", "spoof\u2066.txt"):
                with self.assertRaises(sender.TransferError):
                    sender.send("127.0.0.1", 1, self.pins[0], fd, name)
            for pin in ("", "00", "g" * 64):
                with self.assertRaises(sender.TransferError):
                    sender.send("127.0.0.1", 1, pin, fd, "report.bin")
        with self.source(b"x" * (sender.MAX_FILE_BYTES + 1)) as (fd, _):
            with self.assertRaisesRegex(sender.TransferError, "16 MiB"):
                sender.snapshot(fd, "large.bin")
        read, write = os.pipe()
        try:
            with self.assertRaisesRegex(sender.TransferError, "regular file"):
                sender.snapshot(read, "pipe.bin")
        finally:
            os.close(read)
            os.close(write)
        with self.assertRaises(sender.TransferError):
            sender.endpoint("receiver.example", 8770)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--opendrop-checkout", type=Path, required=True)
    args = parser.parse_args()
    actual = subprocess.check_output(["git", "-C", str(args.opendrop_checkout), "rev-parse", "HEAD"], text=True).strip()
    dirty = subprocess.check_output(["git", "-C", str(args.opendrop_checkout), "status", "--porcelain", "--untracked-files=no"], text=True).strip()
    if actual != REVISION or dirty:
        raise SystemExit("fixture requires the unmodified pinned OpenDrop revision")
    sys.path.insert(0, str(args.opendrop_checkout))
    unittest.main(argv=[sys.argv[0]], verbosity=2)
