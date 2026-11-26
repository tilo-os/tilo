// The mail service an entry's mails go to when a run of it is local: it takes
// what Resend's API takes (`POST /emails`, JSON) and keeps each mail as a
// file in OUT_DIR, for the test to read the links out of. Nothing is sent.
//
// usage: node mail-sink.mjs OUT_DIR PORT
// Prints {"url": "http://127.0.0.1:<port>"} once it listens.
import {randomUUID} from 'node:crypto';
import {mkdirSync, readdirSync, renameSync, rmSync, writeFileSync} from 'node:fs';
import {createServer} from 'node:http';
import {join} from 'node:path';

const [out, port] = process.argv.slice(2);
if (!out || !port) {
  console.error('usage: node mail-sink.mjs OUT_DIR PORT');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
// Numbered after what an earlier run kept there, so a restart overwrites none.
let taken = readdirSync(out).reduce((highest, name) =>
  /^\d+\.json$/.test(name) ? Math.max(highest, Number.parseInt(name, 10)) : highest, 0);

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
    // The browser reads every .json as soon as its name appears.
    const pending = join(out, `.mail-${randomUUID()}.pending`);
    const delivered = join(out, `${String(taken).padStart(3, '0')}.json`);
    try {
      writeFileSync(pending, JSON.stringify({
        idempotencyKey: request.headers['idempotency-key'] ?? null,
        ...mail,
      }, null, 2), {flag: 'wx', mode: 0o600});
      renameSync(pending, delivered);
    } finally {
      rmSync(pending, {force: true});
    }
    response.writeHead(200, {'content-type': 'application/json'});
    response.end(JSON.stringify({id: `sink-${taken}`}));
  });
});
server.listen(Number(port), '127.0.0.1', () => {
  console.log(JSON.stringify({url: `http://127.0.0.1:${server.address().port}`}));
});
