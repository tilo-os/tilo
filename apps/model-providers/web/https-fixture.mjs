// Ephemeral, self-signed certificate for the owned HTTPS fixture. Node's
// built-in crypto keeps this test independent of an openssl executable.
import {generateKeyPairSync, randomBytes, sign, X509Certificate} from 'node:crypto';
import assert from 'node:assert/strict';

function der(tag, ...parts) {
  const body = Buffer.concat(parts.map(p => Buffer.from(p)));
  let length = body.length;
  const bytes = [];
  do {bytes.unshift(length & 255); length >>>= 8;} while (length);
  return Buffer.concat([Buffer.from([tag, ...(body.length < 128 ? [body.length] : [128 | bytes.length, ...bytes])]), body]);
}
const sequence = (...parts) => der(0x30, ...parts);
const pem = (name, bytes) => `-----BEGIN ${name}-----\n${bytes.toString('base64').match(/.{1,64}/g).join('\n')}\n-----END ${name}-----\n`;

export function ephemeralTls() {
  const {privateKey, publicKey} = generateKeyPairSync('ec', {namedCurve:'prime256v1'});
  const algorithm = sequence(der(6, [0x2a,0x86,0x48,0xce,0x3d,4,3,2])); // ecdsa-with-SHA256
  const name = sequence(der(0x31, sequence(der(6, [0x55,4,3]), der(12, Buffer.from('localhost')))));
  const time = date => der(0x18, Buffer.from(date.toISOString().replace(/[-:T]/g, '').replace(/\.\d{3}Z$/, 'Z')));
  const serial = randomBytes(16); serial[0] = (serial[0] & 0x7f) || 1;
  const body = sequence(
    der(0xa0, der(2, [2])), der(2, serial), algorithm, name,
    sequence(time(new Date(Date.now()-60000)), time(new Date(Date.now()+86400000))), name,
    publicKey.export({type:'spki',format:'der'}),
  );
  const cert = pem('CERTIFICATE', sequence(body, algorithm, der(3, [0], sign('sha256',body,privateKey))));
  assert(new X509Certificate(cert).verify(publicKey), 'fixture certificate must verify');
  return {cert, key:privateKey.export({type:'pkcs8',format:'pem'})};
}
