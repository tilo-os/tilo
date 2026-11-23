import assert from 'node:assert/strict';
import test from 'node:test';
import {boot, chosenRoute, initialRoute, readEntry} from './entry.mjs';
import {runtimeFake} from './runtime-surface.mjs';

/** The runtime's way in (tilo-session-client's `way_in`, tested there),
 * answering the ways this page's scenario takes, in turn and again for each
 * entry. */
const entering = (...ways) => {
  let next = 0;
  const way = () => ways[next++ % ways.length];
  return {wayIn: way, wayInAfter: way, admittedAsAsked: () => true};
};

const message = {kind: 'chat.message', id: 'chat.message:1'};

test('an unbound desktop entry without a route is the person’s own desktop', () => {
  const link = {parse: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); }};
  assert.deepEqual(initialRoute({link, location: {hash: '', href: 'https://tilo.so/'}, mode: 'desktop'}),
    {mode: 'desktop', target: {scope: {kind: 'own'}}});
  assert.throws(() => initialRoute({link, location: {hash: '', href: 'https://tilo.so/app/'}, mode: 'app'}), {code: 'no-route'});
});

test('a bare entry opens the web server’s home workspace, its workspace learned on admission', () => {
  const link = {parse: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); }};
  const home = {name: 'Commons', host: {origin: 'https://workspaces.example/'}, admission: 'https://workspaces.example/'};
  const scope = {kind: 'workspace', host: 'https://workspaces.example', workspace: null};
  assert.deepEqual(initialRoute({link, location: {hash: '', href: 'https://workspaces.example/open/'}, mode: 'desktop', home}),
    {mode: 'desktop', target: {scope}});
  assert.deepEqual(initialRoute({link, location: {hash: '', href: 'https://workspaces.example/open/app/'}, mode: 'app', home}),
    {mode: 'desktop', target: {scope}});
  assert.deepEqual(initialRoute({link, location: {hash: '', href: 'https://workspaces.example/open/app/'}, mode: 'app', home: {...home, app: 'so.tilo.chat'}}),
    {mode: 'app', target: {scope, app: 'so.tilo.chat'}});
  assert.deepEqual(initialRoute({link, location: {hash: '', href: 'https://workspaces.example/open/'}, mode: 'desktop', home: {...home, app: 'so.tilo.chat'}}),
    {mode: 'desktop', target: {scope, app: 'so.tilo.chat'}});
});

test('the entry is the page’s directory, with the home workspace the web server names', async () => {
  const home = {name: 'Commons', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
  const fetch = async url => ({ok: String(url) === 'https://workspaces.example/open/entry.json', json: async () => ({home})});
  assert.deepEqual(await readEntry({mode: 'app', location: {href: 'https://workspaces.example/open/app/?embed=1#/so.tilo.chat'}, fetch}),
    {base: 'https://workspaces.example/open/', home});
  const tickets = async () => ({ok: true, json: async () => ({tickets: 'https://tilo.so/workspaces/', home: {name: 'no host'}})});
  assert.deepEqual(await readEntry({mode: 'desktop', location: {href: 'https://tilo.so/'}, fetch: tickets}),
    {base: 'https://tilo.so/', tickets: 'https://tilo.so/workspaces/'});
  assert.deepEqual(await readEntry({mode: 'desktop', location: {href: 'https://tilo.so/#/me'}, fetch: async () => ({ok: false})}),
    {base: 'https://tilo.so/'});
});

test('the entry names the embedding site only when it is exactly an https origin', async () => {
  const served = embedder => async () => ({ok: true, json: async () => ({embedder})});
  const read = embedder => readEntry({mode: 'desktop', location: {href: 'https://app.example/'}, fetch: served(embedder)});
  assert.deepEqual(await read('https://site.example'), {base: 'https://app.example/', embedder: 'https://site.example'});
  for (const refused of ['http://site.example', 'https://site.example/', 'https://site.example/page', 'site.example', 7]) {
    assert.deepEqual(await read(refused), {base: 'https://app.example/'}, String(refused));
  }
});

test('the catalogue is never read before admission', async () => {
  const order = [];
  const target = {scope: {kind: 'workspace', host: 'https://h.example', workspace: 'r'}, app: 'so.tilo.chat', record: '', resource: message};
  const runtime = runtimeFake({
    parseLink: () => JSON.stringify({mode: 'app', target}),
    formatLink: () => 'https://tilo.so/app/#/@h.example/w/r/so.tilo.chat/-/chat.message:1',
    ...entering('accounts', 'member'),
  });
  const peer = {whenReady: async () => order.push('ready')};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://tilo.so/app/#/@h.example/w/r/so.tilo.chat/-/chat.message:1', hash: '#/@h.example/w/r',
      replace: url => order.push(url)},
    history: {replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => runtime, fetch: async () => ({ok: false}),
    readAccounts: async () => { order.push('accounts'); return [{userPublicKey: 'ab'}]; },
    connectMember: async () => { order.push('admitted'); return peer; },
  };
  // The page takes the browser over once admitted: no app catalogue is read here at all,
  // and the message the link named is what that page opens first.
  assert.equal(await boot({mode: 'app', window, deps}), null);
  assert.deepEqual(order, ['accounts', 'admitted', 'ready', 'https://tilo.so/w/r/chat/chat.message%3A1']);
});

