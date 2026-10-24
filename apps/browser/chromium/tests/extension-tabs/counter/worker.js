// A button that shows a count for each page, as an ad blocker shows what it
// blocked there, set for that page's tab alone; and its own popup, opened
// for one page that asks.
chrome.runtime.onMessage.addListener((message, sender) => {
  const tabId = sender.tab.id;
  const show = (text) => chrome.scripting.executeScript({
    target: {tabId},
    func: (text) => { document.title = text; },
    args: [text],
  });
  (async () => {
    if (message.path === '/open') {
      await chrome.action.setPopup({tabId, popup: 'popup.html'});
      await chrome.action.openPopup();
      return;
    }
    const count = String(message.path.length);
    await chrome.action.setBadgeText({tabId, text: count});
    await chrome.action.setTitle({tabId, title: `${count} blocked`});
    await chrome.action.setIcon({tabId, path: 'on.png'});
    // The callback form, as older extensions call it.
    await new Promise((resolve) => chrome.action.setBadgeBackgroundColor({tabId, color: [0, 255, 0, 255]}, resolve));
    await chrome.action.setBadgeText({text: 'all'});
    await chrome.action.setIcon({imageData: new ImageData(new Uint8ClampedArray([1, 2, 3, 4, 5, 6, 7, 8]), 2, 1)});
    if (message.path === '/off') await chrome.action.disable(tabId);
    // Chromium answers for the tab what the extension set for it.
    await show('badged:' + JSON.stringify({
      text: await chrome.action.getBadgeText({tabId}),
      every: await chrome.action.getBadgeText({}),
      title: await chrome.action.getTitle({tabId}),
      enabled: await chrome.action.isEnabled(tabId),
    }));
  })().catch((error) => show('failed:' + error.message));
});
