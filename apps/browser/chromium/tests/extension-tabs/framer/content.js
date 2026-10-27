// In every frame, as a password manager's: it asks its worker to inject the
// script that finds the frame's fields, into this frame alone.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.where) reply(location.pathname);
});
chrome.runtime.sendMessage({hello: location.pathname});
