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
