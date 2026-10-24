// Follows the extension's settings in the page: what each change says, and
// a greeting from the worker, asked with an explicit null extension id.
if (!globalThis.settlerRuns) {
  globalThis.settlerRuns = true;
  const seen = {};
  const show = () => { document.title = 'settler:' + JSON.stringify(seen); };
  chrome.runtime.sendMessage(null, {hello: location.pathname}, (reply) => { seen.reply = reply; show(); });
  chrome.storage.onChanged.addListener((changes, area) => {
    if (changes.mode) { seen.area = area; seen.mode = changes.mode.newValue; show(); }
  });
  chrome.storage.local.onChanged.addListener((changes) => {
    if (changes.mode) { seen.local = changes.mode.newValue; show(); }
  });
}