test('the home workspace opens through its host entry, and the address becomes its page', async () => {
  const home = {name: 'Commons', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
  const formatted = [];
  const replaced = [];
  const runtime = runtimeFake({
    parseLink: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); },
    formatLink: route => { formatted.push(JSON.parse(route)); return 'https://workspaces.example/open/#/@workspaces.example/w/WORKSPACE'; },
    ...entering('entry'),
  });
  // The host replays the workspace while the page waits: three records arrive.
  const peer = {workspace: 'WORKSPACE', whenReady: async ({onProgress} = {}) => { onProgress?.(3); }};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://workspaces.example/open/', hash: '', replace: url => replaced.push(url)},
    history: {state: null, replaceState: (state, title, url) => replaced.push(url), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const entered = [];
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: true, json: async () => ({home})}),
    readAccounts: async () => [],
    enterViaHost: async ({workspace}) => { entered.push(workspace); return peer; },
  };
  const shown = [];
  const status = {
    text: 'Loading…',
    get textContent() { return this.text; },
    set textContent(text) { shown.push(text); this.text = text; },
  };
  const steps = [];
  assert.equal(await boot({mode: 'desktop', window, status, deps, onStep: step => steps.push(step)}), null);
  assert.deepEqual(entered, [home]);
  // Progress while joining, then the loading note again, which the page takes over from.
  assert.ok(shown.includes('Joining…'), shown.join(' | '));
  assert.equal(status.text, 'Loading…');
  // The page hears it is joining through the entry, then opening once admitted.
  assert.deepEqual([...new Set(steps)], ['join', 'open']);
  assert.equal(steps.at(-1), 'open');
  assert.deepEqual(replaced, ['https://workspaces.example/open/w/WORKSPACE'], 'the page takes the browser from there');
});

test('a first visit’s first screen holds the page, and the entry loads it only once the screen lets go', async () => {
  // A newcomer sets up their own workspace before anything else: while the
  // screen that does it is on the page, the entry keeps the browser it was
  // admitted as, and the page it would have opened waits in the page's hands.
  const home = {name: 'Commons', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
  const runtime = runtimeFake({
    parseLink: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); },
    formatLink: () => 'https://workspaces.example/open/#/@workspaces.example/w/WORKSPACE',
    ...entering('entry'),
  });
  const peer = {workspace: 'WORKSPACE', whenReady: async () => {}};
  const replaced = [];
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://workspaces.example/open/', hash: '', replace: url => replaced.push(url)},
    history: {state: null, replaceState: (state, title, url) => replaced.push(url), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const held = [];
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: true, json: async () => ({home})}),
    readAccounts: async () => [],
    enterViaHost: async () => peer,
    holdFirstScreen: page => { held.push(page); return true; },
  };
  assert.equal(await boot({mode: 'desktop', window, deps}), null);
  assert.deepEqual(replaced, [], 'nothing is loaded under the first screen');
  assert.deepEqual(held.map(page => page.url), ['https://workspaces.example/open/w/WORKSPACE']);
  assert.equal(held[0].workspace, 'WORKSPACE');
  assert.equal(held[0].entry.base, 'https://workspaces.example/open/', 'the page it holds opens the person’s own workspace');
});

