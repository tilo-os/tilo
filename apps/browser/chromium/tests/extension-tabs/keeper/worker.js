// A page's tab, by the id Chromium gives its messages, against the ids the
// extension kept from the tab's earlier pages.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (!message.kept) return;
  (async () => {
    const kept = await chrome.storage.local.get(['welcome', 'blank']);
    const found = await chrome.tabs.get(sender.tab.id).then((tab) => tab.url, (error) => 'missing:' + error.message);
    const listed = (await chrome.tabs.query({})).filter((tab) => tab.id === sender.tab.id).length;
    reply({welcome: kept.welcome === sender.tab.id, blank: kept.blank === sender.tab.id, found, listed});
  })();
  return true;
});
