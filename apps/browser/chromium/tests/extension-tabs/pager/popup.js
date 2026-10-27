// The popup injects a script into the page it was opened over and answers
// it, by a message and over a port.
let opened;
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.ask) reply('page:' + (sender.tab.id === opened));
});
chrome.runtime.onConnect.addListener((port) => {
  port.onMessage.addListener((message) => port.postMessage('port:' + message));
});
(async () => {
  const [tab] = await chrome.tabs.query({active: true, currentWindow: true});
  opened = tab.id;
  await chrome.scripting.executeScript({target: {tabId: tab.id}, files: ['inject.js']});
})().catch((error) => { document.title = 'failed:' + error.message; });
