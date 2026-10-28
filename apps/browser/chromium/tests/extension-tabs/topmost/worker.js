// A worker that sends the whole tab a message, naming no frame, once its
// content script in the main frame says the page loaded.
chrome.runtime.onMessage.addListener((message, sender) => {
  if (!message.hello || !sender.tab) return;
  chrome.tabs.sendMessage(sender.tab.id, {kind: 'hold'})
    .then((answer) => answer, (error) => 'error: ' + error.message)
    .then((answer) => fetch(new URL('/report/topmost', sender.url), {method: 'POST', body: JSON.stringify({answer})}));
});
