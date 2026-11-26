/// A fixture page presents its device using the production WK challenge.
let testDeviceHandoff = #"""
async function giveDevice(device) {
  const host = globalThis.tiloNativeHost;
  const nonce = crypto.randomUUID();
  const confirmed = host.confirmDevice(nonce);
  webkit.messageHandlers.tiloLive.postMessage({kind: 'device-challenge', nonce});
  if (await confirmed) return host.device(device, nonce);
  return false;
}
"""#
