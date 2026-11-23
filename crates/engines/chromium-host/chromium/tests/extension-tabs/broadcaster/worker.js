// A worker that sends the whole tab a message, naming no frame, as a
// translator does: the content script in every frame hears it, in the
// page's site and in another site's frame, and one reply answers it.
const frames = ['/framed', '/inner', '/outer'];
const hello = new Map();
const heard = new Map();
const settled = (promise) => promise.then((value) => (value === undefined ? 'nothing' : value), (error) => 'error: ' + error.message);
// The frames that said they heard `kind`, once all did or five seconds went.
const hearing = (kind) => new Promise((resolve) => {
  const deadline = Date.now() + 5000;
  const look = () => {
    const at = heard.get(kind) || [];
    if (frames.every((path) => at.includes(path)) || Date.now() > deadline) resolve([...at].sort());
    else setTimeout(look, 50);
  };
  look();
});
chrome.runtime.onMessage.addListener((message, sender) => {
  if (message.heard) {
    heard.set(message.heard, [...(heard.get(message.heard) || []), message.at]);
    return;
  }
  if (!message.hello || !sender.tab) return;
  hello.set(message.hello, {frameId: sender.frameId, documentId: sender.documentId, url: sender.url});
  if (!frames.every((path) => hello.has(path))) return;
  const tabId = sender.tab.id;
  (async () => {
    const everyone = await settled(chrome.tabs.sendMessage(tabId, {kind: 'everyone'}));
    const report = {
      everyone: frames.includes(everyone) ? 'a frame' : everyone,
      heardEveryone: await hearing('everyone'),
      quiet: await settled(chrome.tabs.sendMessage(tabId, {kind: 'quiet'})),
      heardQuiet: await hearing('quiet'),
      later: await settled(chrome.tabs.sendMessage(tabId, {kind: 'later'})),
      // Each frame's result names the frame and its document as Chromium
      // names them to the frame's content script.
      results: await settled(chrome.scripting.executeScript({target: {tabId, allFrames: true}, func: () => location.pathname})
        .then((entries) => entries.map((entry) => [
          entry.result,
          entry.frameId === hello.get(entry.result).frameId,
          entry.documentId === hello.get(entry.result).documentId,
        ]).sort())),
    };
    await fetch(new URL('/report/broadcaster', hello.get('/framed').url), {method: 'POST', body: JSON.stringify(report)});
  })();
});
