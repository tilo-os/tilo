/**
 * The sign-in route: the page at `<web entry>/sign-in`, which a service that
 * is not one of the person's devices sends their browser to.
 *
 * The service writes the whole request in the link:
 *
 * ```text
 * <web entry>/sign-in?audience=<the service's origin>&challenge=<64 hex>
 *   &return=<audience>/sign-in&pending=<its id>&client=<what it calls itself>
 *   &grant_key=<64 hex>&grant_scope=<all|a workspace id>&grant_expires=<unix ms>
 *   [&grant_kind=server_key] [&grant_pages=1]
 * ```
 *
 * `grant_kind=server_key` asks for an API key the person creates for their
 * own backend: it reaches all their workspaces or, where the link names one,
 * that one, and lasts until a time the link names — `never`, which only such
 * a key reaching all of them may name, or a date, where one reaching a
 * workspace ends within the 90 days an agent's grant at most lasts.
 *
 * Everything in it is checked before the page shows anything, and nothing is
 * signed until the person presses Allow: then this browser's device key,
 * certified by the person's own root for this answer, proves them to the
 * service (`tilo-session-client`'s `sign_in_proof`), and — where the service
 * asked for it — the root certifies the key the service generated for the
 * agent's grant, reaching what the card shows: a service that asked for all
 * of the person's workspaces lets them hold the agent to one of the ones this
 * browser knows (`entered-workspaces.mjs`), and one that asked for a
 * workspace gets that one. The answer goes back as a form the browser posts
 * to `return`, so the service resolves the sign-in it started (`pending`)
 * with a session cookie of its own. Cancel posts the same form with
 * `decision=deny`.
 *
 * The page holds no peer and reads no runtime until the person agrees: a
 * sign-in link is not a workspace, and the bundle's runtime code has no part
 * in answering one. What it can say of the card it reads without either: the
 * workspaces this browser entered by name, and the site's own home workspace
 * from `entry.json`. A grant that reaches a workspace neither names is said
 * in plain words, never by its id; so is where the service is when its
 * origin is this computer rather than a name people know; and its date reads in the
 * page's own language.
 */

import {openDeviceKey as openStoredDeviceKey, idbStorage} from './device-key.mjs';
import {enteredWorkspaces} from './entered-workspaces.mjs';
import {browserIdentity, currentIdentity, keptIdentity} from './identity.mjs';
import {identityProfile} from './identity-profile.mjs';

/** The longest a grant may last: the identity model's bound, 90 days. */
export const MAX_GRANT_LIFETIME_MS = 90 * 24 * 60 * 60 * 1000;

/** The longest client name a link may carry, in characters. */
const MAX_CLIENT_CHARS = 200;

const HEX32 = /^[0-9a-f]{64}$/i;
/** A canonical, lower-case UUID (a workspace id). */
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
/** Opaque, short, and safe in a form field as it arrives. */
const PENDING = /^[A-Za-z0-9_.~-]{1,200}$/;
const MILLISECONDS = /^[0-9]{1,16}$/;

/** The words a person reads when a link is not one this page may answer. */
export class SignInLinkError extends Error {
  constructor(message) {
    super(message);
    this.name = 'SignInLinkError';
  }
}

/**
 * `value` as an origin a sign-in may answer — `https://…`, or
 * `http://localhost`/`http://127.0.0.1` with a port for a development
 * service — or `null`. It must be an origin and nothing else: no path,
 * query, fragment, credentials or default-port spelling.
 */
export function signInOrigin(value) {
  let url;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  if (url.origin !== value) return null;
  if (url.protocol === 'https:') return url;
  if (url.protocol === 'http:' && (url.hostname === 'localhost' || url.hostname === '127.0.0.1')) return url;
  return null;
}

/** The service's own name for the client that asks: its text, never its
 * markup, without control characters and short enough to show. */
function clientName(values) {
  if (values.length !== 1) throw new SignInLinkError('This link is not a complete Tilo sign-in link.');
  const name = Array.from(values[0].replace(/[\u0000-\u001f\u007f]/g, '')).join('').trim();
  if (!name) throw new SignInLinkError('This link does not say which client asks to act for you.');
  return Array.from(name).slice(0, MAX_CLIENT_CHARS).join('');
}