test('a visit whose page holds nothing is handed over as before', async () => {
  const home = {name: 'Commons', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
  const runtime = runtimeFake({
    parseLink: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); },
    formatLink: () => 'https://workspaces.example/open/#/@workspaces.example/w/WORKSPACE',
    ...entering('entry'),
  });
  const peer = {workspace: 'WORKSPACE', whenReady: async () => {}};
  const replaced = [];
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://workspaces.example/open/', hash: '', replace: url => replaced.push(url)},
    history: {state: null, replaceState: (state, title, url) => replaced.push(url), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: true, json: async () => ({home})}),
    readAccounts: async () => [],
    enterViaHost: async () => peer,
    holdFirstScreen: () => false,
  };
  assert.equal(await boot({mode: 'desktop', window, deps}), null);
  assert.deepEqual(replaced, ['https://workspaces.example/open/w/WORKSPACE'], 'the page takes the browser over');
});

test('an invitation whose server only it names admits there, and the address names that server from then on', async () => {
  // A link carrying an invitation and no server of its own (`#/w/<workspace>?invite=…`): the
  // invitation names a server reached over iroh alone, which the page joins at, and the next
  // visit comes back to it there rather than at the page's own site.
  const invited = 'https://f97887d9895250ef7d60f7929cc947cc.iroh.invalid';
  const scope = {kind: 'workspace', host: 'https://tilo.example', workspace: 'W'};
  const formatted = [];
  const replaced = [];
  const runtime = runtimeFake({
    parseLink: () => JSON.stringify({mode: 'desktop', target: {scope}}),
    formatLink: route => { formatted.push(JSON.parse(route)); return 'https://tilo.example/#/@iroh/w/W'; },
    ...entering('invitation'),
  });
  const peer = {workspace: 'W', origin: invited, whenReady: async () => {}};
  const window = {
    __tiloLinkSecrets: Object.freeze({invite: 'SIGNED'}),
    location: {href: 'https://tilo.example/#/w/W', hash: '#/w/W', replace: url => replaced.push(url)},
    history: {state: null, replaceState: (state, title, url) => replaced.push(url), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const joined = [];
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: false}),
    readAccounts: async () => [],
    openInvitation: async (origin, workspace, secret) => secret,
    invitedServer: async () => ({origin: invited, offer: {workspace: 'W', role: 'editor'}}),
    confirmInvitation: async () => true,
    joinWorkspace: async options => { joined.push(options.origin); return peer; },
  };
  const page = await boot({mode: 'desktop', window, status: null, deps});
  assert.equal(page, null);
  assert.deepEqual(joined, [invited]);
  assert.equal(replaced.at(-1), 'https://tilo.example/w/W', 'the page takes the browser at the server it joined');
});

test('an address that names no server opens its workspace at the server the site names, not at the site', async () => {
  // `#/w/<workspace>` names no server, which the link grammar reads as the page's own site. A site
  // that names its home workspace's server elsewhere keeps no workspace itself: the person comes
  // back to the workspace at that server, as the address the page writes from then on says.
  const site = 'https://tilo.example';
  const home = 'https://eu1.tilo.example';
  const reconnected = [];
  const runtime = runtimeFake({
    parseLink: () => JSON.stringify({mode: 'desktop', target: {scope: {kind: 'workspace', host: site, workspace: 'W'}}}),
    ...entering('reconnect'),
  });
  const peer = {workspace: 'W', origin: home, whenReady: async () => {}};
  const replaced = [];
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: `${site}/#/w/W`, hash: '#/w/W', replace: url => replaced.push(url)},
    history: {state: null, replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async url => ({ok: String(url) === `${site}/entry.json`, json: async () => ({home: {name: 'Lounge', host: {origin: home}}})}),
    readAccounts: async () => [],
    holdsPerson: async () => true,
    reconnect: async ({origin, workspace}) => { reconnected.push([origin, workspace]); return peer; },
  };
  const page = await boot({mode: 'desktop', window, status: null, deps});
  assert.equal(page, null);
  assert.deepEqual(reconnected, [[home, 'W']], 'the person comes back at the site’s server');
  assert.equal(replaced.at(-1), `${site}/w/W`, 'and the page names the workspace there');
});

