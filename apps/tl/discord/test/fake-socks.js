// A fake egress proxy for the tests: the TCP side answers a SOCKS5 greeting and a UDP
// ASSOCIATE, and the relay it names carries each datagram to the endpoint the client wrapped it
// for, and what that endpoint answers back the same way.

import { createSocket } from "node:dgram";
import { createServer } from "node:net";
import { unwrap, wrap } from "../src/socks.js";

function ipv4(address) {
  return Buffer.from(String(address).split(".").map(Number));
}

function portBytes(port) {
  const out = Buffer.alloc(2);
  out.writeUInt16BE(port, 0);
  return out;
}

/// The proxy's relay and control sockets, its requests, and what it carried.
export async function fakeProxy() {
  const relay = createSocket("udp4");
  // A datagram to a client that already left is refused, as on any relay: nothing to carry.
  relay.on("error", () => {});
  await new Promise((resolve) => relay.bind(0, "127.0.0.1", resolve));
  const requests = [];
  const carried = [];
  let client = null;

  relay.on("message", (packet, from) => {
    // The client's datagrams carry a SOCKS5 header; an endpoint's answers are the media itself,
    // which the relay wraps with where it came from. The first datagram comes from the client.
    const mine = !client || (from.address === client.address && from.port === client.port);
    if (mine) {
      const opened = unwrap(packet);
      if (!opened) return;
      client = { address: from.address, port: from.port };
      carried.push(opened);
      relay.send(opened.data, opened.target.port, opened.target.address);
      return;
    }
    const at = { address: from.address, port: from.port };
    relay.send(wrap(at, packet), client.port, client.address);
  });

  const server = createServer((socket) => {
    let buffer = Buffer.alloc(0);
    let stage = 0;
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      for (;;) {
        if (stage === 0) {
          if (buffer.length < 3) return;
          const greeting = buffer.subarray(0, 3);
          buffer = buffer.subarray(3);
          requests.push({ greeting: [...greeting] });
          socket.write(Buffer.from([0x05, 0x00]));
          stage = 1;
          continue;
        }
        if (stage === 1) {
          if (buffer.length < 10) return;
          const head = buffer.subarray(0, 10);
          buffer = buffer.subarray(10);
          requests.push({ command: head[1] });
          const at = relay.address();
          socket.write(
            Buffer.concat([
              Buffer.from([0x05, 0x00, 0x00, 0x01]),
              ipv4(at.address),
              portBytes(at.port),
            ]),
          );
          stage = 2;
          continue;
        }
        return;
      }
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  return {
    /// The address the client connects its control socket to.
    at: { host: "127.0.0.1", port: server.address().port },
    /// What the proxy was asked, in order.
    requests,
    /// Every datagram it carried, with the endpoint it was for.
    carried,
    /// Where the client sent from, once it has sent anything.
    clientAddress: () => client,
    stop() {
      relay.close();
      server.close();
    },
  };
}
