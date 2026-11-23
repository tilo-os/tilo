// A password manager's popup, as 1Password's and LastPass's: its policy lets
// it connect only to its own servers. It finds the page it was opened over,
// asks its worker which window is the last focused one, and learns why a
// call failed from runtime.lastError, as Chrome tells a callback.
const failure = (call) => new Promise((resolve) => call((value) => {
  const error = chrome.runtime.lastError;
  resolve({value: value === undefined, error: error ? error.message : null});
}));
(async () => {
  const [tab] = await chrome.tabs.query({active: true, currentWindow: true});
  const worker = await chrome.runtime.sendMessage({ask: true});
  const missing = await failure((done) => chrome.tabs.get(2000000000, done));
  document.title = 'vault:' + JSON.stringify({
    url: tab ? tab.url : null,
    worker,
    missing,
    cleared: chrome.runtime.lastError === undefined,
  });
})().catch((error) => { document.title = 'failed:' + error.message; });
