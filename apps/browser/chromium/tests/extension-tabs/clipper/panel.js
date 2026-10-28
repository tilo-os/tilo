// The panel answers every message at once, as Evernote's does (its replies
// come later, in messages of their own), and says it is open.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  reply(message.ping ? {panel: location.pathname, from: sender.id} : {});
});
chrome.runtime.sendMessage({hello: location.pathname});
