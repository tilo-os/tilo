"""Tests of the OS image size report (tools/perf/image_size.py)."""

from __future__ import annotations

import gzip
import hashlib
import io
import json
import pathlib
import sqlite3
import struct
import sys
import tarfile
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import image_size  # noqa: E402

FONT = b"F" * 300_000
WINE = b"W" * 500_000
SDK = b"S" * 200_000
LIB = b"L" * 40_000
REVISION = "0123456789abcdef0123456789abcdef01234567"


def rpm_blob(tags: dict[int, object]) -> bytes:
    """An rpmdb.sqlite Packages blob: header index and data store, no magic."""
    index, store = [], b""
    for tag, value in tags.items():
        if isinstance(value, int):
            kind, data, count = 4, struct.pack(">I", value), 1
        elif isinstance(value, str):
            kind, data, count = 6, value.encode() + b"\0", 1
        else:
            kind, data, count = 8, b"".join(item.encode() + b"\0" for item in value), len(value)
        index.append(struct.pack(">iiii", tag, kind, len(store), count))
        store += data
    return struct.pack(">II", len(index), len(store)) + b"".join(index) + store


def package(name: str, size: int, provides=(), requires=(), recommends=()) -> bytes:
    tags: dict[int, object] = {1000: name, 1001: "1.0", 1002: "1.fc44", 1009: size,
                               1047: [name, *provides]}
    if requires:
        tags[1049] = list(requires)
    if recommends:
        tags[5046] = list(recommends)
    return rpm_blob(tags)


def rpmdb() -> bytes:
    with tempfile.TemporaryDirectory() as directory:
        path = pathlib.Path(directory) / "rpmdb.sqlite"
        connection = sqlite3.connect(path)
        connection.execute("CREATE TABLE Packages (hnum INTEGER PRIMARY KEY, blob BLOB NOT NULL)")
        for blob in (
            package("python3-botocore", 120_000_000, provides=["python3.14dist(botocore)"]),
            # Fedora's python requirements are rich dependencies.
            package("python3-boto3", 1_000_000, requires=[
                "(python3.14dist(botocore) < 1.44~~ with python3.14dist(botocore) >= 1.43.99)"]),
            package("cloud-thing", 10_000, recommends=["python3-boto3"]),
            package("wine-core", 1_400_000_000),
            package("gpg-pubkey", 0),
        ):
            connection.execute("INSERT INTO Packages (blob) VALUES (?)", (blob,))
        connection.commit()
        connection.close()
        return path.read_bytes()


