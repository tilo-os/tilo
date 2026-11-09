/**
 * The bar over one app full tab (`app/index.html`): where the thing is (its
 * workspace and its app), who is here, Share, and "Open in Tilo" to the
 * whole desktop. Plain DOM over the app's canvas: the page draws it, so a
 * shared link opens without the desktop.
 *
 * Who is here is the workspace's presence board (`peer.live`), the same
 * words every desktop draws its people from: each person once, those in
 * this document first. Share hands on the link to what is in front, as the
 * router names it: as it stands it opens for the workspace's people; with
 * an invitation of the person's own (`peer.createInvitation`, which their
 * role allows or the host refuses) it lets in anyone who has it, as a
 * viewer or an editor. Every refusal is the host's, said in its words.
 */

/** How long a link for anyone works, as the page's other invitations. */
export const ANYONE_LINK_SECONDS = 7 * 86_400;
/** Faces shown before the rest are counted. */
export const MAX_FACES = 3;

/**
 * Everyone here as the bar shows them: one entry per person (by their
 * user root, else their device), not this page's own person, those whose
 * focus is `here` (`{app, record}`, the record as its 16-hex digest) first,
 * then by name. `{key, label, colour, agent, here}`.
 */
export function peopleHere(members, {self = null, here = null} = {}) {
  const people = new Map();
  for (const member of members ?? []) {
    const key = member.user ?? member.publisher;
    if (!key || key === self) continue;
    const inHere = Boolean(here && member.focus && member.focus.app === here.app && member.focus.record === here.record);
    const known = people.get(key);
    if (known && (known.here || !inHere)) continue;
    people.set(key, {key, label: member.label || 'Someone', colour: member.colour ?? null,
      agent: Boolean(member.agent), here: inHere});
  }
  return [...people.values()].sort((a, b) => Number(b.here) - Number(a.here) || a.label.localeCompare(b.label));
}

/**
 * Where "Open in Tilo" takes one app's `target`: the same place on the
 * desktop. The desktop opens an app on its workspace document only, so a
 * document of its own opens as its app there.
 */
export function desktopRoute(target) {
  if (!target?.record) return {mode: 'desktop', target};
  const {scope, app} = target;
  return {mode: 'desktop', target: {scope, app}};
}

/** Whether `value` is a colour as the mount names one (`#rrggbb`). */
const isHex = value => typeof value === 'string' && /^#[0-9a-f]{6}$/i.test(value);

/**
 * Draw the bar in the colours the app is drawn in (the mount's `pageTheme`:
 * `{dark, ground, ink, quiet, line}`), so bar and app read as one surface
 * whatever theme the person chose rather than whatever the browser
 * prefers. Fills are the ink laid thinly over the ground; the primary
 * button is the ink itself. Returns whether it applied.
 */
export function applyTheme(style, theme) {
  const {ground, ink, quiet, line} = theme ?? {};
  if (![ground, ink, quiet, line].every(isHex)) return false;
  const mix = percent => `color-mix(in srgb, ${ink} ${percent}%, ${ground})`;
  for (const [name, value] of Object.entries({
    '--bar-bg': ground, '--bar-text': ink, '--bar-quiet': quiet, '--bar-line': line,
    '--bar-button': mix(9), '--bar-button-hover': mix(15),
    '--bar-primary': ink, '--bar-primary-text': ground, '--bar-sheet': ground,
  })) style.setProperty(name, value);
  style.setProperty('color-scheme', theme.dark ? 'dark' : 'light');
  return true;
}

/** A name's initial, as a face shows it. */
export const initial = label => ([...String(label ?? '').trim()][0] ?? '?').toUpperCase();

