// One Discord voice connection: the voice gateway (v8) and the UDP socket that carries the
// call's audio both ways.
//
// Identify, Hello and Ready, IP discovery on UDP, Select Protocol with the best transport mode,
// Session Description with the key; then heartbeats, who speaks on which SSRC, and the audio.
// What a Discord user says reaches the workspace as their own Opus frames, one speaker at a
// time; what the workspace says reaches Discord as one Opus stream. The two never meet, so
// nothing the Tilo call heard is sent back to Discord.
//
// Discord now closes a call that speaks no end-to-end encryption protocol with code 4017 unless
// the channel allows transport-only media. This program offers `max_dave_protocol_version: 0`,
// and a call Discord refuses for want of DAVE ends with that reason in the Discord record.

import { createSocket } from "node:dgram";
import { Cipher, Sender, chooseMode, open } from "./rtp.js";

/// Opus silence, sent before a pause so receivers do not interpolate across it.
export const SILENCE = Buffer.from([0xf8, 0xff, 0xfe]);
const SILENCE_FRAMES = 5;
/// Speaking flag: the microphone.
const MICROPHONE = 1;
/// One Opus frame: 20 ms.
const FRAME_MS = 20;
/// How long the gateway has to answer before the join is given up.
const CONNECT_MS = 15_000;
const DISCOVERY_MS = 5_000;

/// The voice gateway URL for `endpoint`. Discord names the endpoint with a port its voice
/// servers also answer on 443, so the port is dropped; a loopback endpoint, which only a test
/// names, is plain `ws://` and keeps its port.
export function voiceUrl(endpoint, { allowLoopback = false } = {}) {
  const raw = String(endpoint ?? "").trim();
  const host = raw.split("/")[0];
  if (!host) throw new Error("Discord named no voice server");
  const loopback = /^(127\.0\.0\.1|\[::1\]|localhost)(:\d+)?$/.test(host);
  if (loopback) {
    if (!allowLoopback) throw new Error(`the voice server ${host} is not one Discord names`);
    return `ws://${host}/?v=8`;
  }
  if (!/(\.|^)discord\.media(:\d+)?$|(\.|^)discord\.gg(:\d+)?$/.test(host)) {
    throw new Error(`the voice server ${host} is not one Discord names`);
  }
  const bare = host.replace(/:\d+$/, "");
  return `wss://${bare}/?v=8`;
}

/// What a voice gateway close code means: `identify` starts afresh, `resume` reopens and
/// resumes, and a reason ends the call for good.
export function closeMeaning(code, reason) {
  if (code === 4006 || code === 4009) return { again: "identify" };
  if (code === 4014) return { end: "Discord disconnected the bot from the call" };
  if (code === 4021) return { end: "Discord is rate-limiting the bot's voice connection" };
  if (code === 4022) return { end: "Discord ended the call" };
  if (code === 4017) {
    return { end: "Discord requires end-to-end encryption (DAVE) for this call" };
  }
  if (code >= 4001 && code <= 4005) return { end: closedWords(code, reason) };
  if ([4011, 4012, 4016, 4017, 4020].includes(code)) return { end: closedWords(code, reason) };
  // 4015 (the voice server crashed) and anything unlisted: reopen.
  return { again: "resume" };
}

function closedWords(code, reason) {
  return `Discord's voice server closed the connection: ${reason || "no reason given"} (${code})`;
}

/// Join the voice channel Discord handed out for this bridge.
///
/// `onAudio({speaker, opus})` is called for each Opus frame an outside person says, `speaker`
/// their Discord account id. The answer is the connection: `say(opus)` queues one frame of the
/// workspace's voice, `muted` stops both directions, and `close()` ends the call.
export function connect({
  endpoint,
  guild,
  channel,
  user,
  session,
  token,
  onAudio = () => {},
  onEnd = () => {},
  log = () => {},
  allowLoopback = false,
  make = {},
}) {
  const socketFactory = make.dgram ?? createSocket;
  const webSocket = make.WebSocket ?? WebSocket;
  const url = voiceUrl(endpoint, { allowLoopback });
  const voice = new Voice({
    url,
    guild,
    channel,
    user,
    session,
    token: String(token ?? ""),
    onAudio,
    onEnd,
    log,
    webSocket,
    socketFactory,
  });
  voice.start();
  return voice;
}

export class Voice {
  constructor({ url, guild, channel, user, session, token, onAudio, onEnd, log, webSocket, socketFactory }) {
    this.url = url;
    this.guild = String(guild);
    this.channel = String(channel);
    this.user = String(user);
    this.session = String(session);
    this.token = token;
    this.onAudio = onAudio;
    this.onEnd = onEnd;
    this.log = log;
    this.webSocket = webSocket;
    this.socketFactory = socketFactory;
    this.udp = null;
    this.cipher = null;
    this.sender = null;
    this.mode = null;
    this.ssrc = 0;
    this.users = new Map();
    this.sequenceAck = -1;
    this.socket = null;
    this.beat = null;
    this.tick = null;
    this.deadline = null;
    this.speaking = false;
    this.silenceLeft = 0;
    this.muted = false;
    this.closed = false;
    this.joined = false;
    this.acked = true;
    /// Opus frames the workspace said, waiting for their 20 ms.
    this.queue = [];
    this.attempt = 0;
  }

