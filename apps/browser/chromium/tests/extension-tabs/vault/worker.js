// The worker of a password manager whose policy lets it connect only to its
// own servers. Asked before the person focused any page, it still has a last
// focused window, as in Chrome; a failed call's callback sees lastError.
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (!message.ask) return;
  (async () => {
    const window = await chrome.windows.getLastFocused({populate: true});
    const missing = await new Promise((resolve) => chrome.windows.get(2000000000, (value) => {
      const error = chrome.runtime.lastError;
      resolve({value: value === undefined, error: error ? error.message : null});
    }));
    // A password manager shows on its button how many logins it has for
    // each tab, a blank one too.
    const all = await chrome.tabs.query({});
    const blank = all.find((tab) => tab.url === 'about:blank');
    const page = all.find((tab) => tab.url !== 'about:blank');
    const outcome = (promise) => promise.then(() => 'set', (error) => error.message);
    const badge = {
      blank: await outcome(chrome.action.setBadgeText({tabId: blank.id, text: '0'})),
      page: await outcome(chrome.action.setBadgeText({tabId: page.id, text: '3'})),
      shown: await chrome.action.getBadgeText({tabId: page.id}),
      missing: await outcome(chrome.action.setBadgeText({tabId: 2000000000, text: '1'})),
    };
    reply({url: window.tabs[0].url, missing, badge});
  })().catch((error) => reply({failed: error.message}));
  return true;
});