test('a wait-list invitation’s link opens the person’s own workspace, and the address names it from then on; a link into the workspace’s chat opens the chat', async () => {
  // The mail a wait-list address is sent carries `#/w/<lounge>?invite=…`:
  // the page joins the Lounge with it, then opens the person's own workspace
  // in its place, once, with no load of its own.
  const walk = async target => {
    const scope = {kind: 'workspace', host: 'https://tilo.example', workspace: 'LOUNGE'};
    const formatted = [];
    const replaced = [];
    const runtime = runtimeFake({
      parseLink: () => JSON.stringify({mode: 'desktop', target: {scope, ...target}}),
      formatLink: route => { formatted.push(JSON.parse(route)); return `https://tilo.example/#/w/${JSON.parse(route).target.scope.workspace}`; },
      ...entering('invitation'),
    });
    const peerOf = workspace => ({workspace, origin: 'https://tilo.example', whenReady: async () => {}, dispose: async () => {}});
    const window = {
      __tiloLinkSecrets: Object.freeze({invite: 'SIGNED'}),
      location: {href: 'https://tilo.example/#/w/LOUNGE', hash: '#/w/LOUNGE', replace: url => replaced.push(url)},
      history: {state: null, replaceState: (state, title, url) => replaced.push(url), pushState() {}},
      addEventListener() {}, removeEventListener() {},
    };
    const deps = {
      loadRuntime: async () => runtime,
      fetch: async () => ({ok: false}),
      readAccounts: async () => [],
      openInvitation: async (origin, workspace, secret) => secret,
      inspectInvitation: async () => ({workspace: 'LOUNGE', role: 'editor', waitList: true}),
      confirmInvitation: async () => true,
      joinWorkspace: async () => peerOf('LOUNGE'),
      ownWorkspace: async () => ({workspace: 'OWN', name: 'Ada'}),
      reconnect: async ({workspace}) => peerOf(workspace),
    };
    const page = await boot({mode: 'desktop', window, status: null, deps});
    return {page, replaced};
  };
  const own = await walk({});
  assert.equal(own.page, null);
  assert.equal(own.replaced.at(-1), 'https://tilo.example/w/OWN', 'the address names their own workspace');
  const chat = await walk({app: 'so.tilo.chat'});
  assert.equal(chat.page, null);
  assert.equal(chat.replaced.at(-1), 'https://tilo.example/w/LOUNGE/chat', 'a link into the chat opens the chat');
});

test('a link whose document id is damaged opens its workspace, and the address it names leaves the damaged part out', async () => {
  const scope = {kind: 'workspace', host: 'https://eu1.tilo.sh', workspace: 'WORKSPACE'};
  const replaced = [];
  const runtime = runtimeFake({
    // The runtime's grammar names the workspace of a link whose document is damaged, and says so.
    parseLink: () => JSON.stringify({mode: 'desktop', target: {scope}, damaged: 'The document id in this link is damaged'}),
    formatLink: route => { assert.equal(JSON.parse(route).damaged, undefined); return 'https://tilo.sh/#/@eu1.tilo.sh/w/WORKSPACE'; },
    ...entering('accounts', 'member'),
  });
  const peer = {workspace: 'WORKSPACE', whenReady: async () => {}};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://tilo.sh/#/@eu1.tilo.sh/w/WORKSPACE/so.tilo.pdf-viewer/application.pdf',
      hash: '#/@eu1.tilo.sh/w/WORKSPACE/so.tilo.pdf-viewer/application.pdf', replace: url => replaced.push(url)},
    history: {state: null, replaceState: (state, title, url) => replaced.push(url), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const page = await boot({mode: 'desktop', window, status: null, deps: {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: false}),
    readAccounts: async () => [{userPublicKey: 'ab'}],
    connectMember: async () => peer,
  }});
  assert.equal(page, null);
  assert.deepEqual(replaced, ['https://tilo.sh/w/WORKSPACE'], 'the page takes the browser to the workspace, without the damaged part');
});

test('the page hears the peer the host let in before it hands the browser to the page, so the page can name the workspace', async () => {
  const home = {name: 'Lounge', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
  const runtime = runtimeFake({
    parseLink: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); },
    formatLink: () => 'https://workspaces.example/#/@workspaces.example/w/WORKSPACE',
    ...entering('entry'),
  });
  const heard = [];
  const peer = {workspace: 'WORKSPACE', workspaceName: 'Kaffeeküche', whenReady: async () => {}};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://workspaces.example/', hash: '', replace: url => heard.push(url)},
    history: {state: null, replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: true, json: async () => ({home})}),
    readAccounts: async () => [],
    enterViaHost: async () => peer,
    onAdmitted: admitted => heard.push(admitted.workspaceName),
  };
  assert.equal(await boot({mode: 'desktop', window, status: null, deps}), null);
  assert.deepEqual(heard, ['Kaffeeküche', 'https://workspaces.example/w/WORKSPACE']);
});

