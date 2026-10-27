// Parses what the worker sends, and asks the worker for the page in front.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.target !== 'offscreen') return;
  const parsed = new DOMParser().parseFromString(message.html, 'text/html');
  chrome.runtime.sendMessage({target: 'worker'}).then((front) => {
    reply({title: parsed.title, apis: Object.keys(chrome).filter((name) => ['tabs', 'runtime', 'offscreen'].includes(name)).sort(), front});
  });
  return true;
});
