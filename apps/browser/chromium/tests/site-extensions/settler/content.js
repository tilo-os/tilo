// Follows the extension's settings in the page: what each change says, and
// the worker's answers, asked with an explicit null or undefined extension
// id, and the name of a port opened with a null one.
if (!globalThis.settlerRuns) {
  globalThis.settlerRuns = true;
  const seen = {};
  const show = () => { document.title = 'settler:' + JSON.stringify(seen); };
  chrome.runtime.sendMessage(null, {hello: location.pathname}, (reply) => { seen.reply = reply ?? 'no answer'; show(); });
  chrome.runtime.sendMessage(undefined, {hello: 'again'}, (reply) => { seen.again = reply ?? 'no answer'; show(); });
  chrome.runtime.connect(null, {name: 'settings'}).onMessage.addListener((message) => { seen.port = message.name; show(); });
  chrome.storage.onChanged.addListener((changes, area) => {
    if (changes.mode) { seen.area = area; seen.mode = changes.mode.newValue; show(); }
  });
  chrome.storage.local.onChanged.addListener((changes) => {
    if (changes.mode) { seen.local = changes.mode.newValue; show(); }
  });
}