/**
 * The request `search` (a location's query) carries, checked: what the
 * service asks, or a [`SignInLinkError`] in words fit for the person.
 *
 * `now` (Unix milliseconds) decides whether a grant the link asks for is
 * still ahead; it is a boundary for tests, not a value the page chooses.
 */
export function readSignInRequest(search, {now = Date.now()} = {}) {
  const params = new URLSearchParams(search);
  const once = name => {
    const values = params.getAll(name);
    if (values.length !== 1) throw new SignInLinkError('This link is not a complete Tilo sign-in link.');
    return values[0];
  };
  const audience = signInOrigin(once('audience'));
  if (!audience) throw new SignInLinkError('This link does not name the service it would sign you in to.');
  if (once('return') !== `${audience.origin}/sign-in`) {
    throw new SignInLinkError('This link would send your answer somewhere other than the service that asked.');
  }
  const challenge = once('challenge').toLowerCase();
  if (!HEX32.test(challenge)) throw new SignInLinkError('This link carries no usable challenge from the service.');
  const pending = once('pending');
  if (!PENDING.test(pending)) throw new SignInLinkError('This link carries no request from the service.');
  const client = clientName(params.getAll('client'));

  const granted = ['grant_key', 'grant_scope', 'grant_expires'].filter(name => params.has(name));
  if (granted.length && granted.length < 3) {
    throw new SignInLinkError('This link asks for a grant Tilo cannot give.');
  }
  let grantKey = null;
  let grantScope = null;
  let grantExpiresMs = null;
  const grantServerKey = params.has('grant_kind');
  // A key its person lets operate web pages in Browser carries it in the
  // certificate this page has certified: only where a grant was asked for.
  const grantPages = granted.length === 3 && params.get('grant_pages') === '1';
  if (grantServerKey && (granted.length < 3 || once('grant_kind') !== 'server_key')) {
    throw new SignInLinkError('This link asks for a grant Tilo cannot give.');
  }
  if (granted.length === 3) {
    grantKey = once('grant_key').toLowerCase();
    if (!HEX32.test(grantKey)) throw new SignInLinkError('This link asks for a grant Tilo cannot give.');
    grantScope = once('grant_scope');
    if (grantScope !== 'all' && !UUID.test(grantScope)) {
      throw new SignInLinkError('This link asks a grant to reach a workspace Tilo cannot read.');
    }
    const expires = once('grant_expires');
    // Only a key reaching all of the person's workspaces may last until they
    // revoke it: one reaching a workspace ends within the 90 days an agent's
    // grant at most lasts.
    const untilRevoked = grantServerKey && grantScope === 'all';
    if (!(untilRevoked && expires === 'never')) {
      if (!MILLISECONDS.test(expires)) throw new SignInLinkError('This link asks for a grant Tilo cannot give.');
      grantExpiresMs = Number(expires);
      if (grantExpiresMs <= now) {
        throw new SignInLinkError('This sign-in link has expired. Connect again from where you started, for a new one.');
      }
      if (!untilRevoked && grantExpiresMs - now > MAX_GRANT_LIFETIME_MS) {
        throw new SignInLinkError('This link asks for a grant longer than Tilo gives. Ask the service for a shorter one.');
      }
    }
  }
  return Object.freeze({
    audience: audience.origin,
    host: audience.host,
    challenge,
    returnTo: `${audience.origin}/sign-in`,
    pending,
    client,
    grantKey,
    grantScope,
    grantExpiresMs,
    grantServerKey,
    grantPages,
  });
}

/** The date style the card shows a grant's end in. */
const DATE = Object.freeze({year: 'numeric', month: 'long', day: 'numeric'});

/** The language the page is written in, from its own `<html lang>`: what the
 * card's date reads in, never the browser's language. */
export function pageLocale(document = globalThis.document) {
  const lang = document?.documentElement?.lang;
  return typeof lang === 'string' && /^[a-z]{2,3}(-[a-z0-9]{2,8})*$/i.test(lang) ? lang : 'en';
}