test('a short link asks its workspace’s host for the invitation it keeps and opens that workspace, as its own link', async () => {
  const site = 'https://tilo.sh';
  const host = 'https://workspaces.example';
  const key = 'A'.repeat(43);
  const opened = [];
  const inspected = [];
  const confirmed = [];
  const joined = [];
  const replaced = [];
  const scope = {kind: 'workspace', host, workspace: 'WORKSPACE'};
  const runtime = runtimeFake({
    parseLink: () => JSON.stringify({mode: 'desktop', target: {scope}}),
    formatLink: () => `${site}/#/@workspaces.example/w/WORKSPACE`,
    ...entering('invitation'),
  });
  const offer = {workspace: 'WORKSPACE', workspaceName: 'Lounge', role: 'commenter', members: 4,
    inviter: {name: 'Nimble Panda', userPublicKey: 'ab'.repeat(32)}};
  const peer = {workspace: 'WORKSPACE', origin: host, userPublicKey: 'cd'.repeat(32), whenReady: async () => {}};
  const window = {
    __tiloLinkSecrets: Object.freeze({invite: key}),
    location: {href: `${site}/#/@workspaces.example/w/WORKSPACE`, hash: '#/@workspaces.example/w/WORKSPACE', host: 'tilo.sh',
      replace: url => replaced.push(url)},
    history: {state: null, replaceState: (state, title, url) => replaced.push(url), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: true, json: async () => ({home: {name: 'Lounge', host: {origin: host}}})}),
    readAccounts: async () => [],
    // The workspace keeps the invitation, not the link: the page asks its
    // host for it with the secret the fragment carried.
    openInvitation: async (origin, workspace, secret) => { opened.push([origin, workspace, secret]); return 'OPENED'; },
    inspectInvitation: async (origin, invitation) => { inspected.push([origin, invitation]); return offer; },
    confirmInvitation: async shown => { confirmed.push(shown); return true; },
    joinWorkspace: async options => { joined.push(options); return peer; },
  };
  const page = await boot({mode: 'desktop', window, deps});
  assert.equal(page, null);
  assert.deepEqual(opened, [[host, 'WORKSPACE', key]]);
  assert.deepEqual(inspected, [[host, 'OPENED']]);
  assert.deepEqual(confirmed, [offer], 'the person is shown the invitation before it is joined');
  assert.equal(joined[0].origin, host);
  assert.equal(joined[0].invitation, 'OPENED');
  assert.equal(replaced.at(-1), `${site}/w/WORKSPACE`, 'the page takes the browser at the workspace’s own link');
});

