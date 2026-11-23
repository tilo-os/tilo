// Copies what the worker sends, as MV3 extensions copy: a worker has no
// clipboard, and an offscreen document selects the words and copies them.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.target !== 'offscreen') return;
  const text = document.getElementById('text');
  text.value = message.text;
  text.select();
  reply({copied: document.execCommand('copy')});
});