  start() {
    this.#open(false);
  }

  /// Queue one Opus frame the workspace's call said. A frame that arrives while the queue is
  /// already a second deep is dropped, not queued: the outside call is live audio.
  say(opus) {
    if (this.closed || this.muted) return;
    if (this.queue.length > 50) return;
    this.queue.push(Buffer.from(opus));
  }

  /// Stop sending and stop playing: the person or the bridge silenced this side.
  setMuted(muted) {
    this.muted = Boolean(muted);
    if (!this.muted) return;
    this.queue.length = 0;
    if (this.speaking) {
      this.speaking = false;
      this.silenceLeft = 0;
      this.#send({ op: 5, d: { speaking: 0, delay: 0, ssrc: this.ssrc } });
    }
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.#stopTimers();
    try {
      this.socket?.close(1000);
    } catch {}
    this.socket = null;
    try {
      this.udp?.close();
    } catch {}
    this.udp = null;
    this.queue.length = 0;
  }

  #stopTimers() {
    if (this.beat) clearInterval(this.beat);
    if (this.deadline) clearTimeout(this.deadline);
    if (this.tick) clearInterval(this.tick);
    this.beat = null;
    this.deadline = null;
    this.tick = null;
  }

  #send(payload) {
    try {
      this.socket?.send(JSON.stringify(payload));
    } catch {}
  }

  #open(resume) {
    if (this.closed) return;
    let socket;
    try {
      socket = new this.webSocket(this.url);
    } catch (error) {
      this.#ended(`the voice gateway is unreachable: ${error?.message ?? error}`);
      return;
    }
    this.socket = socket;
    socket.binaryType = "arraybuffer";
    this.deadline = setTimeout(() => {
      if (!this.joined) this.#reopen();
    }, CONNECT_MS);
    socket.onopen = () => {
      const opening = resume
        ? {
            op: 7,
            d: {
              server_id: this.guild,
              session_id: this.session,
              token: this.token,
              seq_ack: this.sequenceAck,
            },
          }
        : {
            op: 0,
            d: {
              server_id: this.guild,
              user_id: this.user,
              session_id: this.session,
              token: this.token,
              max_dave_protocol_version: 0,
            },
          };
      this.#send(opening);
    };
    socket.onmessage = (event) => this.#heard(event.data);
    socket.onerror = () => {};
    socket.onclose = (event) => {
      if (this.socket !== socket) return;
      this.socket = null;
      this.#stopTimers();
      this.#closed(event.code ?? 0, event.reason ?? "");
    };
  }

  #reopen(resume = true) {
    if (this.closed) return;
    this.attempt += 1;
    setTimeout(() => this.#open(resume), Math.min(5_000, 250 * 2 ** this.attempt));
  }

  #closed(code, reason) {
    if (this.closed) return;
    const meaning = closeMeaning(code, reason);
    if (meaning.end) {
      this.#ended(meaning.end);
      return;
    }
    const resume = meaning.again === "resume";
    if (!resume) {
      // The session is gone: nothing of it is resumed, and the call joins again whole.
      this.joined = false;
      this.users.clear();
      this.sequenceAck = -1;
      try {
        this.udp?.close();
      } catch {}
      this.udp = null;
      this.cipher = null;
      this.sender = null;
      this.queue.length = 0;
    }
    this.#reopen(resume);
  }

  #ended(reason) {
    if (this.closed) return;
    this.log(reason);
    this.close();
    this.onEnd(reason);
  }

  #heard(raw) {
    if (typeof raw !== "string") {
      // A DAVE opcode frame: protocol version 0 speaks none of them.
      return;
    }
    let message;
    try {
      message = JSON.parse(raw);
    } catch {
      return;
    }
    const seq = message?.s;
    if (typeof seq === "number") this.sequenceAck = Math.max(this.sequenceAck, seq);
    const data = message?.d ?? {};
    switch (message?.op) {
      case 8: {
        const interval = Math.min(600_000, Math.max(100, Math.round(data.heartbeat_interval ?? 13_750)));
        this.#startBeat(interval);
        return;
      }
      case 6:
        this.acked = true;
        return;
      case 2:
        this.#ready(data).catch((error) => this.#ended(String(error?.message ?? error)));
        return;
      case 4:
        this.#description(data).catch((error) => this.#ended(String(error?.message ?? error)));
        return;
      case 5: {
        const ssrc = Number(data.ssrc);
        const who = String(data.user_id ?? "");
        if (Number.isFinite(ssrc) && who) this.users.set(ssrc >>> 0, who);
        return;
      }
      case 9:
        // Session described: live from here.
        this.#live();
        return;
      default:
        return;
    }
  }

  #startBeat(interval) {
    if (this.beat) clearInterval(this.beat);
    this.acked = true;
    const beat = () => {
      if (this.acked === false) {
        this.socket?.close(4000);
        return;
      }
      this.acked = false;
      this.#send({ op: 3, d: { t: Date.now(), seq_ack: this.sequenceAck } });
    };
    this.beat = setInterval(beat, interval);
  }

  /// The voice server's Ready: its UDP address, our SSRC and the modes. Find our public address
  /// on UDP and answer with Select Protocol.
  async #ready(data) {
    const ssrc = Number(data.ssrc);
    const ip = String(data.ip ?? "");
    const port = Number(data.port);
    if (!Number.isFinite(ssrc) || !ip || !Number.isFinite(port)) {
      throw new Error("the voice server's Ready is incomplete");
    }
    const { mode, reason } = chooseMode(data.modes);
    if (!mode) throw new Error(reason);
    // A resumed call negotiates its address and key again: the connection before it is over.
    try {
      this.udp?.close();
    } catch {}
    this.cipher = null;
    this.sender = null;
    const udp = this.socketFactory(ip.includes(":") ? "udp6" : "udp4");
    this.udp = udp;
    await new Promise((resolve, reject) => {
      const failed = (error) => reject(error);
      udp.once("error", failed);
      udp.connect(port, ip, () => {
        udp.off("error", failed);
        resolve();
      });
    });
    udp.on("message", (packet) => this.#packet(packet));
    udp.on("error", (error) => this.log(`the voice socket failed: ${error?.message ?? error}`));
    const found = await discovery(udp, ssrc >>> 0);
    if (!found) throw new Error("the voice server did not answer IP discovery");
    this.ssrc = ssrc >>> 0;
    this.mode = mode;
    this.#send({
      op: 1,
      d: { protocol: "udp", data: { address: found.address, port: found.port, mode } },
    });
  }

  /// Session Description: the key the call's media is sealed with.
  async #description(data) {
    const mode = String(data.mode ?? "");
    if (mode !== this.mode) {
      throw new Error(`the voice server chose another encryption mode (${mode})`);
    }
    const key = Buffer.from(Array.isArray(data.secret_key) ? data.secret_key : []);
    this.cipher = new Cipher(mode, key);
    this.sender = new Sender(this.ssrc);
    this.#live();
  }

  #live() {
    if (this.tick || !this.cipher) return;
    this.joined = true;
    if (this.deadline) clearTimeout(this.deadline);
    this.deadline = null;
    this.tick = setInterval(() => this.#tick(), FRAME_MS);
  }

  /// One packet on the UDP socket: a Discord user's audio for the workspace.
  #packet(packet) {
    if (!this.cipher || this.muted) return;
    const received = open(this.cipher, packet);
    if (!received) return;
    const who = this.users.get(received.ssrc);
    if (!who) return;
    if (received.frame.length === SILENCE.length && received.frame.equals(SILENCE)) return;
    this.onAudio({ speaker: who, opus: received.frame });
  }

  /// Send the next 20 ms of the workspace's call, or the silence that ends it, with the Speaking
  /// op ahead of the first packet and after the last.
  #tick() {
    if (this.muted) return;
    const frame = this.queue.shift() ?? null;
    if (frame) {
      this.silenceLeft = SILENCE_FRAMES;
      if (!this.speaking) {
        this.speaking = true;
        this.#send({ op: 5, d: { speaking: MICROPHONE, delay: 0, ssrc: this.ssrc } });
      }
      return this.#packetOut(frame);
    }
    if (!this.speaking) return;
    if (this.silenceLeft > 0) {
      this.silenceLeft -= 1;
      return this.#packetOut(SILENCE);
    }
    this.speaking = false;
    this.#send({ op: 5, d: { speaking: 0, delay: 0, ssrc: this.ssrc } });
  }

  #packetOut(frame) {
    if (!this.cipher || !this.sender || !this.udp) return;
    const packet = this.sender.packet(this.cipher, frame);
    if (!packet) return;
    this.udp.send(packet, (error) => {
      if (error) this.log(`a voice packet did not reach the server: ${error.message}`);
    });
  }
}

/// Discord's IP discovery: our address and port as the voice server sees them.
export function discovery(udp, ssrc) {
  const request = Buffer.alloc(74);
  request.writeUInt16BE(1, 0);
  request.writeUInt16BE(70, 2);
  request.writeUInt32BE(ssrc >>> 0, 4);
  return new Promise((resolve) => {
    let settled = false;
    const finish = (answer) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      udp.off("message", heard);
      resolve(answer);
    };
    const heard = (answer) => {
      if (answer.length < 74 || answer.readUInt16BE(0) !== 2) return;
      const text = answer.subarray(8, 72);
      const end = text.indexOf(0);
      const address = text.subarray(0, end < 0 ? text.length : end).toString("utf8");
      finish({ address, port: answer.readUInt16BE(72) });
    };
    const timer = setTimeout(() => finish(null), DISCOVERY_MS);
    udp.on("message", heard);
    udp.send(request, (error) => {
      if (error) finish(null);
    });
  });
}
