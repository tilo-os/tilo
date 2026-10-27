// A click injects a script that sends many messages at once over a port:
// each reaches the worker, in order, and the port stays open.
const COUNT = 300;
chrome.action.onClicked.addListener((tab) => {
  chrome.scripting.executeScript({target: {tabId: tab.id}, files: ['inject.js']});
});
chrome.runtime.onConnect.addListener((port) => {
  const got = [];
  port.onMessage.addListener((message) => {
    got.push(message);
    if (got.length === COUNT) port.postMessage({count: got.length, ordered: got.every((value, at) => value === at)});
  });
});
