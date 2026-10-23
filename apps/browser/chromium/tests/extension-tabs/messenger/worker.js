// The worker and its content script talk both ways, by the tab's id.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (!message.hello) return;
  (async () => {
    const tabs = await chrome.tabs.query({});
    const pong = await chrome.tabs.sendMessage(sender.tab.id, {ping: true});
    reply({known: tabs.some((tab) => tab.id === sender.tab.id), pong});
  })();
  return true;
});