/** Where a service is, in words: `{name, local}`. `name` is the site's
 * name a person reads; `local` says a program on this computer asks (a
 * loopback address); an address that names no site (an IP) gives neither. */
export function serviceWords(origin) {
  let url;
  try {
    url = new URL(origin);
  } catch {
    return {name: null, local: false};
  }
  const address = url.hostname;
  if (address === 'localhost' || address === '127.0.0.1' || address === '[::1]' || address === '::1') {
    return {name: null, local: true};
  }
  // A bare address is no name a person knows: nothing to say.
  if (address.startsWith('[') || /^\d{1,3}(\.\d{1,3}){3}$/.test(address)) return {name: null, local: false};
  return {name: address, local: false};
}

/** When `expiresMs` ends, in `locale`; English where the page's tag is one
 * the browser's date formatter does not know. */
function dateWords(expiresMs, locale) {
  const at = new Date(expiresMs);
  try {
    return at.toLocaleDateString(locale, DATE);
  } catch {
    return at.toLocaleDateString('en', DATE);
  }
}

/**
 * What the card says to the person about `request`: who asks, from where,
 * what the grant reaches and until when. `locale` is the page's own (its
 * `<html lang>`), so the date reads in the language of the words around it.
 * `home` is the workspace the page's own site calls its home
 * (`{workspace, name}`), which names a grant that reaches it.
 */
export function requestWords(request, {locale = pageLocale(), local = globalThis.localStorage, home = null} = {}) {
  const {client, grantScope, grantExpiresMs, grantPages} = request;
  const service = serviceWords(request.audience);
  if (grantScope === null) {
    return {
      heading: service.name ? `Sign in to ${service.name}?` : `Sign in to ${client}?`,
      service: service.local
        ? 'Tilo in this browser will prove to the service on this computer that this is your device.'
        : service.name
          ? `Tilo in this browser will prove to ${service.name} that this is your device.`
          : null,
      reaches: null,
      until: null,
      pages: null,
    };
  }
  const name = workspaceName(grantScope, {local, home});
  return {
    heading: `Let ${client} act for you?`,
    service: service.local ? 'From a terminal on this computer.'
      : service.name ? `Through ${service.name}.` : null,
    reaches: grantScope === 'all'
      ? 'All your workspaces, and it may create new ones'
      : name
        ? `Only the workspace “${name}”`
        : 'Only one of your workspaces',
    until: grantExpiresMs === null ? 'Until you revoke it' : dateWords(grantExpiresMs, locale),
    pages: grantPages
      ? 'It navigates, fills in and clicks the page without asking you each time. Submitting still asks you.'
      : 'It asks you before every page action.',
  };
}

/**
 * The workspaces this browser entered, each by the id a grant names it by
 * and the name the person saw it under (its host's name where none was
 * learned yet): `[{scope, name}]`, most recently opened first, each once.
 */
export function rememberedWorkspaces({local = globalThis.localStorage} = {}) {
  const seen = new Set();
  const workspaces = [];
  for (const {workspace, origin, name} of enteredWorkspaces(local)) {
    if (seen.has(workspace)) continue;
    seen.add(workspace);
    workspaces.push({scope: workspace, name: name ?? new URL(origin).host});
  }
  return workspaces;
}

/** The name this browser knows the workspace `id` by — from a workspace it
 * entered, else from the site's home workspace (`home`, `{workspace, name}`)
 * when the grant reaches that one — or `null` where it knows none: a card
 * then says what it reaches in plain words, never by the id. */
export function workspaceName(id, {local = globalThis.localStorage, home = null} = {}) {
  const known = rememberedWorkspaces({local}).find(workspace => workspace.scope === id)?.name;
  if (known) return known;
  return home?.workspace === id && typeof home.name === 'string' && home.name ? home.name : null;
}

/** The workspace the page's own site calls its home, as `entry.json` names it
 * (`home.name` and the workspace its admission address spells): `{workspace,
 * name}` where it names both, else `null`. */