def layer(entries: list[tuple]) -> bytes:
    """A gzip layer tar from (kind, path, data-or-target) entries."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for kind, path, payload in entries:
            info = tarfile.TarInfo(path)
            if kind == "dir":
                info.type = tarfile.DIRTYPE
                archive.addfile(info)
            elif kind == "link":
                info.type, info.linkname = tarfile.LNKTYPE, payload
                archive.addfile(info)
            elif kind == "symlink":
                info.type, info.linkname = tarfile.SYMTYPE, payload
                archive.addfile(info)
            else:
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
    return gzip.compress(buffer.getvalue(), mtime=0)


class Layout:
    def __init__(self, root: pathlib.Path) -> None:
        self.root = root
        (root / "blobs" / "sha256").mkdir(parents=True)

    def put(self, data: bytes) -> dict:
        digest = hashlib.sha256(data).hexdigest()
        (self.root / "blobs" / "sha256" / digest).write_bytes(data)
        return {"digest": f"sha256:{digest}", "size": len(data)}

    def image(self, layers: list[bytes], created_by: list[str]) -> str:
        descriptors, diff_ids = [], []
        for blob in layers:
            descriptors.append({"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", **self.put(blob)})
            diff_ids.append("sha256:" + hashlib.sha256(gzip.decompress(blob)).hexdigest())
        config = {"architecture": "amd64", "os": "linux",
                  "config": {"Labels": {"org.opencontainers.image.revision": REVISION,
                                        "org.tilo.rust-lto": "off", "org.tilo.with-windows": "1"}},
                  "rootfs": {"type": "layers", "diff_ids": diff_ids},
                  "history": [{"created_by": text} for text in created_by]
                  + [{"created_by": "LABEL a=b", "empty_layer": True}]}
        manifest = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                               **self.put(json.dumps(config).encode())},
                    "layers": descriptors}
        described = self.put(json.dumps(manifest).encode())
        index = {"schemaVersion": 2, "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json", **described,
            "platform": {"architecture": "amd64", "os": "linux"}}]}
        (self.root / "index.json").write_text(json.dumps(index))
        (self.root / "oci-layout").write_text('{"imageLayoutVersion": "1.0.0"}')
        return described["digest"]


def fixture(root: pathlib.Path) -> str:
    """Three layers: a base, fonts shipped twice, then a cleanup with whiteouts."""
    layout = Layout(root)
    base = layer([
        ("dir", "usr", None), ("dir", "usr/share/rpm", None),
        ("file", "usr/share/rpm/rpmdb.sqlite", b"stale"),
        ("file", "usr/bin/tool", b"T" * 1000),
        ("dir", "usr/lib64/wine-wow64", None),
        ("file", "usr/lib64/wine-wow64/wine.dll", WINE),
        # The base's own ostree repository, hard-linked to its /usr.
        ("file", "sysroot/ostree/repo/objects/aa/sdk.file", SDK),
        ("link", "usr/lib/python3/botocore/data.json", "sysroot/ostree/repo/objects/aa/sdk.file"),
        ("file", "sysroot/ostree/repo/objects/bb/lib.file", LIB),
        ("link", "usr/lib64/libkeep.so", "sysroot/ostree/repo/objects/bb/lib.file"),
        ("file", "etc/old.conf", b"x" * 100),
        ("dir", "var/cache/junk", None),
        ("file", "var/cache/junk/a", b"a" * 50),
    ])
    fonts = layer([
        ("file", "usr/lib/tilo-runtime/v1/lib/fonts/Noto.ttf", FONT),
        ("file", "usr/share/tilo/font-catalogs/abc/Noto.ttf", FONT),
        ("link", "usr/share/fonts/tilo/Noto.ttf", "usr/lib/tilo-runtime/v1/lib/fonts/Noto.ttf"),
        ("symlink", "usr/share/fonts/tilo/link.ttf", "Noto.ttf"),
    ])
    cleanup = layer([
        ("file", "usr/share/rpm/rpmdb.sqlite", rpmdb()),
        ("file", "etc/.wh.old.conf", b""),
        ("file", "usr/lib/python3/.wh.botocore", b""),
        ("dir", "var/cache/junk", None),
        ("file", "var/cache/junk/.wh..wh..opq", b""),
        ("file", "var/cache/junk/b", b"b" * 70),
        ("file", "usr/bin/tool", b"U" * 1200),
    ])
    return layout.image([base, fonts, cleanup], [
        "RUN |2 A=1 B=2 /bin/sh -c dnf install -y wine # buildkit",
        "COPY /fonts / # buildkit",
        "RUN /bin/sh -c finalize # buildkit",
    ])


class Report(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(self.directory, ignore_errors=True))
        self.digest = fixture(self.directory)

    def run_report(self, *extra: str) -> tuple[int, dict]:
        out = self.directory / "report.json"
        status = image_size.main(["--oci-layout", str(self.directory), "--json", str(out), "--quiet", *extra])
        return status, json.loads(out.read_text()) if out.exists() else {}

    def test_totals_replay_whiteouts_and_count_links_once(self) -> None:
        status, data = self.run_report()
        self.assertEqual(status, 0)
        totals = data["totals"]
        db = len(rpmdb())
        # Merged root: tool (replaced, 1200), wine.dll, both font copies, the
        # rpmdb, junk/b; etc/old.conf and junk/a are whited out, the hard link
        # and the symlink carry no content.
        # The removed package's bytes stay in the undeployed /sysroot; the
        # library /usr still names is deployed, once.
        self.assertEqual(totals["content"], 1200 + len(WINE) + 2 * len(FONT) + db + 70 + len(LIB))
        self.assertEqual(totals["unique_content"], 1200 + len(WINE) + len(FONT) + db + 70 + len(LIB))
        # Everything but /var/cache/junk/b: what the image-size budget gates.
        self.assertEqual(totals["usr_content"], 1200 + len(WINE) + 2 * len(FONT) + db + len(LIB))
        self.assertEqual(totals["undeployed"], len(SDK))
        layers = [5 + 1000 + len(WINE) + len(SDK) + len(LIB) + 100 + 50, 2 * len(FONT), db + 70 + 1200]
        self.assertEqual([item["payload"] for item in data["layers"]], layers)
        self.assertEqual(totals["payload"], sum(layers))
        # One font copy too many; the replaced tool and rpmdb differ in content
        # from what they replaced, so they are not redundant.
        self.assertEqual(totals["redundant_payload"], len(FONT))
        self.assertEqual(totals["hard_links"], 2)
        self.assertEqual(totals["files"], 7)
        self.assertIn(["/usr/lib64/libkeep.so", len(LIB)], data["files"])
        self.assertEqual(totals["compressed"], sum(item["compressed"] for item in data["layers"]))
        self.assertEqual(data["redundant"][0]["paths"], [
            "/usr/lib/tilo-runtime/v1/lib/fonts/Noto.ttf", "/usr/share/tilo/font-catalogs/abc/Noto.ttf"])
        self.assertEqual(data["revision"], REVISION)
        self.assertEqual(data["manifest_digest"], self.digest)

    def test_layers_name_their_instruction_without_build_arguments(self) -> None:
        _, data = self.run_report()
        self.assertEqual([item["created_by"] for item in data["layers"]], [
            "RUN /bin/sh -c dnf install -y wine", "COPY /fonts /", "RUN /bin/sh -c finalize"])

    def test_packages_come_from_the_merged_rpmdb(self) -> None:
        _, data = self.run_report()
        self.assertEqual(data["packages"][:2], [["wine-core", 1_400_000_000], ["python3-botocore", 120_000_000]])
        self.assertNotIn("gpg-pubkey", [name for name, _ in data["packages"]])
        self.assertEqual(data["totals"]["packages"], 4)

    def test_why_walks_requires_and_recommends_to_a_root(self) -> None:
        status, data = self.run_report("--why", "python3-botocore", "--why", "absent")
        self.assertEqual(status, 0)
        self.assertEqual(data["why"]["python3-botocore"], [
            "python3-botocore <- python3-boto3 requires "
            "(python3.14dist(botocore) < 1.44~~ with python3.14dist(botocore) >= 1.43.99)",
            "  python3-boto3 <- cloud-thing recommends python3-boto3",
            "    cloud-thing: nothing requires it (installed by name)",
        ])
        self.assertEqual(data["why"]["absent"], ["absent is not installed"])

    def test_records_carry_the_image_revision(self) -> None:
        records = self.directory / "image.jsonl"
        status, data = self.run_report("--records", str(records))
        self.assertEqual(status, 0)
        lines = [json.loads(line) for line in records.read_text().splitlines()]
        self.assertEqual([line["kpi"] for line in lines], [
            "footprint.image.compressed_bytes.x86_64", "footprint.image.unique_content_bytes.x86_64",
            "footprint.image.redundant_payload_bytes.x86_64"])
        self.assertEqual({line["commit"] for line in lines}, {REVISION})
        self.assertEqual({line["profile"] for line in lines}, {"dev"})
        self.assertEqual(lines[1]["p50"], data["totals"]["unique_content"])
        self.assertEqual({line["unit"] for line in lines}, {"bytes"})

    def test_a_corrupt_layer_fails(self) -> None:
        blobs = sorted((self.directory / "blobs" / "sha256").iterdir(), key=lambda path: -path.stat().st_size)
        data = bytearray(blobs[0].read_bytes())
        data[-9] ^= 0xFF  # inside the gzip trailer: decompression may pass, the digest cannot
        blobs[0].write_bytes(bytes(data))
        status, _ = self.run_report()
        self.assertEqual(status, 1)

    def test_a_missing_rpmdb_fails_why(self) -> None:
        layout = Layout(self.directory / "bare")
        layout.image([layer([("file", "usr/bin/x", b"x")])], ["COPY x /"])
        status = image_size.main(["--oci-layout", str(self.directory / "bare"), "--quiet",
                                  "--why", "python3-botocore"])
        self.assertEqual(status, 1)


class Update(unittest.TestCase):
    """What a device downloads moving between two builds: the layers with new digests."""

    def setUp(self) -> None:
        self.directory = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(self.directory, ignore_errors=True))
        self.base = layer([("file", "usr/lib/libbase.so", b"B" * 5000)])
        self.app_v1 = layer([("file", "usr/share/tilo-apps/imager.tapp", b"1" * 3000)])
        self.app_v2 = layer([("file", "usr/share/tilo-apps/imager.tapp", b"2" * 3000)])
        Layout(self.directory / "old").image([self.base, self.app_v1], ["base", "apps"])
        Layout(self.directory / "new").image([self.base, self.app_v2], ["base", "apps"])

    def test_only_new_layer_digests_are_downloaded(self) -> None:
        out = self.directory / "update.json"
        status = image_size.main(["--oci-layout", str(self.directory / "new"), "--quiet", "--json", str(out),
                                  "--update-from", f"oci:{self.directory / 'old'}", "--update-only"])
        self.assertEqual(status, 0)
        moved = json.loads(out.read_text())["update"]
        self.assertEqual(moved["layers"], 2)
        self.assertEqual(moved["new_layers"], 1)
        self.assertEqual(moved["bytes"], len(self.app_v2))
        self.assertEqual(moved["compressed"], len(self.base) + len(self.app_v2))
        self.assertEqual(moved["from_revision"], REVISION)

    def test_the_full_report_carries_the_update(self) -> None:
        out = self.directory / "report.json"
        status = image_size.main(["--oci-layout", str(self.directory / "new"), "--quiet", "--json", str(out),
                                  "--update-from", f"oci:{self.directory / 'new'}"])
        self.assertEqual(status, 0)
        data = json.loads(out.read_text())
        self.assertEqual(data["update"]["bytes"], 0)
        self.assertEqual(data["totals"]["layers"], 2)

    def test_update_only_needs_an_old_image(self) -> None:
        self.assertEqual(image_size.main(["--oci-layout", str(self.directory / "new"), "--quiet",
                                          "--update-only"]), 1)


class Resuming(unittest.TestCase):
    def test_a_dropped_download_resumes_at_its_offset(self) -> None:
        data = bytes(range(256)) * 64
        starts = []

        class Flaky(io.BytesIO):
            def read(self, size=-1):
                if len(starts) == 1 and self.tell() >= 1000:
                    raise TimeoutError("The read operation timed out")
                return super().read(min(size, 500))

        def opener(start: int) -> io.BytesIO:
            starts.append(start)
            return Flaky(data[start:])

        stream = io.BufferedReader(image_size.Resuming(opener), 4096)
        self.assertEqual(stream.read(), data)
        self.assertEqual(starts, [0, 1000])

    def test_a_download_that_keeps_failing_fails(self) -> None:
        class Dead(io.BytesIO):
            def read(self, size=-1):
                raise TimeoutError("The read operation timed out")

        stream = io.BufferedReader(image_size.Resuming(lambda start: Dead()), 4096)
        with self.assertRaisesRegex(ValueError, "download failed 5 times at byte 0"):
            stream.read()


class TokenRegistry(unittest.TestCase):
    """A registry that wants a pull token and redirects blobs to a store that
    refuses a second authorization, as registry.tilo.so does inside CI."""

    def test_the_token_is_not_carried_to_the_blob_store(self) -> None:
        import http.server
        import threading

        body = b"layer bytes" * 100

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args) -> None:
                pass

            def reply(self, code: int, data: bytes = b"", headers: dict | None = None) -> None:
                self.send_response(code)
                for name, value in (headers or {}).items():
                    self.send_header(name, value)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self) -> None:
                port = self.server.server_address[1]
                authorized = self.headers.get("Authorization") == "Bearer t0ken"
                if self.path.startswith("/token"):
                    self.reply(200, b'{"token": "t0ken"}')
                elif self.path.startswith("/v2/tilo/x/blobs/") and not authorized:
                    self.reply(401, headers={"WWW-Authenticate":
                                             f'Bearer realm="http://127.0.0.1:{port}/token",service="r"'})
                elif self.path.startswith("/v2/tilo/x/blobs/"):
                    self.reply(307, headers={"Location": f"http://127.0.0.1:{port}/store/blob"})
                elif self.path == "/store/blob" and self.headers.get("Authorization"):
                    self.reply(400, b"only one auth mechanism allowed")
                elif self.path == "/store/blob":
                    self.reply(200, body)
                else:
                    self.reply(404)

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.shutdown)
        registry = image_size.Registry(f"127.0.0.1:{server.server_address[1]}", "tilo/x", scheme="http")
        with registry.blob("sha256:" + "0" * 64) as stream:
            self.assertEqual(stream.read(), body)
        self.assertEqual(registry.token, "t0ken")


class Capabilities(unittest.TestCase):
    def test_rich_dependencies_name_their_capabilities(self) -> None:
        self.assertEqual(image_size.capabilities("python(abi)"), {"python(abi)"})
        self.assertEqual(image_size.capabilities(
            "((python3.14dist(urllib3) < 2.2 or python3.14dist(urllib3) > 2.2) with python3.14dist(urllib3) >= 1)"),
            {"python3.14dist(urllib3)"})
        self.assertEqual(image_size.capabilities("(foo if bar else baz)"), {"foo", "bar", "baz"})


class References(unittest.TestCase):
    def test_parse(self) -> None:
        self.assertEqual(image_size.parse_reference("registry.tilo.so/tilo/tilo-ci:main-abc-x86_64"),
                         ("registry.tilo.so", "tilo/tilo-ci", "main-abc-x86_64"))
        digest = "sha256:" + "a" * 64
        self.assertEqual(image_size.parse_reference(f"registry.tilo.so:5000/tilo/tilo@{digest}"),
                         ("registry.tilo.so:5000", "tilo/tilo", digest))
        self.assertEqual(image_size.parse_reference("registry.tilo.so/tilo/tilo"),
                         ("registry.tilo.so", "tilo/tilo", "latest"))
        with self.assertRaises(ValueError):
            image_size.parse_reference("tilo/tilo:latest")

    def test_platform_selection(self) -> None:
        index = {"manifests": [
            {"digest": "sha256:" + "1" * 64, "platform": {"architecture": "amd64", "os": "linux"}},
            {"digest": "sha256:" + "2" * 64, "platform": {"architecture": "arm64", "os": "linux"}}]}
        self.assertEqual(image_size.select_platform(index, "aarch64"), "sha256:" + "2" * 64)
        with self.assertRaises(ValueError):
            image_size.select_platform(index, None)


if __name__ == "__main__":
    unittest.main()
