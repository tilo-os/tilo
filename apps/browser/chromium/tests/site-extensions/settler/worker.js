// As High Contrast, dark modes and readers do: once installed, it runs its
// content script in the pages already open, which Chrome gives none.
chrome.runtime.onInstalled.addListener(async () => {
  for (const window of await chrome.windows.getAll({populate: true})) {
    for (const tab of window.tabs || []) {
      if (!tab.url || !tab.url.startsWith('http')) continue;
      await chrome.scripting.executeScript({
        target: {tabId: tab.id, allFrames: true},
        files: ['content.js'],
        injectImmediately: true,
      });
    }
  }
});
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message && message.hello) reply('hello ' + message.hello);
});
chrome.runtime.onConnect.addListener((port) => port.postMessage({name: port.name}));
