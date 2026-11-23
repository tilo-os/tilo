// The worker and its content script talk both ways, by the tab's id: by
// messages and over a port the worker opens.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (!message.hello) return;
  (async () => {
    const tabs = await chrome.tabs.query({});
    const pong = await chrome.tabs.sendMessage(sender.tab.id, {ping: true});
    const ported = await new Promise((resolve) => {
      const port = chrome.tabs.connect(sender.tab.id, {name: 'declared'});
      port.onMessage.addListener((answer) => {
        port.disconnect();
        resolve(answer);
      });
      port.onDisconnect.addListener(() => resolve('closed'));
      port.postMessage('ping');
    });
    reply({known: tabs.some((tab) => tab.id === sender.tab.id), pong, ported});
  })();
  return true;
});
