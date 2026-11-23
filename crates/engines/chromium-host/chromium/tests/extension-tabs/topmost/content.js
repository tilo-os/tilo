// In the main frame alone, as a content script runs unless it asks for every
// frame: it holds a message to the whole tab a while before it answers, and
// says when it has it.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.kind !== 'hold') return undefined;
  fetch(new URL('/report/holding', location.href), {method: 'POST', body: '{}'});
  setTimeout(() => reply(location.pathname), 3000);
  return true;
});
// Once the page's frames have loaded.
const hello = () => chrome.runtime.sendMessage({hello: location.pathname});
if (document.readyState === 'complete') hello();
else addEventListener('load', hello);
