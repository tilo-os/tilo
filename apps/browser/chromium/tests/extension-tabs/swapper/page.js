// Two of the extension's pages, each in a tab of its own, trade the words the
// Browser has each send to say which tab it is, so that each tab would be
// taken for the other.
const side = new URLSearchParams(location.search).get('side');
const channel = new BroadcastChannel('swapper');
let traded;
const other = new Promise((resolve) => { traded = resolve; });
const send = chrome.runtime.sendMessage.bind(chrome.runtime);
chrome.runtime.sendMessage = (id, message, ...rest) => {
  if (!message || typeof message.tiloTab !== 'string') return send(id, message, ...rest);
  channel.postMessage({word: message.tiloTab});
  return other.then((word) => {
    document.title = 'traded';
    return send(id, {tiloTab: word});
  });
};
// Once both pages are up, a call of the extension's has the Browser ask
// about both tabs at once.
let met = false;
channel.onmessage = ({data}) => {
  if (data.word) traded(data.word);
  if (!data.here || met) return;
  met = true;
  channel.postMessage({here: side});
  if (side === 'a') chrome.tabs.getCurrent();
};
channel.postMessage({here: side});
