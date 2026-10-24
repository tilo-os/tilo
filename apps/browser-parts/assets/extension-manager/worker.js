// Each tab's id among Chromium's, which the engine asks for so that
// extensions know a tab by one id from its first page on, a blank page, an
// extension's own or an internal one too.
//
// An extension's own page in a tab is not listed with its tab (below): the
// engine has it send a word it made up, and the tab its message comes from
// is kept under that word, for an extension's page alone.
const told = new Map();
chrome.runtime.onMessageExternal.addListener((message, sender, reply) => {
  if (!message || typeof message.tiloTab !== 'string' || !sender.tab || told.size >= 256) return;
  if (!sender.url || !sender.url.startsWith(`chrome-extension://${sender.id}/`)) return;
  told.set(message.tiloTab, sender.tab.id);
  reply(true);
});
// Every page's id by its DevTools target, which Chromium lists to an
// extension that may debug; listing attaches to none. Then the tabs kept
// under the words `words`, each answered once.
globalThis.__tiloTabIds = async (words = []) => {
  const ids = {};
  for (const target of await chrome.debugger.getTargets()) {
    if (target.type === 'page' && Number.isInteger(target.tabId) && target.tabId > 0) ids[target.id] = target.tabId;
  }
  for (const word of words) {
    if (told.has(word)) ids[word] = told.get(word);
    told.delete(word);
  }
  return ids;
};

// Requests extensions' network rules blocked, or redirected to an extension's
// own file, in the Browser's tabs. Chromium counts them for no tab here, so it
// shows no count on an ad blocker's button; the engine counts them for the
// page (extension_tabs.h), told by the token the Browser wrote beside this
// worker. Many at once go in one report.
try {
  importScripts('tilo-bridge-token.js');
} catch (missing) {
  // A Browser that wrote no token hears of no requests.
}
const token = globalThis.__tiloBridgeToken || null;
delete globalThis.__tiloBridgeToken;
let acted = [];
let reporting = 0;
const report = () => {
  clearTimeout(reporting);
  reporting = 0;
  const requests = acted;
  acted = [];
  if (!requests.length) return;
  fetch('https://extension-bridge.tilo.invalid/requests.acted', {
    method: 'POST',
    headers: {'Content-Type': 'text/plain'},
    body: JSON.stringify({token, args: {requests}}),
    credentials: 'omit',
    cache: 'no-store',
  }).catch(() => {});
};
const note = (details, extension) => {
  // A page's own document starts it anew; requests of no tab count for none.
  if (!token || details.tabId < 0 || details.type === 'main_frame') return;
  const {tabId, url, type, method, initiator, timeStamp} = details;
  acted.push({tabId, url, type, method: method.toLowerCase(), initiator: initiator === 'null' ? undefined : initiator,
    timeStamp, extension});
  if (acted.length >= 256) report();
  else if (!reporting) reporting = setTimeout(report, 100);
};
const all = {urls: ['<all_urls>']};
chrome.webRequest.onErrorOccurred.addListener((details) => {
  if (details.error === 'net::ERR_BLOCKED_BY_CLIENT') note(details, undefined);
}, all);
// A redirect to an extension's own file names the extension, unless the file
// has an address of its own for the session (use_dynamic_url), as uBlock
// Origin Lite's harmless stand-ins for ad scripts have.
chrome.webRequest.onBeforeRedirect.addListener((details) => {
  const own = /^chrome-extension:\/\/([^/]+)\//.exec(details.redirectUrl);
  if (own) note(details, /^[a-p]{32}$/.test(own[1]) ? own[1] : undefined);
}, all);
