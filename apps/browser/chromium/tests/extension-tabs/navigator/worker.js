// What a tab manager or a password manager follows of the tabs: their
// navigations, frame by frame, and the tab events. Kept readable, in order.
globalThis.heard = [];
const keep = (name) => (...args) => heard.push({name, args: JSON.parse(JSON.stringify(args))});
for (const name of ['onBeforeNavigate', 'onCommitted', 'onDOMContentLoaded', 'onCompleted', 'onErrorOccurred',
  'onReferenceFragmentUpdated', 'onHistoryStateUpdated', 'onCreatedNavigationTarget', 'onTabReplaced']) {
  chrome.webNavigation[name].addListener(keep('webNavigation.' + name));
}
for (const name of ['onCreated', 'onActivated', 'onHighlighted', 'onZoomChange', 'onReplaced', 'onAttached',
  'onDetached', 'onMoved']) {
  chrome.tabs[name].addListener(keep('tabs.' + name));
}
