// In every frame, as a translator's or a password manager's: it hears what
// the worker sends the whole tab, tells the worker it did, and answers as
// the message asks.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (!message.kind) return undefined;
  chrome.runtime.sendMessage({heard: message.kind, at: location.pathname});
  if (message.kind === 'everyone') reply(location.pathname);
  // Only the frame of the other site answers this one, and later.
  if (message.kind === 'later' && location.pathname === '/outer') {
    setTimeout(() => reply(location.pathname), 200);
    return true;
  }
  return undefined;
});
chrome.runtime.sendMessage({hello: location.pathname});
