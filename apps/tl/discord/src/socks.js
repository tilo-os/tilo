// Discord's voice media leaves this app as UDP, and a sandboxed app sends its UDP through the
// device's egress proxy: the one it names in `ALL_PROXY` as a SOCKS5 address, as `socks5h` so a
// name is resolved by the proxy and never here. HTTP libraries read that variable themselves;
// nothing in Node or Bun does for UDP, so this speaks the protocol: a TCP control connection
// through which the proxy is asked for a UDP association, and the datagrams each wrapped with
// the SOCKS5 header naming the voice server. The control connection stays open for as long as
// the association is wanted.

import { EventEmitter } from "node:events";
import { createSocket } from "node:dgram";
import { createConnection } from "node:net";

const SOCKS5 = 0x05;
const NO_AUTH = 0x00;
const UDP_ASSOCIATE = 0x03;
const REPLY_SUCCEEDED = 0x00;
const ATYP_IPV4 = 0x01;
const ATYP_DOMAIN = 0x03;
const ATYP_IPV6 = 0x04;
/// How long the proxy has to answer the association before the call goes without it.
const ASSOCIATE_TIMEOUT_MS = 5_000;

/// The proxy the app's UDP goes through, from the environment its sandbox sets: `null` when it
/// names none, which is a bridge run outside a sandbox.
export function callProxy(env = process.env) {
  const named = String(env.ALL_PROXY ?? env.all_proxy ?? "");
  const match = /^socks5h?:\/\/(?:[^@/]*@)?(\[[^\]]+\]|[^:/]+):(\d+)\/?$/.exec(named);
  if (!match) return null;
  return { host: match[1].replace(/^\[|\]$/g, ""), port: Number(match[2]) };
}

/// A UDP socket reaching `target` through the proxy's UDP association, with the interface a
/// datagram socket has: `connect(port, ip, callback)`, `send(packet, callback)`,
/// `on("message")`, `close()`.
export function proxyUdp(proxy, { control, socket } = {}) {
  return new ProxyUdp(proxy, {
    control: control ?? ((options) => createConnection(options)),
    socket: socket ?? (() => createSocket("udp4")),
  });
}

class ProxyUdp extends EventEmitter {
  constructor(proxy, { control, socket }) {
    super();
    this.proxy = proxy;
    this.control = control;
    this.socket = socket;
    this.connection = null;
    this.udp = null;
    this.target = null;
    this.closed = false;
  }

  /// Open the association and aim it at the voice server at `ip`:`port`; `callback` is called
  /// once the socket is ready, and an `error` is emitted when it is not.
  connect(port, ip, callback) {
    this.#open(port, ip).then(
      () => callback?.(),
      (error) => this.emit("error", error),
    );
  }

  async #open(port, ip) {
    if (this.closed) throw new Error("the call's UDP is closed");
    const connection = this.control({ host: this.proxy.host, port: this.proxy.port });
    this.connection = connection;
    const reader = new Reader(connection);
    await withTimeout(
      once(connection, "connect"),
      ASSOCIATE_TIMEOUT_MS,
      "the egress proxy did not answer",
    );
    // No authentication: the sandbox's proxy admits the app by its socket, not by a password.
    connection.write(Buffer.from([SOCKS5, 0x01, NO_AUTH]));
    const greeting = await reader.exact(2);
    if (greeting[0] !== SOCKS5 || greeting[1] !== NO_AUTH) {
      throw new Error("the egress proxy refused the SOCKS5 greeting");
    }
    // UDP ASSOCIATE for a client address it need not know: the reply names where to send.
    connection.write(
      Buffer.from([SOCKS5, UDP_ASSOCIATE, 0x00, ATYP_IPV4, 0, 0, 0, 0, 0, 0]),
    );
    const head = await reader.exact(4);
    if (head[0] !== SOCKS5 || head[1] !== REPLY_SUCCEEDED) {
      throw new Error(`the egress proxy refused the call's UDP (reply ${head[1]})`);
    }
    const relay = await readEndpoint(reader, head[3]);
    const udp = this.socket();
    this.udp = udp;
    udp.on("message", (packet) => {
      const opened = unwrap(packet);
      if (opened) this.emit("message", opened.data);
    });
    udp.on("error", (error) => this.emit("error", error));
    await new Promise((resolve, reject) => {
      const failed = (error) => reject(error);
      udp.once("error", failed);
      udp.connect(relay.port, relay.address, () => {
        udp.off("error", failed);
        resolve();
      });
    });
    this.target = { address: ip, port };
    // The association lives as long as its control connection: the proxy drops the UDP path
    // when it closes, so this socket ends with it.
    connection.on("close", () => this.close());
    connection.on("error", () => this.close());
    connection.resume();
  }

  send(packet, callback) {
    if (!this.udp || !this.target) {
      callback?.(new Error("the call's UDP is not open"));
      return;
    }
    this.udp.send(wrap(this.target, packet), callback);
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    try {
      this.udp?.close();
    } catch {}
    try {
      this.connection?.destroy();
    } catch {}
    this.udp = null;
    this.connection = null;
  }
}