export function siteHome(entry) {
  const name = typeof entry?.home?.name === 'string' ? entry.home.name.trim() : '';
  const admission = entry?.home?.admission;
  const match = typeof admission === 'string' ? admission.match(/\/directory\/([^/]+)\/?$/) : null;
  return match && name ? {workspace: match[1], name} : null;
}

/** The page's site configuration (`entry.json` beside it), or `null` where
 * this bundle has none: read once, never what the card waits for long. */
async function readSiteEntry(window) {
  try {
    const response = await window.fetch(new URL('entry.json', window.location.href), {cache: 'no-cache'});
    return response.ok ? await response.json() : null;
  } catch {
    return null;
  }
}

/**
 * What the card offers the person to choose what `request`'s grant reaches,
 * or `null` where there is nothing to choose: the service asked for one
 * workspace, or for no grant at all. One that asked for all of them is a
 * choice of the person's workspaces, the service's own ask first, so the
 * agent is held to what they chose and never to more. A server key reaches
 * all of them: the person chose that where they asked for it.
 */
export function grantChoices(request, {local = globalThis.localStorage} = {}) {
  if (request.grantScope !== 'all' || request.grantServerKey) return null;
  const workspaces = rememberedWorkspaces({local});
  return workspaces.length ? [{scope: 'all', name: 'All my workspaces'}, ...workspaces] : null;
}

/** The fields that answer `request` with `{proof, grant}`: what the service
 * resolves its sign-in from. */
export function allowFields(request, {proof, grant}) {
  const fields = new URLSearchParams({pending: request.pending, proof});
  if (grant) fields.set('grant', grant);
  return fields;
}

/** The fields that turn `request` down. */
export function denyFields(request) {
  return new URLSearchParams({pending: request.pending, decision: 'deny'});
}

/**
 * Post `fields` to `action` as a form, which the browser sends and follows:
 * the service answers with the page the person lands on. Resolves to the
 * form, which the page leaves in place until it goes.
 */
export function postForm({document = globalThis.document, action, fields}) {
  const form = document.createElement('form');
  form.method = 'POST';
  form.action = action;
  form.acceptCharset = 'utf-8';
  form.hidden = true;
  for (const [name, value] of fields) {
    const input = document.createElement('input');
    input.type = 'hidden';
    input.name = name;
    input.value = value;
    form.append(input);
  }
  document.body.append(form);
  form.submit();
  return form;
}

/**
 * The two texts the service takes from `request`, signed: the proof of the
 * person, and the grant where the link asked for one, reaching `scope` —
 * what the service asked for unless the person narrowed it. `runtime` is the
 * page's bundle, loaded only now.
 */
export async function signedAnswer(request, {identity, deviceKey, runtime, scope = request.grantScope}) {
  const ask = {
    audience: request.audience,
    challenge: request.challenge,
    client: request.client,
    ...(request.grantKey === null ? {} : {
      grantKey: request.grantKey,
      grantScope: scope,
      expiresAtMs: request.grantExpiresMs,
      serverKey: request.grantServerKey,
      operateBrowserPages: request.grantPages,
    }),
  };
  // A device key kept as a sealed seed is opened for the one call that
  // signs; one held by `WebCrypto` signs where it is.
  const signer = deviceKey.kind === 'sealed-seed' ? await deviceKey.unseal() : deviceKey;
  const answer = JSON.parse(await runtime.signInAnswer(identity, signer, JSON.stringify(ask)));
  if (typeof answer?.proof !== 'string') throw new Error('Tilo could not sign this sign-in.');
  return answer;
}

/** The card's elements, by their ids (`sign-in.html`). */
const ELEMENT_IDS = {
  heading: 'sign-in-heading',
  request: 'sign-in-request',
  service: 'sign-in-service',
  reaches: 'sign-in-reaches',
  reachesRow: 'sign-in-reaches-row',
  scope: 'sign-in-scope',
  scopeRow: 'sign-in-scope-row',
  until: 'sign-in-until',
  untilRow: 'sign-in-until-row',
  pages: 'sign-in-pages',
  pagesRow: 'sign-in-pages-row',
  message: 'sign-in-message',
  actions: 'sign-in-actions',
  allow: 'sign-in-allow',
  cancel: 'sign-in-cancel',
  signedOut: 'sign-in-signed-out',
  signedOutText: 'sign-in-signed-out-text',
  start: 'sign-in-start',
  copy: 'sign-in-copy',
};

