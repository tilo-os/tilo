'use strict';

// Synthetic input generated exclusively by the admitted official Electron APIs.
// The supervising coordinator supplies runtime admission, private Secret Service,
// process retirement and the later independent production-reader observations.
const fs = require('node:fs');
const path = require('node:path');
const { app, safeStorage, session, WebContentsView } = require('electron');
const VERSION = '44.4.5';
const COOKIE_NAME = '__Host-tilo-source-fixture';
const COOKIE_VALUE = 'synthetic-electron-source-cookie';
const RAW_TOKEN = JSON.stringify('synthetic-electron-source-token');
const DOCUMENT = 'https://discord.com/.tilo-source-fixture';
let stage = 'runtime-admission';
let view;
let quitting = false;
let emitted = false;
let result;

function requireCondition(value) {
  if (!value) throw new Error('fixture-refused');
}

// Cooperative namespace validation only; the coordinator retains directory
// authority and rules out concurrent mutation by this fixture's other writers.
function privateDirectory(filename) {
  const info = fs.lstatSync(filename);
  requireCondition(info.isDirectory() && !info.isSymbolicLink());
  requireCondition(info.uid === process.getuid() && (info.mode & 0o777) === 0o700);
  requireCondition(fs.realpathSync(filename) === filename);
}

function freshDirectory(filename) {
  fs.mkdirSync(filename, { mode: 0o700 }); // EEXIST is a refusal, never a reuse.
  privateDirectory(filename);
  requireCondition(fs.readdirSync(filename).length === 0);
}

function finish(status) {
  if (quitting) return;
  quitting = true;
  clearTimeout(watchdog);
  process.exitCode = status;
  if (view && !view.webContents.isDestroyed()) view.webContents.close({ waitForBeforeUnload: false });
  // app.quit drives Chromium's normal shutdown. Success is not established
  // until the coordinator observes this entire owned process tree terminate.
  app.quit();
}

function fail() {
  if (!emitted) {
    emitted = true;
    fs.writeSync(1, JSON.stringify({ schema: 1, status: 'refused', error: stage }) + '\n');
  }
  finish(1);
}

const watchdog = setTimeout(() => {
  stage = 'generator-timeout';
  fail();
}, 45000);
process.on('uncaughtException', fail);
process.on('unhandledRejection', fail);
app.on('window-all-closed', () => {}); // No native window is ever constructed.
app.on('will-quit', () => {
  if (result && !emitted) {
    emitted = true;
    fs.writeSync(1, JSON.stringify(result) + '\n');
  }
});

let slackPath;
let discordPath;
try {
  requireCondition(process.platform === 'linux' && process.versions.electron === VERSION);
  requireCondition(!app.commandLine.hasSwitch('no-sandbox'));
  requireCondition(!app.commandLine.hasSwitch('disable-setuid-sandbox'));
  requireCondition(app.commandLine.getSwitchValue('ozone-platform') === 'headless');
  requireCondition(!app.commandLine.hasSwitch('password-store') ||
    app.commandLine.getSwitchValue('password-store') === 'gnome-libsecret');
  requireCondition(typeof process.env.DBUS_SESSION_BUS_ADDRESS === 'string' &&
    process.env.DBUS_SESSION_BUS_ADDRESS.length > 0);
  const home = process.env.HOME;
  requireCondition(typeof home === 'string' && path.isAbsolute(home) && path.normalize(home) === home);
  privateDirectory(home);
  const source = path.join(home, 'source-format');
  privateDirectory(source);
  slackPath = path.join(source, 'slack');
  discordPath = path.join(source, 'discord');
  freshDirectory(slackPath);
  freshDirectory(discordPath);
  const runtime = path.join(home, 'electron-runtime-state');
  freshDirectory(runtime);
  app.setName('Slack');
  app.setPath('userData', runtime);
  app.setPath('sessionData', runtime);
  app.commandLine.appendSwitch('password-store', 'gnome-libsecret');
  requireCondition(app.getName() === 'Slack');
} catch {
  fail();
}