/// One endpoint as SOCKS5 writes it: its address type and bytes, and its port.
function endpoint({ address, port }) {
  const bytes = address.includes(":") ? ipv6Bytes(address) : ipv4Bytes(address);
  if (bytes) {
    return Buffer.concat([
      Buffer.from([bytes.length === 4 ? ATYP_IPV4 : ATYP_IPV6]),
      bytes,
      portBytes(port),
    ]);
  }
  const name = Buffer.from(String(address), "utf8");
  return Buffer.concat([
    Buffer.from([ATYP_DOMAIN, name.length]),
    name,
    portBytes(port),
  ]);
}

/// A datagram the way the proxy carries it: a reserved byte pair, no fragmentation, and the
/// endpoint it is for.
export function wrap(target, packet) {
  return Buffer.concat([Buffer.from([0x00, 0x00, 0x00]), endpoint(target), packet]);
}

/// The endpoint and payload a datagram the proxy relays carries; `null` for one it does not
/// relay, which is dropped.
export function unwrap(packet) {
  if (!Buffer.isBuffer(packet) || packet.length < 4 || packet[2] !== 0x00) return null;
  const atyp = packet[3];
  let at = 4;
  let address;
  if (atyp === ATYP_IPV4) {
    if (packet.length < at + 4 + 2) return null;
    address = [...packet.subarray(at, at + 4)].join(".");
    at += 4;
  } else if (atyp === ATYP_IPV6) {
    if (packet.length < at + 16 + 2) return null;
    address = ipv6Text(packet.subarray(at, at + 16));
    at += 16;
  } else if (atyp === ATYP_DOMAIN) {
    if (packet.length < at + 1) return null;
    const len = packet[at];
    at += 1;
    if (packet.length < at + len + 2) return null;
    address = packet.subarray(at, at + len).toString("utf8");
    at += len;
  } else {
    return null;
  }
  const port = packet.readUInt16BE(at);
  at += 2;
  return { target: { address, port }, data: packet.subarray(at) };
}

function portBytes(port) {
  const out = Buffer.alloc(2);
  out.writeUInt16BE(Number(port), 0);
  return out;
}

function ipv4Bytes(address) {
  const parts = String(address).split(".");
  if (parts.length !== 4) return null;
  const bytes = parts.map((part) => Number(part));
  if (bytes.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) return null;
  return Buffer.from(bytes);
}

function ipv6Bytes(address) {
  try {
    const groups = ipv6Groups(address);
    if (!groups) return null;
    const out = Buffer.alloc(16);
    groups.forEach((group, at) => out.writeUInt16BE(group, at * 2));
    return out;
  } catch {
    return null;
  }
}

