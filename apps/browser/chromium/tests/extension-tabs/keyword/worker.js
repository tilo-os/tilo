// The address's keyword: what the person types after it reaches the worker,
// which suggests for each change and, on Enter, sends the page it was typed
// over (whose address activeTab lets it read) to what it heard.
globalThis.heard = [];
chrome.omnibox.setDefaultSuggestion({description: 'Look up <match>%s</match>'});
chrome.omnibox.onInputStarted.addListener(() => heard.push('started'));
chrome.omnibox.onInputChanged.addListener((text, suggest) => {
  heard.push('changed:' + text);
  suggest([
    {content: text + ' one', description: `First <match>${text}</match> &amp; more`},
    {content: text + ' two', description: 'Second'},
  ]);
});
chrome.omnibox.onInputCancelled.addListener(() => heard.push('cancelled'));
chrome.omnibox.onInputEntered.addListener(async (text, disposition) => {
  heard.push(`entered:${text}:${disposition}`);
  const [tab] = await chrome.tabs.query({active: true, currentWindow: true});
  await chrome.tabs.update(tab.id, {url: new URL('/found?' + encodeURIComponent(JSON.stringify(heard)), tab.url).href});
});
