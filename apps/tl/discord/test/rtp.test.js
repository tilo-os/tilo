import { test, expect } from "bun:test";
import {
  AES_GCM,
  Cipher,
  OPUS_PAYLOAD_TYPE,
  Sender,
  XCHACHA,
  chooseMode,
  open,
} from "../src/rtp.js";

const KEY = new Uint8Array(32).fill(7);

test("packets round trip and bind their counter and header", () => {
  const cipher = new Cipher(AES_GCM, KEY);
  const sender = new Sender(42, [0xff, 0xff, 0, 0, 0, 1, 0, 0, 0, 9]);
  const first = sender.packet(cipher, Buffer.from("opus one"));
  const second = sender.packet(cipher, Buffer.from("opus two"));
  expect(first[0]).toBe(0x80);
  expect(first[1]).toBe(OPUS_PAYLOAD_TYPE);
  expect([...first.subarray(first.length - 4)]).toEqual([0, 0, 0, 9]);
  const opened = open(cipher, first);
  expect(opened.ssrc).toBe(42);
  expect(opened.sequence).toBe(0xffff);
  expect(opened.frame.toString()).toBe("opus one");
  expect(open(cipher, second).sequence).toBe(0);
  if (first.length < 5) throw new Error("packet too short");
  const tampered = Buffer.from(first);
  tampered[3] ^= 1;
  expect(open(cipher, tampered)).toBeNull();
});

test("another key or another counter opens nothing", () => {
  const cipher = new Cipher(AES_GCM, KEY);
  const sender = new Sender(1, [0, 1, 0, 0, 0, 2, 0, 0, 0, 5]);
  const packet = sender.packet(cipher, Buffer.from("hello"));
  expect(open(new Cipher(AES_GCM, new Uint8Array(32).fill(8)), packet)).toBeNull();
  const moved = Buffer.from(packet);
  moved.writeUInt32BE(6, moved.length - 4);
  expect(open(cipher, moved)).toBeNull();
});

test("a sender's skipped frame advances the stream without a packet", () => {
  const cipher = new Cipher(AES_GCM, KEY);
  const sender = new Sender(7, [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
  const before = sender.packet(cipher, Buffer.from("a"));
  sender.skip();
  const after = sender.packet(cipher, Buffer.from("b"));
  expect(open(cipher, before).sequence).toBe(0);
  expect(open(cipher, after).sequence).toBe(2);
  expect(open(cipher, after).frame.toString()).toBe("b");
});

test("AES is preferred, and a server with only XChaCha says why it cannot be used", () => {
  expect(chooseMode([XCHACHA, AES_GCM])).toEqual({ mode: AES_GCM });
  const only = chooseMode(["xsalsa20_poly1305", XCHACHA]);
  expect(only.mode).toBeUndefined();
  expect(only.reason).toContain("aead_xchacha20_poly1305_rtpsize");
  expect(chooseMode(["xsalsa20_poly1305"]).reason).toContain("no encryption mode");
});

test("a key of the wrong length is refused", () => {
  expect(() => new Cipher(AES_GCM, new Uint8Array(16))).toThrow("32 bytes");
  expect(() => new Cipher(XCHACHA, KEY)).toThrow("not an encryption mode this build carries");
});
