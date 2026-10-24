// A page that finishes loading is one the worker hears of, and may script.
// What it heard and how scripting went stay readable, for when it did not.
globalThis.heard = [];
chrome.tabs.onUpdated.addListener((tabId, change, tab) => {
  heard.push({tabId, change, url: tab.url});
  if (change.status !== 'complete' || !tab.url || !tab.url.endsWith('/watched')) return;
  chrome.scripting.executeScript({
    target: {tabId},
    func: (id) => { document.title = 'watched:' + id; },
    args: [tabId],
  }).then(() => heard.push('scripted'), (error) => heard.push('failed: ' + error.message));
});
