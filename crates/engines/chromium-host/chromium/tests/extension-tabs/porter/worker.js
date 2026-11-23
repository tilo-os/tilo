// A click injects a script where the extension has no world of its own; it
// and the worker talk over ports both ways, and it hears of storage changes.
chrome.action.onClicked.addListener(async (tab) => {
  await chrome.storage.local.set({color: 'green'});
  await chrome.scripting.executeScript({target: {tabId: tab.id}, files: ['inject.js']});
});
chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== 'from-page') return;
  const tabId = port.sender.tab.id;
  port.onMessage.addListener(async (message) => {
    if (!message.hello) return;
    // The script listens for no message: as in Chrome, sending one fails.
    let unanswered = false;
    try {
      await chrome.tabs.sendMessage(tabId, {nobody: true});
    } catch (error) {
      unanswered = /Receiving end does not exist/.test(error.message);
    }
    const back = chrome.tabs.connect(tabId, {name: 'from-worker'});
    back.onMessage.addListener(async (reply) => {
      back.disconnect();
      port.postMessage({echo: message.hello, pong: reply.pong, unanswered});
      await chrome.storage.local.set({color: 'blue'});
      port.disconnect();
    });
    back.postMessage({ping: 'ping'});
  });
});