/// The eight groups of an IPv6 address, `::` expanded, or `null` when it is none.
function ipv6Groups(address) {
  let text = String(address);
  const zone = text.indexOf("%");
  if (zone >= 0) text = text.slice(0, zone);
  const halves = text.split("::");
  if (halves.length > 2) return null;
  const left = halves[0] ? halves[0].split(":") : [];
  const right = halves.length === 2 && halves[1] ? halves[1].split(":") : [];
  const missing = 8 - left.length - right.length;
  if (halves.length === 1 && left.length !== 8) return null;
  const groups = [];
  for (const part of left) groups.push(Number.parseInt(part, 16));
  for (let at = 0; at < missing; at += 1) groups.push(0);
  for (const part of right) groups.push(Number.parseInt(part, 16));
  if (groups.length !== 8 || groups.some((group) => !Number.isInteger(group))) return null;
  return groups;
}

/// An IPv6 address as text, its longest run of zeros folded to `::`.
function ipv6Text(bytes) {
  const groups = [];
  for (let at = 0; at < 16; at += 2) groups.push(bytes.readUInt16BE(at).toString(16));
  let best = { at: -1, length: 0 };
  for (let at = 0; at < 8; at += 1) {
    if (groups[at] !== "0") continue;
    let end = at;
    while (end < 8 && groups[end] === "0") end += 1;
    if (end - at > best.length) best = { at, length: end - at };
    at = end;
  }
  if (best.length < 2) return groups.join(":");
  return `${groups.slice(0, best.at).join(":")}::${groups.slice(best.at + best.length).join(":")}`;
}

/// `endpoint` as read from a SOCKS5 reply, after the four-byte head that names its type.
async function readEndpoint(reader, atyp) {
  if (atyp === ATYP_IPV4) {
    const raw = await reader.exact(4);
    return readPort(reader, [...raw].join("."));
  }
  if (atyp === ATYP_IPV6) {
    const raw = await reader.exact(16);
    return readPort(reader, ipv6Text(raw));
  }
  if (atyp === ATYP_DOMAIN) {
    const len = (await reader.exact(1))[0];
    const name = await reader.exact(len);
    return readPort(reader, name.toString("utf8"));
  }
  throw new Error(`the egress proxy named no address it knows (type ${atyp})`);
}

async function readPort(reader, address) {
  const raw = await reader.exact(2);
  return { address, port: raw.readUInt16BE(0) };
}

/// A stream's bytes, as much at a time as is asked for.
class Reader {
  constructor(stream) {
    this.stream = stream;
    this.buffer = Buffer.alloc(0);
    this.ended = false;
    this.error = null;
    this.waiting = [];
    stream.on("data", (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      this.#wake();
    });
    stream.on("close", () => {
      this.ended = true;
      this.#wake();
    });
    stream.on("error", (error) => {
      this.error = error;
      this.#wake();
    });
  }

  async exact(count) {
    while (this.buffer.length < count) {
      if (this.error) throw this.error;
      if (this.ended) throw new Error("the egress proxy closed the connection");
      await new Promise((resolve) => this.waiting.push(resolve));
    }
    const out = this.buffer.subarray(0, count);
    this.buffer = this.buffer.subarray(count);
    return out;
  }

  #wake() {
    const waiting = this.waiting;
    this.waiting = [];
    for (const resolve of waiting) resolve();
  }
}

function once(emitter, event) {
  return new Promise((resolve, reject) => {
    const ok = (...args) => {
      emitter.off("error", failed);
      resolve(args);
    };
    const failed = (error) => {
      emitter.off(event, ok);
      reject(error);
    };
    emitter.once(event, ok);
    emitter.once("error", failed);
  });
}

function withTimeout(promise, ms, why) {
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      const timer = setTimeout(() => reject(new Error(why)), ms);
      if (typeof timer?.unref === "function") timer.unref();
    }),
  ]);
}
