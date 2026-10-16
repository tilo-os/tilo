/**
 * JSON passed through the page as text, never as JS values.
 *
 * A device certificate carries `issued_at_ns` and `expires_at_ns`, u64
 * nanoseconds since the epoch, and its signature covers their exact bytes. A
 * JS number holds integers exactly only up to 2^53, so `JSON.parse` followed
 * by `JSON.stringify` changes any timestamp with more than about 16
 * significant digits: every certificate a host on a clock with nanosecond
 * resolution (Linux) issued. The host then refuses the page's own
 * certificate ("device-cert signature did not verify"). Certificates move
 * from the runtime's outputs to its inputs through these helpers instead.
 */

const SPACE = ' \t\n\r';

function skip(json, at) {
  let i = at;
  while (i < json.length && SPACE.includes(json[i])) i++;
  return i;
}

function stringEnd(json, at) {
  if (json[at] !== '"') throw new SyntaxError(`expected a string at ${at}`);
  for (let i = at + 1; i < json.length; i++) {
    if (json[i] === '\\') i++;
    else if (json[i] === '"') return i + 1;
  }
  throw new SyntaxError('unterminated string');
}

function valueEnd(json, at) {
  const first = json[at];
  if (first === '"') return stringEnd(json, at);
  if (first === '{' || first === '[') {
    let depth = 0;
    for (let i = at; i < json.length; i++) {
      const c = json[i];
      if (c === '"') i = stringEnd(json, i) - 1;
      else if (c === '{' || c === '[') depth++;
      else if (c === '}' || c === ']') {
        depth--;
        if (depth === 0) return i + 1;
      }
    }
    throw new SyntaxError('unterminated value');
  }
  let i = at;
  while (i < json.length && !',}]'.includes(json[i]) && !SPACE.includes(json[i])) i++;
  if (i === at) throw new SyntaxError(`expected a value at ${at}`);
  return i;
}

/**
 * The exact JSON text of member `name` of the object in `json`, or
 * `undefined` when it has none. Throws a `SyntaxError` for anything that is
 * not an object's JSON text.
 */
export function memberText(json, name) {
  if (typeof json !== 'string') throw new TypeError('memberText reads JSON text');
  // Well-formed first: the scan below then only has to find the member.
  JSON.parse(json);
  let i = skip(json, 0);
  if (json[i] !== '{') throw new SyntaxError('not a JSON object');
  i = skip(json, i + 1);
  if (json[i] === '}') return undefined;
  for (;;) {
    const keyEnd = stringEnd(json, i);
    const key = JSON.parse(json.slice(i, keyEnd));
    i = skip(json, keyEnd);
    if (json[i] !== ':') throw new SyntaxError(`expected ':' at ${i}`);
    const start = skip(json, i + 1);
    const end = valueEnd(json, start);
    if (key === name) return json.slice(start, end);
    i = skip(json, end);
    if (json[i] === '}') return undefined;
    if (json[i] !== ',') throw new SyntaxError(`expected ',' at ${i}`);
    i = skip(json, i + 1);
  }
}

/**
 * An object's JSON text from `members`, each the JSON text of its value
 * (`memberText`, or `JSON.stringify` of a plain value). Members whose text is
 * `undefined` are left out.
 */
export function objectText(members) {
  const fields = Object.entries(members).filter(([, text]) => text !== undefined)
    .map(([name, text]) => `${JSON.stringify(name)}:${text}`);
  return `{${fields.join(',')}}`;
}
