// Injected into every frame the extension may script, where it has no
// content script of its own: it hears what the worker sends the whole tab,
// tells the worker it did, and answers.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.kind !== 'everyone') return;
  chrome.runtime.sendMessage({heard: location.pathname});
  reply(location.pathname);
});
location.pathname;
