#!/usr/bin/env python3
"""Drive FluxCast's production WFD RTSP server as a strict simulated sink."""

import socket
import time

from wfd.config import WFDMediaConfig
from wfd.rtsp.message import _read_rtsp_message
from wfd.rtsp.rtsp_server import WFDRTSPServer
import wfd.rtsp.handler as handler


class FakeProcess:
    returncode = None

    def poll(self):
        return None


class FakeMediaPipeline:
    started = False
    stopped = False

    def __init__(self, config, tv_ip, local_ip, sink_rtp_port):
        assert sink_rtp_port == 19000
        self.config = config
        self.processes = [FakeProcess()]
        self.tx_interface = None

    def start(self):
        FakeMediaPipeline.started = True

    def stop(self):
        FakeMediaPipeline.stopped = True


def send(stream, start, cseq, body="", headers=()):
    lines = [start, f"CSeq: {cseq}", *headers]
    if body:
        lines.extend(["Content-Type: text/parameters", f"Content-Length: {len(body.encode())}"])
    else:
        lines.append("Content-Length: 0")
    stream.write(("\r\n".join(lines) + "\r\n\r\n" + body).encode())
    stream.flush()


def expect(reader, prefix):
    message = _read_rtsp_message(reader)
    assert message is not None and message.start.startswith(prefix), message
    return message


handler.WFDMediaPipeline = FakeMediaPipeline
server = WFDRTSPServer(
    WFDMediaConfig(monitor=None, test_pattern=True, no_audio=True),
    host="127.0.0.1",
    port=0,
)
server.start()
port = server._server.server_address[1]

with socket.create_connection(("127.0.0.1", port), timeout=3) as sock:
    reader = sock.makefile("rb")
    writer = sock.makefile("wb")

    m1 = expect(reader, "OPTIONS * RTSP/1.0")
    send(writer, "RTSP/1.0 200 OK", m1.cseq)
    send(writer, "OPTIONS * RTSP/1.0", 101, headers=("Require: org.wfa.wfd1.0",))
    expect(reader, "RTSP/1.0 200 OK")

    m3 = expect(reader, "GET_PARAMETER ")
    capabilities = (
        "wfd_content_protection: none\r\n"
        "wfd_video_formats: 00 00 02 10 000001e1 00000000 00000000 00 0000 0000 00 none none\r\n"
        "wfd_audio_codecs: none\r\n"
        "wfd_client_rtp_ports: RTP/AVP/UDP;unicast 19000 0 mode=play\r\n"
    )
    send(writer, "RTSP/1.0 200 OK", m3.cseq, capabilities)

    m4 = expect(reader, "SET_PARAMETER ")
    assert "wfd_presentation_URL:" in m4.body
    assert "wfd_video_formats:" in m4.body
    send(writer, "RTSP/1.0 200 OK", m4.cseq)

    m5 = expect(reader, "SET_PARAMETER ")
    assert "wfd_trigger_method: SETUP" in m5.body
    send(writer, "RTSP/1.0 200 OK", m5.cseq)

    send(
        writer,
        f"SETUP rtsp://127.0.0.1:{port}/wfd1.0/streamid=0 RTSP/1.0",
        102,
        headers=("Transport: RTP/AVP/UDP;unicast;client_port=19000",),
    )
    setup = expect(reader, "RTSP/1.0 200 OK")
    session = setup.headers["session"].split(";", 1)[0]

    send(
        writer,
        f"PLAY rtsp://127.0.0.1:{port}/wfd1.0 RTSP/1.0",
        103,
        headers=(f"Session: {session}",),
    )
    expect(reader, "RTSP/1.0 200 OK")
    deadline = time.monotonic() + 2
    while not FakeMediaPipeline.started and time.monotonic() < deadline:
        time.sleep(0.01)
    assert FakeMediaPipeline.started, "PLAY did not start the negotiated media pipeline"

    send(
        writer,
        f"TEARDOWN rtsp://127.0.0.1:{port}/wfd1.0 RTSP/1.0",
        104,
        headers=(f"Session: {session}",),
    )
    expect(reader, "RTSP/1.0 200 OK")

server.stop()
assert FakeMediaPipeline.stopped, "TEARDOWN did not stop the production media lifecycle"
