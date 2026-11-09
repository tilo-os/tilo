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
 * ```
 *
 * Everything in it is checked before the page shows anything, and nothing is
 * signed until the person presses Allow: then this browser's device key,
 * certified by the person's own root for this answer, proves them to the
 * service (`tilo-browser-peer`'s `sign_in_proof`), and — where the service
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
 * sign-in link is not a workspace, and the desktop's bundle has no part in
 * answering one.
 */

import {openDeviceCustody, idbStorage} from './device-custody.mjs';
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
 * still ahead; it is a seam for tests, not a value the page chooses.
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
  if (granted.length === 3) {
    grantKey = once('grant_key').toLowerCase();
    if (!HEX32.test(grantKey)) throw new SignInLinkError('This link asks for a grant Tilo cannot give.');
    grantScope = once('grant_scope');
    if (grantScope !== 'all' && !UUID.test(grantScope)) {
      throw new SignInLinkError('This link asks a grant to reach a workspace Tilo cannot read.');
    }
    const expires = once('grant_expires');
    if (!MILLISECONDS.test(expires)) throw new SignInLinkError('This link asks for a grant Tilo cannot give.');
    grantExpiresMs = Number(expires);
    if (grantExpiresMs <= now) {
      throw new SignInLinkError('This request has expired. Ask the service for a new sign-in link.');
    }
    if (grantExpiresMs - now > MAX_GRANT_LIFETIME_MS) {
      throw new SignInLinkError('This link asks for a grant longer than Tilo gives. Ask the service for a shorter one.');
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
  });
}

/**
 * What the card says to the person about `request`: who asks, through
 * which service, what the grant reaches and until when. `locale` is the
 * page's (tests fix it; the page leaves it to the browser).
 */
export function requestWords(request, {locale = undefined, local = globalThis.localStorage} = {}) {
  const {client, host, grantScope, grantExpiresMs} = request;
  if (grantScope === null) {
    return {
      heading: `Sign in to ${host}?`,
      service: `Tilo in this browser will prove to ${host} that this is your device.`,
      reaches: null,
      until: null,
    };
  }
  return {
    heading: `Let ${client} act for you?`,
    service: `Through ${host}.`,
    reaches: grantScope === 'all'
      ? 'All your workspaces, and it may create new ones'
      : `Only the workspace ${workspaceName(grantScope, {local})}`,
    until: new Date(grantExpiresMs).toLocaleDateString(locale, {year: 'numeric', month: 'long', day: 'numeric'}),
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
  for (const {room, origin, name} of enteredWorkspaces(local)) {
    if (seen.has(room)) continue;
    seen.add(room);
    workspaces.push({scope: room, name: name ?? new URL(origin).host});
  }
  return workspaces;
}

/** The name this browser knows the workspace `id` by, or the id itself where
 * it knows none: what a card names a workspace it was asked to reach. */
export function workspaceName(id, {local = globalThis.localStorage} = {}) {
  return rememberedWorkspaces({local}).find(workspace => workspace.scope === id)?.name ?? id;
}

/**
 * What the card offers the person to choose what `request`'s grant reaches,
 * or `null` where there is nothing to choose: the service asked for one
 * workspace, or for no grant at all. One that asked for all of them is a
 * choice of the person's workspaces, the service's own ask first, so the
 * agent is held to what they chose and never to more.
 */
export function grantChoices(request, {local = globalThis.localStorage} = {}) {
  if (request.grantScope !== 'all') return null;
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
export async function signedAnswer(request, {identity, custody, runtime, scope = request.grantScope}) {
  const ask = {
    audience: request.audience,
    challenge: request.challenge,
    client: request.client,
    ...(request.grantKey === null ? {} : {grantKey: request.grantKey, grantScope: scope, expiresAtMs: request.grantExpiresMs}),
  };
  // A device key kept as a sealed seed is opened for the one call that
  // signs; one held by `WebCrypto` signs where it is.
  const signer = custody.kind === 'sealed-seed' ? await custody.unseal() : custody;
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
  message: 'sign-in-message',
  actions: 'sign-in-actions',
  allow: 'sign-in-allow',
  cancel: 'sign-in-cancel',
};

/** The card's elements, read from `document` by their ids. */
export function signInElements(document = globalThis.document) {
  return Object.fromEntries(Object.entries(ELEMENT_IDS).map(([name, id]) => [name, document.getElementById(id)]));
}

/**
 * Show the person what `location.search` asks, and answer it when they
 * say so. Resolves once the card is ready, not when the person decides: a
 * decision posts a form away from this page.
 *
 * The seams (all defaulted for the page): `elements` the card's own,
 * `storage` where this browser keeps its person, `local` where it keeps the
 * workspaces it entered, `identity` the person already unlocked, `loadRuntime`
 * the page's bundle, `openCustody` the device key store.
 */
export async function startSignInPage({
  window = globalThis,
  elements,
  storage,
  local = globalThis.localStorage,
  identity,
  loadRuntime = async () => (await import('./host.mjs')).loadRuntime(),
  openCustody = openDeviceCustody,
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
  if (!(await keptIdentity({storage}))) {
    return stop('Open Tilo in this browser and sign in first, then follow this link again.');
  }

  const words = requestWords(request, {local});
  card.heading.textContent = words.heading;
  card.service.textContent = words.service;
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
  for (const name of ['allow', 'cancel']) card[name].textContent = name === 'allow' ? 'Allow' : 'Cancel';
  show('request');
  show('actions');
  document.documentElement.dataset.signIn = 'asked';

  card.cancel.addEventListener('click', () => {
    decided(true);
    postForm({document, action: request.returnTo, fields: denyFields(request)});
  });
  card.allow.addEventListener('click', async () => {
    decided(true);
    tell('Signing you in…');
    let custody;
    try {
      const person = identity ?? await browserIdentity({storage, create: false});
      if (!person || person.locked || !person.sign) {
        throw new SignInLinkError('Open Tilo in this browser and sign in first, then follow this link again.');
      }
      const runtime = await loadRuntime();
      custody = await openCustody({
        profile: identityProfile(person, request.audience),
        storage: idbStorage(),
      });
      const answer = await signedAnswer(request, {identity: person, custody, runtime, scope});
      tell('');
      postForm({document, action: request.returnTo, fields: allowFields(request, answer)});
    } catch (error) {
      decided(false);
      tell(error instanceof SignInLinkError ? error.message : 'Tilo could not answer this sign-in. Try again.');
      document.documentElement.dataset.signIn = 'failed';
      await custody?.close?.();
    }
  });
}
