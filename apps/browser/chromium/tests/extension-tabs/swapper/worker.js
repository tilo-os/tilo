// The address of the tab a page's message comes from, as the tab APIs find
// it by the id Chromium gives the message.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (!message.where) return;
  chrome.tabs.get(sender.tab.id).then((tab) => reply(tab.url), (error) => reply('missing:' + error.message));
  return true;
});
