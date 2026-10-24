// A page's id among Chromium's tabs, which only a message from the page tells.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message && message.tiloTabId === true && sender.tab) reply(sender.tab.id);
});