app.whenReady().then(async () => {
  if (quitting) return;
  stage = 'secret-service-backend';
  requireCondition(app.getName() === 'Slack');
  requireCondition(safeStorage.isEncryptionAvailable());
  requireCondition(safeStorage.getSelectedStorageBackend() === 'gnome_libsecret');
  const probe = safeStorage.encryptString('synthetic-electron-encryption-probe');
  try {
    requireCondition(safeStorage.decryptString(probe) === 'synthetic-electron-encryption-probe');
    requireCondition(safeStorage.getSelectedStorageBackend() === 'gnome_libsecret');
  } finally {
    probe.fill(0);
  }

  stage = 'slack-cookie-generation';
  const slack = session.fromPath(slackPath, { cache: false });
  // No Domain attribute: this is a genuine host-only __Host- cookie.
  // One day avoids Chromium's 400-day lifetime clamp.
  const expirationDate = Math.floor(Date.now() / 1000) + 86400;
  await slack.cookies.set({
    url: 'https://app.slack.com/', name: COOKIE_NAME, value: COOKIE_VALUE,
    path: '/', secure: true, httpOnly: true, sameSite: 'lax', expirationDate,
  });
  await slack.cookies.flushStore();
  const cookies = await slack.cookies.get({});
  requireCondition(cookies.length === 1);
  const cookie = cookies[0];
  requireCondition(cookie.name === COOKIE_NAME && cookie.value === COOKIE_VALUE &&
    cookie.domain === 'app.slack.com' && cookie.hostOnly === true && cookie.path === '/' &&
    cookie.secure === true && cookie.httpOnly === true && cookie.sameSite === 'lax' &&
    cookie.session === false && Math.abs(cookie.expirationDate - expirationDate) < 1);

  stage = 'discord-origin-generation';
  const discord = session.fromPath(discordPath, { cache: false });
  // Handle the document locally; do not forward a request to Discord.
  discord.protocol.handle('https', request => {
    if (request.url !== DOCUMENT || request.method !== 'GET') {
      return new Response('', { status: 403 });
    }
    return new Response('<!doctype html><meta charset="utf-8"><title>Fixture</title>', {
      status: 200,
      headers: {
        'Content-Type': 'text/html; charset=utf-8',
        'Content-Security-Policy': "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        'Cache-Control': 'no-store',
      },
    });
  });
  discord.webRequest.onBeforeRequest({ urls: ['<all_urls>'] }, (details, callback) => {
    callback({ cancel: details.url !== DOCUMENT });
  });
  view = new WebContentsView({ webPreferences: {
    session: discord, sandbox: true, contextIsolation: true,
    nodeIntegration: false, webSecurity: true, spellcheck: false,
    backgroundThrottling: false,
  } });
  view.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  await view.webContents.loadURL(DOCUMENT);
  requireCondition(view.webContents.getURL() === DOCUMENT);
  const written = await view.webContents.executeJavaScript(
    `(() => { if (location.origin !== 'https://discord.com') return false; ` +
    `localStorage.setItem('token', ${JSON.stringify(RAW_TOKEN)}); ` +
    `return localStorage.length === 1 && localStorage.getItem('token') === ${JSON.stringify(RAW_TOKEN)}; })()`,
  );
  requireCondition(written === true);
  requireCondition((await discord.cookies.get({})).length === 0);
  discord.flushStorageData(); // Official API has no completion promise.
  await new Promise(resolve => {
    view.webContents.once('destroyed', resolve);
    view.webContents.close({ waitForBeforeUnload: false });
  });
  view = undefined;
  discord.flushStorageData();
  await slack.cookies.flushStore();
  requireCondition(safeStorage.getSelectedStorageBackend() === 'gnome_libsecret');
  result = {
    schema: 1, status: 'generated', electron: VERSION, app_name: 'Slack',
    storage_backend: 'gnome_libsecret', slack_cookie_count: 1,
    discord_cookie_count: 0, discord_origin_storage_count: 1,
    authenticated: false,
  };
  stage = 'graceful-shutdown';
  finish(0);
}).catch(fail);
