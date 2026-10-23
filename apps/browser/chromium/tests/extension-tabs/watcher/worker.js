// A page that finishes loading is one the worker hears of, and may script.
chrome.tabs.onUpdated.addListener((tabId, change, tab) => {
  if (change.status !== 'complete' || !tab.url || !tab.url.endsWith('/watched')) return;
  chrome.scripting.executeScript({
    target: {tabId},
    func: (id) => { document.title = 'watched:' + id; },
    args: [tabId],
  });
});