/** What a browser with no Tilo identity yet is told under the question. */
export const SIGNED_OUT = 'Sign in to Tilo in this browser to answer.';
/** What a card whose person this page cannot open is told. */
const LOCKED = 'This page lost your sign-in. Reload it to answer.';
/** How often a signed-out card looks whether the person signed in another tab. */
const SIGNED_IN_ELSEWHERE_MS = 1500;

/** The card's elements, read from `document` by their ids. */
export function signInElements(document = globalThis.document) {
  return Object.fromEntries(Object.entries(ELEMENT_IDS).map(([name, id]) => [name, document.getElementById(id)]));
}

/**
 * Show the person what `location.search` asks, and answer it when they
 * say so. Resolves once the card is ready, not when the person decides: a
 * decision posts a form away from this page.
 *
 * The boundaries (all defaulted for the page): `elements` the card's own,
 * `storage` where this browser keeps its person, `local` where it keeps the
 * workspaces it entered, `identity` the person already unlocked, `loadRuntime`
 * the page's bundle, `openDeviceKey` the device key store.
 */
export async function startSignInPage({
  window = globalThis,
  elements,
  storage,
  local = globalThis.localStorage,
  identity,
  loadRuntime = async () => (await import('./host.mjs')).loadRuntime('peer'),
  openDeviceKey = openStoredDeviceKey,
} = {}) {
  const {document, location} = window;
  const card = elements ?? signInElements(document);
  const show = (part, shown = true) => {
    if (card[part]) card[part].hidden = !shown;
  };
  const tell = text => {
    card.message.textContent = text;
    show('message', Boolean(text));
  };
  const stop = words => {
    card.heading.textContent = words;
    show('request', false);
    show('actions', false);
    tell('');
    document.documentElement.dataset.signIn = 'stopped';
  };
  const decided = busy => {
    card.allow.disabled = busy;
    card.cancel.disabled = busy;
  };

  document.documentElement.dataset.signIn = 'checking';
  let request;
  try {
    request = readSignInRequest(location.search);
  } catch (error) {
    return stop(error instanceof SignInLinkError ? error.message : 'This sign-in link cannot be read.');
  }
  tell('');
  // What the page can know of the person's workspaces without the runtime:
  // those this browser entered, and the site's own home (`entry.json`).
  const entry = await readSiteEntry(window);
  const home = siteHome(entry);
  const locale = pageLocale(document);
  let listening = false;
  if (!(await keptIdentity({storage}))) {
    // Nobody is signed in here yet. The card says what the link asks, and
    // offers the way in right where it is: Tilo itself in another tab. Once
    // they are in, the card goes on to the question by itself: the link is
    // not followed again.
    return offerSignIn({window, card, show, tell, storage,
      words: requestWords(request, {locale, local, home}), then: askRequest});
  }
  return askRequest();

  function askRequest() {
    const words = requestWords(request, {locale, local, home});
    card.heading.textContent = words.heading;
    card.service.textContent = words.service ?? '';
    show('service', words.service !== null);
    card.reaches.textContent = words.reaches ?? '';
    show('reachesRow', words.reaches !== null);
    // Where the service asked for all of the person's workspaces, they say
    // which it reaches; where it asked for one, the card names it and holds
    // the agent to it. Either way what is certified is what is shown.
    let scope = request.grantScope;
    const choices = grantChoices(request, {local});
    if (choices) {
      show('reachesRow', false);
      card.scope.replaceChildren(...choices.map((choice, index) => {
        const option = document.createElement('label');
        option.className = 'tilo-sign-in-scope';
        const radio = document.createElement('input');
        radio.type = 'radio';
        radio.name = 'sign-in-scope';
        radio.value = choice.scope;
        radio.checked = index === 0;
        radio.addEventListener('change', () => {
          if (radio.checked) scope = choice.scope;
        });
        const name = document.createElement('span');
        name.textContent = choice.name;
        option.append(radio, name);
        return option;
      }));
    }
    show('scopeRow', Boolean(choices));
    card.until.textContent = words.until ?? '';
    show('untilRow', words.until !== null);
    card.pages.textContent = words.pages ?? '';
    show('pagesRow', words.pages !== null);
    for (const name of ['allow', 'cancel']) card[name].textContent = name === 'allow' ? 'Allow' : 'Cancel';
    show('request');
    show('actions');
    document.documentElement.dataset.signIn = 'asked';
    if (listening) return;
    listening = true;

    card.cancel.addEventListener('click', () => {
      decided(true);
      postForm({document, action: request.returnTo, fields: denyFields(request)});
    });
    card.allow.addEventListener('click', async () => {
      decided(true);
      tell('Signing you in…');
      let deviceKey;
      try {
        const person = identity ?? await browserIdentity({storage, create: false});
        if (!person) {
          // Signed out since the card asked (the site's data cleared in
          // another tab): the ways in again, and the question after them.
          decided(false);
          show('request', false);
          show('actions', false);
          return offerSignIn({window, card, show, tell, storage,
            words: requestWords(request, {locale, local, home}), then: askRequest});
        }
        if (person.locked || !person.sign) throw new SignInLinkError(LOCKED);
        const runtime = await loadRuntime();
        deviceKey = await openDeviceKey({
          profile: identityProfile(person, request.audience),
          storage: idbStorage(),
        });
        const answer = await signedAnswer(request, {identity: person, deviceKey, runtime, scope});
        tell('');
        postForm({document, action: request.returnTo, fields: allowFields(request, answer)});
      } catch (error) {
        decided(false);
        tell(error instanceof SignInLinkError ? error.message : 'Tilo could not answer this sign-in. Try again.');
        document.documentElement.dataset.signIn = 'failed';
        await deviceKey?.close?.();
      }
    });
  }
}

