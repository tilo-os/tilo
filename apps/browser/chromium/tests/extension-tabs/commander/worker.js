// A command's shortcut marks the page it was pressed over, which the
// shortcut lets it script (activeTab), with every command it heard of and
// the shortcuts it was given.
const heard = [];
chrome.commands.onCommand.addListener(async (command, tab) => {
  heard.push(command);
  const all = await new Promise((resolve) => chrome.commands.getAll(resolve));
  await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (state) => { document.title = 'command:' + JSON.stringify({...state, url: location.href === state.url}); },
    args: [{heard, url: tab.url, all}],
  });
});