test('a bare entry under a home workspace asks the host’s entry once the runtime says that is the way in', async () => {
  const home = {name: 'Commons', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
  const order = [];
  let loaded;
  const runtime = runtimeFake({
    parseLink: () => { throw new Error('a home entry needs no grammar to be admitted'); },
    formatLink: () => 'https://workspaces.example/open/#/@workspaces.example/w/WORKSPACE',
    ...entering('entry'),
  });
  const peer = {workspace: 'WORKSPACE', whenReady: async () => {}};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://workspaces.example/open/', hash: '', replace: url => order.push(url)},
    history: {state: null, replaceState: () => order.push('canonical'), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: () => new Promise(resolve => { loaded = () => { order.push('runtime'); resolve(runtime); }; }),
    fetch: async () => { loaded(); return {ok: true, json: async () => ({home})}; },
    enterViaHost: async () => {
      order.push('entry');
      return peer;
    },
  };
  assert.equal(await boot({mode: 'desktop', window, deps}), null);
  // The page takes the browser from there.
  assert.deepEqual(order, ['runtime', 'entry', 'https://workspaces.example/open/w/WORKSPACE']);
});

test('any other link waits for the runtime’s grammar before admission', async () => {
  const order = [];
  const runtime = runtimeFake({
    parseLink: () => { order.push('parse'); return JSON.stringify({mode: 'desktop', target: {scope: {kind: 'own'}}}); },
    formatLink: () => 'https://tilo.so/#/me',
    ...entering('own-desktop'),
  });
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://tilo.so/#/me', hash: '#/me'},
    history: {state: null, replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => { order.push('runtime'); return runtime; },
    fetch: async () => ({ok: false}),
  };
  // The person's own desktop is no workspace the web shows: the page says so.
  await assert.rejects(boot({mode: 'desktop', window, deps}), /opens no workspace/);
  assert.deepEqual(order, ['runtime', 'parse']);
});

test('an admitted browser is handed to the page, and one the host refuses is not', async () => {
  const home = {name: 'Commons', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/', app: 'so.tilo.chat'};
  const run = async enterViaHost => {
    const order = [];
    const window = {
      __tiloLinkSecrets: Object.freeze({}),
      location: {href: 'https://workspaces.example/open/', hash: '', replace: url => order.push(url)},
      history: {state: null, replaceState() {}, pushState() {}},
      addEventListener() {}, removeEventListener() {},
    };
    const deps = {
      loadRuntime: async () => runtimeFake({...entering('entry'), formatLink: () => 'https://workspaces.example/open/#/@workspaces.example/w/r1'}),
      fetch: async () => ({ok: true, json: async () => ({home})}),
      enterViaHost: async () => { order.push('entry'); return enterViaHost(); },
    };
    void boot({mode: 'desktop', window, deps}).catch(error => order.push(error.message));
    await new Promise(resolve => setTimeout(resolve, 10));
    return order;
  };
  // The app the home names is the page it opens on the server; nothing of it is
  // asked of the entry.
  assert.deepEqual(await run(async () => ({workspace: 'r1'})), ['entry', 'https://workspaces.example/open/w/r1/chat']);
  assert.deepEqual(await run(async () => { throw new Error('closed'); }), ['entry', 'closed']);
});

test('only a bare desktop entry under a home reopens the workspace the person last chose', () => {
  const home = {name: 'Lounge', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
  const chosen = {origin: 'https://acme.example', workspace: 'A'};
  const bare = {hash: '', href: 'https://tilo.example/'};
  assert.deepEqual(chosenRoute({location: bare, mode: 'desktop', home, chosen}),
    {mode: 'desktop', target: {scope: {kind: 'workspace', host: 'https://acme.example', workspace: 'A'}}});
  assert.equal(chosenRoute({location: {hash: '#/@workspaces.example/w/X', href: 'https://tilo.example/#/@workspaces.example/w/X'}, mode: 'desktop', home, chosen}), null);
  assert.equal(chosenRoute({location: bare, mode: 'app', home, chosen}), null);
  assert.equal(chosenRoute({location: bare, mode: 'desktop', home: null, chosen}), null);
  assert.equal(chosenRoute({location: bare, mode: 'desktop', home, chosen: null}), null);
});

function workspacePage({href, hash, parse}) {
  const calls = {replaces: [], scopes: []};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href, hash, replace: url => calls.replaces.push(url)},
    history: {state: null, replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const runtime = runtimeFake({parseLink: url => JSON.stringify(parse(url)), formatLink: () => window.location.href,
    ...entering('accounts', 'member')});
  return {window, calls, runtime};
}

test('a returning person’s bare entry opens the chosen workspace through their membership, not the home’s entry', async () => {
  const home = {name: 'Lounge', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
  const page = workspacePage({href: 'https://tilo.example/', hash: '', parse: () => { throw new Error('a chosen workspace needs no parse'); }});
  const connected = [];
  const peer = {workspace: 'A', whenReady: async () => {}};
  const deps = {
    loadRuntime: async () => page.runtime,
    fetch: async () => ({ok: true, json: async () => ({home})}),
    chosenWorkspace: () => ({origin: 'https://acme.example', workspace: 'A'}),
    onScope: seen => page.calls.scopes.push(seen),
    readAccounts: async origin => [{userPublicKey: 'ab', origin}],
    connectMember: async options => { connected.push(options); return peer; },
    enterViaHost: async () => { throw new Error('the home’s entry is not asked'); },
  };
  assert.equal(await boot({mode: 'desktop', window: page.window, deps}), null);
  assert.deepEqual(connected.map(({origin, workspace}) => [origin, workspace]), [['https://acme.example', 'A']]);
  assert.deepEqual(page.calls.scopes.map(({link, ...seen}) => seen), [{scope: {kind: 'workspace', host: 'https://acme.example', workspace: 'A'}, entry: {base: 'https://tilo.example/', home}}]);
  assert.equal(typeof (await page.calls.scopes[0].link).format, 'function', 'and how its links read, once the runtime is in');
  assert.equal(page.calls.replaces.at(-1), 'https://tilo.example/w/A', 'and the page opens the workspace it got into');
});

test('a second workspace on the home’s host goes by no name or count of the home’s', async () => {
  const {listedAt} = await import('./workspaces.mjs');
  const home = {name: 'Lounge', host: {origin: 'https://lounge.example'}};
  const none = {getItem: () => null};
  assert.equal(listedAt([home], {kind: 'workspace', host: 'https://lounge.example', workspace: 'r1'}, 'r1', none), home);
  assert.equal(listedAt([home], {kind: 'workspace', host: 'https://lounge.example', workspace: 'second'}, null, none), null);
  assert.equal(listedAt([home], {kind: 'workspace', host: 'https://lounge.example', workspace: null}, null, none), home);
  assert.equal(listedAt([{...home, workspace: 'r1'}], {kind: 'workspace', host: 'https://lounge.example', workspace: 'r1'}, null, none).name, 'Lounge');
  assert.equal(listedAt([home], {kind: 'own'}, null, none), null);
});

/** The grammar's answer to an address with no route: `no-route`, or for one
 * that ends in `#/`, a route with an empty part (tilo-links, tested there). */
const noRoute = url => {
  throw url.endsWith('#/')
    ? Object.assign(new Error('This link has an empty part (two slashes in a row, or a trailing slash)'), {code: 'empty-segment'})
    : Object.assign(new Error('no route'), {code: 'no-route'});
};

test('an address that ends in #/ names no route, and goes where the bare site goes', () => {
  // What is left of a link whose route was taken out, and what people type:
  // `https://site/#/`. Read as a route, it stopped the page with "This link
  // has an empty part" where the bare site opens the person's workspace.
  const home = {name: 'Lounge', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
  const chosen = {origin: 'https://acme.example', workspace: 'A'};
  const link = {parse: noRoute};
  for (const hash of ['', '#', '#/']) {
    const location = {hash, href: `https://tilo.example/${hash}`};
    assert.deepEqual(chosenRoute({location, mode: 'desktop', home, chosen}),
      {mode: 'desktop', target: {scope: {kind: 'workspace', host: 'https://acme.example', workspace: 'A'}}}, `the workspace last chosen, at "${hash}"`);
    assert.deepEqual(initialRoute({link, location, mode: 'desktop', home}),
      {mode: 'desktop', target: {scope: {kind: 'workspace', host: 'https://workspaces.example', workspace: null}}}, `the site's home, at "${hash}"`);
    assert.deepEqual(initialRoute({link, location, mode: 'desktop'}),
      {mode: 'desktop', target: {scope: {kind: 'own'}}}, `the person's own desktop, at "${hash}"`);
    assert.throws(() => initialRoute({link, location: {hash, href: `https://tilo.example/app/${hash}`}, mode: 'app'}), {code: 'no-route'}, `an app page names its app, at "${hash}"`);
  }
  // A route with an empty part is still refused as one.
  const broken = {hash: '#/desktop//chat', href: 'https://tilo.example/#/desktop//chat'};
  const strict = {parse: () => { throw Object.assign(new Error('This link has an empty part'), {code: 'empty-segment'}); }};
  assert.equal(chosenRoute({location: broken, mode: 'desktop', home, chosen}), null);
  assert.throws(() => initialRoute({link: strict, location: broken, mode: 'desktop', home}), {code: 'empty-segment'});
});

test('a page opened at an address ending in #/ goes in through its home’s entry, as the bare site does', async () => {
  const home = {name: 'Commons', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/', app: 'so.tilo.chat'};
  const order = [];
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://workspaces.example/open/#/', hash: '#/', replace: url => order.push(url)},
    history: {state: null, replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => runtimeFake({...entering('entry'), parseLink: noRoute,
      formatLink: () => 'https://workspaces.example/open/#/@workspaces.example/w/r1'}),
    fetch: async () => ({ok: true, json: async () => ({home})}),
    enterViaHost: async () => { order.push('entry'); return {workspace: 'r1'}; },
  };
  void boot({mode: 'desktop', window, deps}).catch(error => order.push(error.message));
  await new Promise(resolve => setTimeout(resolve, 10));
  assert.deepEqual(order.slice(0, 2), ['entry', 'https://workspaces.example/open/w/r1/chat']);
});
