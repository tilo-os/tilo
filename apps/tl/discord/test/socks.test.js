// The call's UDP through the sandbox's egress proxy: the environment names it, the association
// is what the proxy hands out, and each datagram carries the voice server it is for.

import { test, expect } from "bun:test";
import { createSocket } from "node:dgram";
import { callProxy, proxyUdp, unwrap, wrap } from "../src/socks.js";
import { fakeProxy } from "./fake-socks.js";

/// Wait until `check()` is true, or fail after `timeout`.
async function until(check, what, timeout = 4_000) {
  const end = Date.now() + timeout;
  while (!check()) {
    if (Date.now() > end) throw new Error(`waited for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

test("the proxy a sandbox names is read from all_proxy, socks5h or plain", () => {
  expect(callProxy({ ALL_PROXY: "socks5h://127.0.0.1:9050" })).toEqual({
    host: "127.0.0.1",
    port: 9050,
  });
  expect(callProxy({ all_proxy: "socks5://proxy.internal:1080/" })).toEqual({
    host: "proxy.internal",
    port: 1080,
  });
  expect(callProxy({ ALL_PROXY: "http://127.0.0.1:8118" })).toBeNull();
  expect(callProxy({})).toBeNull();
});

test("a datagram carries the endpoint it is for, and reads it back", () => {
  const packet = Buffer.from([1, 2, 3]);
  const wrapped = wrap({ address: "192.0.2.7", port: 50000 }, packet);
  expect([...wrapped.subarray(0, 4)]).toEqual([0, 0, 0, 1]);
  const opened = unwrap(wrapped);
  expect(opened.target).toEqual({ address: "192.0.2.7", port: 50000 });
  expect([...opened.data]).toEqual([1, 2, 3]);
  // A reply names the voice server the same way, and an IPv6 one too.
  const v6 = unwrap(wrap({ address: "2001:db8::7", port: 50001 }, packet));
  expect(v6.target).toEqual({ address: "2001:db8::7", port: 50001 });
  expect([...v6.data]).toEqual([1, 2, 3]);
  // A datagram the proxy did not relay is dropped.
  expect(unwrap(Buffer.from([0, 0, 1, 1]))).toBeNull();
});

test("the call's UDP is carried through the proxy's association", async () => {
  const proxy = await fakeProxy();
  // The voice server: a socket answering `pong` to whatever it was sent.
  const voice = createSocket("udp4");
  await new Promise((resolve) => voice.bind(0, "127.0.0.1", resolve));
  voice.on("message", (packet, from) => {
    voice.send(Buffer.from("pong"), from.port, from.address);
  });
  const udp = proxyUdp(proxy.at);
  const heard = [];
  try {
    await new Promise((resolve, reject) => {
      udp.on("error", reject);
      udp.on("message", (packet) => heard.push(packet));
      udp.connect(voice.address().port, "127.0.0.1", resolve);
    });
    expect(proxy.requests.map((request) => request.command ?? request.greeting[0])).toEqual([
      5, 3,
    ]);
    udp.send(Buffer.from("ping"));
    await until(() => heard.length >= 1, "the relayed answer");
    expect(heard[0].toString()).toBe("pong");
    expect(proxy.carried[0].target).toEqual({
      address: "127.0.0.1",
      port: voice.address().port,
    });
  } finally {
    udp.close();
    voice.close();
    proxy.stop();
  }
});