/** Black or white, whichever reads on `background` (`#rrggbb`). */
function readableOn(background) {
  const [r, g, b] = [1, 3, 5].map(at => Number.parseInt(background.slice(at, at + 2), 16) / 255)
    .map(c => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.4 ? '#121214' : '#ffffff';
}

/** Draw `people` into `list`: up to `MAX_FACES` faces, then "+N". */
export function drawPeople(list, people, document = globalThis.document) {
  list.replaceChildren();
  const shown = people.slice(0, MAX_FACES);
  for (const person of shown) {
    const face = document.createElement('span');
    face.className = `bar-face${person.agent ? ' bar-agent' : ''}${person.here ? '' : ' bar-elsewhere'}`;
    face.textContent = initial(person.label);
    face.title = person.here ? person.label : `${person.label} (elsewhere in the workspace)`;
    face.setAttribute('role', 'listitem');
    face.setAttribute('aria-label', face.title);
    if (typeof person.colour === 'string' && /^#[0-9a-f]{6}$/i.test(person.colour)) {
      face.style.background = person.colour;
      face.style.color = readableOn(person.colour);
    }
    list.append(face);
  }
  if (people.length > shown.length) {
    const more = document.createElement('span');
    more.className = 'bar-face bar-more';
    more.textContent = `+${people.length - shown.length}`;
    more.setAttribute('role', 'listitem');
    more.setAttribute('aria-label', `${people.length - shown.length} more`);
    list.append(more);
  }
  list.hidden = people.length === 0;
}

/** Put `text` on the clipboard; resolves whether it got there. */
async function copied(text, clipboard = globalThis.navigator?.clipboard) {
  try {
    await clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/**
 * The Share sheet under the bar's Share button: the link as it stands for
 * the workspace's people, and a link for anyone, made on asking. Resolves
 * when it closes.
 */
export function shareSheet({anchor, workspaceName, link, anyoneLink, document = globalThis.document}) {
  return new Promise(resolve => {
    const sheet = document.createElement('div');
    sheet.className = 'bar-sheet';
    sheet.setAttribute('role', 'dialog');
    sheet.setAttribute('aria-label', 'Share');
    const line = (text, className) => Object.assign(document.createElement('p'), {textContent: text, className});
    const button = (text, className = '') => Object.assign(document.createElement('button'), {type: 'button', textContent: text, className});

    const members = document.createElement('div');
    members.className = 'bar-sheet-row';
    const membersCopy = button('Copy link', 'bar-primary');
    members.append(line(workspaceName ? `People in ${workspaceName} can open this link.` : 'People in this workspace can open this link.', 'bar-sheet-text'), membersCopy);
    membersCopy.addEventListener('click', async () => {
      membersCopy.textContent = await copied(link()) ? 'Copied' : 'Copy failed';
    });

    const anyone = document.createElement('div');
    anyone.className = 'bar-sheet-row';
    const role = document.createElement('select');
    role.setAttribute('aria-label', 'What anyone with the link can do');
    for (const [value, text] of [['viewer', 'can view'], ['editor', 'can edit']]) {
      role.append(Object.assign(document.createElement('option'), {value, textContent: text}));
    }
    const lead = line('Anyone with the link ', 'bar-sheet-text');
    lead.append(role);
    const make = button('Copy link');
    const said = line('', 'bar-sheet-note');
    said.setAttribute('role', 'status');
    make.addEventListener('click', async () => {
      make.disabled = true;
      said.textContent = 'Making a link…';
      try {
        const url = await anyoneLink(role.value);
        said.textContent = await copied(url)
          ? `Copied. It lets anyone in for ${ANYONE_LINK_SECONDS / 86_400} days.`
          : 'Select the link and copy it.';
        if (!said.textContent.startsWith('Copied')) {
          const field = Object.assign(document.createElement('input'), {value: url, readOnly: true});
          field.setAttribute('aria-label', 'Link');
          said.after(field);
          field.select();
        }
      } catch (error) {
        said.textContent = error?.message ?? String(error);
      } finally {
        make.disabled = false;
      }
    });
    anyone.append(lead, make);

    sheet.append(line('Share', 'bar-sheet-title'), members, anyone, said);
    const close = () => {
      sheet.remove();
      document.removeEventListener('pointerdown', outside, true);
      document.removeEventListener('keydown', escape, true);
      anchor.setAttribute('aria-expanded', 'false');
      anchor.focus();
      resolve();
    };
    const outside = event => { if (!sheet.contains(event.target) && event.target !== anchor) close(); };
    const escape = event => { if (event.key === 'Escape') { event.preventDefault(); close(); } };
    document.addEventListener('pointerdown', outside, true);
    document.addEventListener('keydown', escape, true);
    anchor.setAttribute('aria-expanded', 'true');
    document.body.append(sheet);
    membersCopy.focus();
  });
}

/**
 * Start the bar once the page is in. `page` is what `boot` resolved to
 * (`peer`, `router`, `link`, `mount`); `appName(id)` names an app;
 * `workspaceName()` the workspace as the page knows it, or nothing.
 * Returns `stop()`.
 */
export function startAppBar({page, appName = id => id, workspaceName = () => '', document = globalThis.document,
  setTimer = globalThis.setTimeout, clearTimer = globalThis.clearTimeout}) {
  const root = document.documentElement;
  const where = document.getElementById('bar-workspace');
  const app = document.getElementById('bar-app');
  const list = document.getElementById('bar-people');
  const share = document.getElementById('bar-share');
  const target = () => page.router.route.target;
  let stopped = false;
  let timer;
  let theme = null;

  const named = () => {
    const name = workspaceName() || page.peer?.workspaceName || '';
    if (where) where.textContent = name;
    if (app) app.textContent = target().app ? appName(target().app) : '';
    root.dataset.bar = 'shown';
  };
  const refresh = () => {
    // The mount names the document it shows as its presence does (its
    // app and record digest), so the bar matches members against that.
    const state = page.mount?.state?.();
    const here = state?.livePlace?.focus ?? null;
    if (state?.pageTheme && state.pageTheme !== theme && applyTheme(root.style, state.pageTheme)) {
      theme = state.pageTheme;
      root.dataset.barTheme = theme.dark ? 'dark' : 'light';
    }
    if (list) drawPeople(list, peopleHere(page.peer?.live?.presenceMembers?.() ?? [], {self: page.peer?.userPublicKey, here}), document);
    named();
  };
  const tick = () => {
    timer = undefined;
    if (stopped) return;
    try { refresh(); } finally { timer = setTimer(tick, 1000); }
  };
  tick();

  if (share) {
    // Sharing needs a workspace to share in: the person's own desktop has
    // no people to hand a link to.
    share.hidden = !page.peer?.workspace;
    share.addEventListener('click', () => {
      if (share.getAttribute('aria-expanded') === 'true') return;
      void shareSheet({
        anchor: share, document,
        workspaceName: workspaceName() || page.peer?.workspaceName || '',
        link: () => page.router.url('app'),
        anyoneLink: async role => {
          const invitation = await page.peer.createInvitation({role, expiresIn: ANYONE_LINK_SECONDS});
          return page.link.invite({mode: 'app', target: target()}, invitation);
        },
      });
    });
  }
  return () => {
    stopped = true;
    if (timer !== undefined) clearTimer(timer);
  };
}
