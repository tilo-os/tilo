// The mail service a door's mails go to when a run of it is local: it takes
// what Resend's API takes (`POST /emails`, JSON) and keeps each mail as a
// file in OUT_DIR, for the test to read the links out of. Nothing is sent.
//
// usage: node mail-sink.mjs OUT_DIR PORT
// Prints {"url": "http://127.0.0.1:<port>"} once it listens.
import {mkdirSync, readdirSync, writeFileSync} from 'node:fs';
import {createServer} from 'node:http';
import {join} from 'node:path';

const [out, port] = process.argv.slice(2);
if (!out || !port) {
  console.error('usage: node mail-sink.mjs OUT_DIR PORT');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
// Numbered after what an earlier run kept there, so a restart overwrites none.
let taken = readdirSync(out).filter(name => /^\d+\.json$/.test(name)).length;

const server = createServer((request, response) => {
  if (request.method !== 'POST' || request.url !== '/emails') {
    response.writeHead(404, {'content-type': 'application/json'});
    response.end(JSON.stringify({error: 'not the mail service'}));
    return;
  }
  const chunks = [];
  request.on('data', chunk => chunks.push(chunk));
  request.on('end', () => {
    let mail;
    try {
      mail = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    } catch (error) {
      response.writeHead(400, {'content-type': 'application/json'});
      response.end(JSON.stringify({error: `not JSON: ${error.message}`}));
      return;
    }
    taken += 1;
    writeFileSync(join(out, `${String(taken).padStart(3, '0')}.json`), JSON.stringify({
      idempotencyKey: request.headers['idempotency-key'] ?? null,
      ...mail,
    }, null, 2));
    response.writeHead(200, {'content-type': 'application/json'});
    response.end(JSON.stringify({id: `sink-${taken}`}));
  });
});
server.listen(Number(port), '127.0.0.1', () => {
  console.log(JSON.stringify({url: `http://127.0.0.1:${port}`}));
});