/**
 * The card of a browser with nobody signed in: the question's heading, and
 * the way in right on it. Tilo in another tab signs them in (a new person
 * starts there), and the card notices once they are in and goes on to the
 * question (`then`). The person never has to follow the link again.
 */
function offerSignIn({window, card, show, tell, storage, words, then}) {
  const {document} = window;
  document.documentElement.dataset.signIn = 'signed-out';
  card.heading.textContent = words.heading;
  card.service.textContent = words.service ?? '';
  show('service', words.service !== null);
  // What the request reaches is said before the person signs in to answer it,
  // not only after: a key that reaches one workspace says which.
  card.reaches.textContent = words.reaches ?? '';
  show('reachesRow', words.reaches !== null);
  if (card.signedOutText) card.signedOutText.textContent = SIGNED_OUT;
  for (const row of ['scopeRow', 'untilRow', 'pagesRow']) show(row, false);
  show('request');
  show('actions', false);
  tell('');
  let done = false;
  const signedIn = () => {
    if (done) return;
    done = true;
    clearInterval(timer);
    window.removeEventListener?.('focus', look);
    show('signedOut', false);
    then();
  };
  const look = async () => {
    if (!done && await keptIdentity({storage}).catch(() => null)) signedIn();
  };
  // Signed in from another tab of the site: the person comes back here.
  const timer = setInterval(look, SIGNED_IN_ELSEWHERE_MS);
  timer?.unref?.();
  window.addEventListener?.('focus', look);

  if (card.start) {
    card.start.textContent = 'Open Tilo';
    card.start.href = new URL('./', window.location.href).href;
  }
  if (card.copy) {
    card.copy.textContent = 'Copy this link';
    card.copy.onclick = async () => {
      try {
        await window.navigator.clipboard.writeText(window.location.href);
        tell('Copied. Open it in the browser where you use Tilo.');
      } catch {
        tell('Couldn’t copy. Copy the address from the address bar instead.');
      }
    };
  }
  show('signedOut');
}
