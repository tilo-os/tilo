// The bridge's tests run with no network: each one hands the bridge the fetch it means, so one
// the code reaches for by default is a test that would ask the real Discord. It is refused, and
// the test that did it fails naming the address.

import { afterEach } from "bun:test";

const reached = [];

globalThis.fetch = async (input) => {
  const url = typeof input === "string" ? input : (input?.url ?? String(input));
  reached.push(url);
  throw new Error(`a test reached the network: ${url}`);
};

afterEach(() => {
  if (reached.length === 0) return;
  const urls = reached.splice(0);
  throw new Error(`this test reached the network, which tests never do: ${urls.join(", ")}`);
});
