// As AdGuard does, a script goes into the page as soon as the tab says it
// shows a new address, before the page's declared content scripts have run;
// what it found is reported to the page's server.
globalThis.listening = true;
chrome.tabs.onUpdated.addListener((tabId, change) => {
  if (!change.url || !change.url.endsWith('/awaited')) return;
  const report = (found) => fetch(new URL('/report/awaiter', change.url), {method: 'POST', body: JSON.stringify(found)});
  chrome.scripting.executeScript({target: {tabId}, files: ['kept.js']})
    .then((results) => report(results[0].result), (error) => report({failed: error.message}));
});
