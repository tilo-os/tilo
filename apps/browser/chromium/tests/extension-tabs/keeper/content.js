chrome.runtime.sendMessage({kept: true}).then((answer) => {
  document.title = 'kept:' + JSON.stringify(answer);
});
