// The ingress a deployment puts in front of the live plane: pages dial the
// address and port the plane's origin names, and the ingress forwards each
// client to the live listener, as the deployment's ingress publishes the
// plane on UDP 443 and forwards to the pod. One upstream socket per client,
// so the listener answers where the client dialled and two clients never see
// each other's datagrams — a page joins the plane only through here, never
// the listener itself.
//
// usage: node ingress.mjs PUBLIC_PORT LISTENER_HOST LISTENER_PORT
// Prints `{"udp": "127.0.0.1:PUBLIC_PORT"}` once it is bound.
import {createSocket} from 'node:dgram';

const [publicPort, listenerHost, listenerPort] = process.argv.slice(2);
if (!publicPort || !listenerHost || !listenerPort) {
  console.error('usage: node ingress.mjs PUBLIC_PORT LISTENER_HOST LISTENER_PORT');
  process.exit(2);
}
const port = Number(listenerPort);
// A client that has sent nothing for this long is gone: a QUIC connection
// keeps its socket alive with its own keep-alive.
const IDLE_MS = 120_000;

const inbound = createSocket('udp4');
const upstreams = new Map(); // "client address:port" -> {socket, bound, sent, queued}

const upstreamFor = client => {
  const key = `${client.address}:${client.port}`;
  let upstream = upstreams.get(key);
  if (upstream) return upstream;
  const socket = createSocket('udp4');
  upstream = {socket, bound: false, sent: 0, queued: []};
  socket.on('message', reply => inbound.send(reply, client.port, client.address));
  socket.on('error', () => {});
  socket.on('listening', () => {
    upstream.bound = true;
    for (const message of upstream.queued.splice(0)) socket.send(message, port, listenerHost);
  });
  socket.bind(0, '127.0.0.1');
  upstreams.set(key, upstream);
  return upstream;
};

inbound.on('message', (message, client) => {
  const upstream = upstreamFor(client);
  upstream.sent = Date.now();
  if (upstream.bound) upstream.socket.send(message, port, listenerHost);
  else upstream.queued.push(message);
});
inbound.on('error', error => {
  console.error(`ingress: ${error.message}`);
  process.exit(1);
});
inbound.bind(Number(publicPort), '127.0.0.1', () => {
  console.log(JSON.stringify({udp: `127.0.0.1:${publicPort}`}));
});
setInterval(() => {
  const now = Date.now();
  for (const [key, upstream] of upstreams) {
    if (now - upstream.sent < IDLE_MS) continue;
    upstream.socket.close();
    upstreams.delete(key);
  }
}, 30_000).unref();
