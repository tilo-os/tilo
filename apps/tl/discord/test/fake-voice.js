// A fake Discord voice server for the bridge's tests: a WebSocket gateway handing out a UDP
// address, and that UDP socket doing IP discovery and carrying sealed RTP both ways.

import { createSocket } from "node:dgram";
import { AES_GCM, Cipher, Sender, open } from "../src/rtp.js";

export const KEY = new Uint8Array(32).fill(3);
export const SSRC = 1234;
/// The Discord account the fake's audio is said by.
export const USER = "7001";

/// A fake Discord voice server: a WebSocket gateway handing out a UDP address, and that UDP
/// socket doing IP discovery and carrying sealed RTP both ways.
export function fakeVoiceServer({ modes = [AES_GCM], ssrc = SSRC } = {}) {
  const seen = [];
  const packets = [];
  let client = null;
  let ws = null;
  let udp = null;
  let server = null;
  const cipher = new Cipher(AES_GCM, KEY);
  const speaking = [];

  const socket = createSocket("udp4");
  // A datagram to a client that already left is refused, as on Discord's own server.
  socket.on("error", () => {});
  socket.on("message", (packet, from) => {
    // Discovery: op 1 (request), 70 bytes of padding, the SSRC.
    if (packet.length === 74 && packet.readUInt16BE(0) === 1) {
      client = from;
      const answer = Buffer.alloc(74);
      answer.writeUInt16BE(2, 0);
      answer.writeUInt16BE(70, 2);
      answer.writeUInt32BE(packet.readUInt32BE(4), 4);
      answer.write(from.address, 8, "utf8");
      answer.writeUInt16BE(from.port, 72);
      socket.send(answer, from.port, from.address);
      return;
    }
    const opened = open(cipher, packet);
    if (opened) packets.push(opened);
  });

  return new Promise((resolve) => {
    socket.bind(0, "127.0.0.1", () => {
      const port = socket.address().port;
      server = Bun.serve({
        port: 0,
        fetch(request, server) {
          if (server.upgrade(request)) return;
          return new Response("a voice gateway speaks WebSocket alone", { status: 400 });
        },
        websocket: {
          open(socket) {
            ws = socket;
            socket.send(JSON.stringify({ op: 8, d: { heartbeat_interval: 60 } }));
          },
          message(socket, raw) {
            const frame = JSON.parse(String(raw));
            seen.push(frame);
            if (frame.op === 3) {
              socket.send(JSON.stringify({ op: 6, d: {} }));
              return;
            }
            if (frame.op === 0) {
              socket.send(
                JSON.stringify({
                  op: 2,
                  d: { ssrc, ip: "127.0.0.1", port, modes, heartbeat_interval: 60 },
                }),
              );
              return;
            }
            if (frame.op === 1) {
              socket.send(
                JSON.stringify({
                  op: 4,
                  d: { mode: frame.d.data.mode, secret_key: [...KEY], dave_protocol_version: 0 },
                }),
              );
              // Who speaks on the SSRC this fake sends audio under, as Discord's own gateway
              // says it before any frame arrives.
              socket.send(JSON.stringify({ op: 5, d: { ssrc: 1, user_id: USER } }));
              return;
            }
            if (frame.op === 5) speaking.push(frame.d);
          },
        },
      });
      resolve({
        url: `127.0.0.1:${server.port}`,
        seen,
        packets,
        speaking,
        /// One sealed packet from `USER`, as the voice server sends audio.
        say(opus) {
          const sender = new Sender(1, [0, 1, 0, 0, 0, 2, 1, 2, 3, 4]);
          const packet = sender.packet(cipher, Buffer.from(opus));
          if (!client) throw new Error("no client address yet");
          socket.send(packet, client.port, client.address);
        },
        stop() {
          socket.close();
          server.stop(true);
        },
      });
    });
  });
}

