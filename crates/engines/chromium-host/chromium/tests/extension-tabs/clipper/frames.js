// In every frame: says which it is, and answers where it is.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.where) reply(location.pathname);
});
chrome.runtime.sendMessage({hello: location.pathname});
