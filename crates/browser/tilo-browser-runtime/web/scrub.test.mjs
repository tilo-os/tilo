import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import {takeLinkSecrets} from './link-secrets.mjs';

const web = new URL('./', import.meta.url);
const scrubber = readFileSync(new URL('scrub.inline.js', web), 'utf8');
const corpus = JSON.parse(readFileSync(new URL('../../../collab/reference/tests/links.json', web), 'utf8'));

/** Run the scrubber against `href` with a fake location and history. */
function scrub(href) {
  const url = new URL(href);
  const writes = [];
  const location = {
    get hash() { return url.hash; }, get pathname() { return url.pathname; }, get search() { return url.search; },
  };
  const history = {state: {page: 1}, replaceState(state, _title, next) {
    writes.push(next);
    const moved = new URL(next, url);
    url.hash = moved.hash;
    assert.equal(state, history.state, 'the page generation state is kept');
  }};
  const context = vm.createContext({location, history});
  context.globalThis = context;
  vm.runInContext(scrubber, context);
  return {url: url.href, writes, secrets: context.__tiloLinkSecrets};
}

test('secrets leave the URL before any module runs', () => {
  const rows = corpus.links.filter(row => row.secrets);
  assert.ok(rows.length >= 2, 'the corpus carries secret rows');
  for (const row of rows) {
    const {url, writes, secrets} = scrub(row.url);
    assert.equal(url, row.public, row.url);
    assert.deepEqual({...secrets}, row.secrets, row.url);
    for (const write of writes) {
      for (const value of Object.values(row.secrets)) assert.ok(!write.includes(value), `${write} holds a secret`);
    }
  }
});

test('a link without secrets is left alone', () => {
  for (const row of corpus.links.filter(row => row.target && !row.secrets)) {
    const {url, writes, secrets} = scrub(row.url);
    assert.equal(url, new URL(row.url).href, row.url);
    assert.deepEqual(writes, [], row.url);
    assert.deepEqual({...secrets}, {});
  }
});

test('secrets out of grammar order are removed and flagged', () => {
  const out = scrub('https://tilo.so/#/me/so.tilo.chat/-?invite=a&view=b');
  assert.equal(out.url, 'https://tilo.so/#/me/so.tilo.chat/-?view=b');
  assert.equal(out.secrets.malformed, true);
  const twice = scrub('https://tilo.so/#/me/so.tilo.chat?token=a&invite=b');
  assert.equal(twice.url, 'https://tilo.so/#/me/so.tilo.chat');
  assert.equal(twice.secrets.malformed, true);
});

test('the secrets are read once', () => {
  const holder = {__tiloLinkSecrets: Object.freeze({invite: 'x'})};
  assert.deepEqual(takeLinkSecrets(holder), {invite: 'x'});
  assert.deepEqual(takeLinkSecrets(holder), {});
});

test('every entry page inlines the scrubber verbatim as its first script', () => {
  for (const page of ['index.html', 'app/index.html']) {
    const html = readFileSync(new URL(page, web), 'utf8');
    const first = html.indexOf('<script');
    assert.ok(first > 0, page);
    assert.ok(html.slice(first).startsWith(`<script>\n${scrubber}</script>`), `${page} must inline scrub.inline.js unchanged`);
    assert.match(html, /<meta name="referrer" content="no-referrer">/, page);
  }
});
