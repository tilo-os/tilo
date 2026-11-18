// Serves a built bundle on this machine's loopback for a person's own browser,
// the way the page checks serve it (`serve` in ../chrome.mjs): compressed as a
// deployment serves it, and never cached, so a rebuild shows on the next load.
//
// usage: node tools/web/html-renderer/serve.mjs DIR [PORT]   (PORT 0: any free one)
import {serve} from '../chrome.mjs';

const [root, port = '0'] = process.argv.slice(2);
if (!root) {
  console.error('usage: serve.mjs DIR [PORT]');
  process.exit(2);
}
const server = await serve(root, {port: Number(port)});
console.log(server.url);
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => server.close().then(() => process.exit(0)));
