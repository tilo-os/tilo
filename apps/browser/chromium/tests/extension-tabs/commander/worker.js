// Keyboard shortcuts: a command marks the page it was pressed on, which
// activeTab lets it script, with what commands.getAll answers.
chrome.commands.onCommand.addListener(async (command, tab) => {
  const all = await chrome.commands.getAll();
  await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (seen) => { document.title = 'command:' + JSON.stringify({...seen, url: seen.url === location.href}); },
    args: [{command, url: tab.url, all}],
  });
});
