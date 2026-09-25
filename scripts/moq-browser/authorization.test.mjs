import assert from "node:assert/strict";
import test from "node:test";

import * as Parameters from "../../vendor/moq/js/net/src/ietf/parameters.ts";
import { Setup } from "../../vendor/moq/js/net/src/ietf/setup.ts";
import { Writer } from "../../vendor/moq/js/net/src/stream.ts";
import { Version } from "../../vendor/moq/js/net/src/ietf/version.ts";
import { Connection } from "../../crates/browser/qos-browser-runtime/web/media-moq-library.mjs";

function captureWriter(version) {
  const chunks = [];
  const stream = new WritableStream({
    write(chunk) {
      chunks.push(new Uint8Array(chunk));
    },
  });
  const writer = new Writer(stream, version);
  return {
    writer,
    async bytes() {
      writer.close();
      await writer.closed;
      const size = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
      const bytes = new Uint8Array(size);
      let offset = 0;
      for (const chunk of chunks) {
        bytes.set(chunk, offset);
        offset += chunk.byteLength;
      }
      return bytes;
    },
  };
}

async function encodePeerSetup(version = Version.DRAFT_21) {
  const output = captureWriter(version);
  await output.writer.u53(Setup.id);
  await new Setup().encode(output.writer, version);
  return output.bytes();
}

function containsBytes(haystack, needle) {
  return haystack.some((_, offset) =>
    offset + needle.length <= haystack.length &&
    needle.every((value, index) => haystack[offset + index] === value),
  );
}

class SetupTransport {
  #closed;
  #close;
  wasClosed = false;
  closeCalls = 0;
  setupChunks = [];

  constructor(peerSetup, protocol = "moqt-21") {
    this.protocol = protocol;
    const closed = Promise.withResolvers();
    this.#closed = closed.promise;
    this.#close = closed.resolve;
    this.incomingUnidirectionalStreams = new ReadableStream({
      start(controller) {
        controller.enqueue(
          new ReadableStream({
            start(stream) {
              stream.enqueue(peerSetup);
              stream.close();
            },
          }),
        );
        controller.close();
      },
    });
    this.incomingBidirectionalStreams = new ReadableStream({
      start(controller) {
        controller.close();
      },
    });
  }

  get closed() {
    return this.#closed;
  }

  async createUnidirectionalStream() {
    return new WritableStream({
      write: (chunk) => this.setupChunks.push(new Uint8Array(chunk)),
    });
  }

  close() {
    this.wasClosed = true;
    this.closeCalls += 1;
    this.#close();
  }

  setupBytes() {
    const size = this.setupChunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of this.setupChunks) {
      bytes.set(chunk, offset);
      offset += chunk.byteLength;
    }
    return bytes;
  }
}

test("Draft 21 authorization uses the standard token option wire fixture", async () => {
  assert.equal(typeof Parameters.setAuthorization, "function");

  const options = new Parameters.SetupOptions();
  Parameters.setAuthorization(options, new Uint8Array([0x68, 0x69]));

  const output = captureWriter(Version.DRAFT_21);
  await options.encode(output.writer, Version.DRAFT_21);

  assert.deepEqual(
    await output.bytes(),
    new Uint8Array([0x03, 0x04, 0x03, 0x00, 0x68, 0x69]),
  );
});

test("authorization preserves the largest permitted opaque proof", () => {
  assert.equal(typeof Parameters.setAuthorization, "function");

  const proof = new Uint8Array(8192);
  proof[0] = 0xa5;
  proof[proof.length - 1] = 0x5a;
  const options = new Parameters.SetupOptions();
  Parameters.setAuthorization(options, proof);

  assert.deepEqual(
    options.getBytes(Parameters.SetupOption.AuthorizationToken),
    new Uint8Array([0x03, 0x00, ...proof]),
  );
});

test("authorization rejects an opaque proof larger than 8 KiB", () => {
  assert.equal(typeof Parameters.setAuthorization, "function");

  const options = new Parameters.SetupOptions();
  assert.throws(
    () => Parameters.setAuthorization(options, new Uint8Array(8193)),
    /authorization payload exceeds 8192 bytes/,
  );
  assert.equal(options.getBytes(Parameters.SetupOption.AuthorizationToken), undefined);
});

test("connect rejects oversized authorization before opening a transport", async () => {
  const original = globalThis.WebTransport;
  let constructed = 0;
  globalThis.WebTransport = class {
    constructor() {
      constructed += 1;
      this.ready = Promise.reject(new Error("transport opened"));
      this.closed = Promise.resolve();
    }

    close() {}
  };

  try {
    await assert.rejects(
      Connection.connect(new URL("https://relay.invalid/media"), {
        authorization: new Uint8Array(8193),
        websocket: { enabled: false },
        webtransport: { protocols: ["moqt-21"] },
      }),
      /authorization payload exceeds 8192 bytes/,
    );
    assert.equal(constructed, 0);
  } finally {
    globalThis.WebTransport = original;
  }
});

test("connect places the opaque proof in the Draft 21 SETUP authorization option", async () => {
  const transport = new SetupTransport(await encodePeerSetup());
  const connection = await Connection.connect(new URL("https://relay.invalid/media"), {
    authorization: new Uint8Array([0x68, 0x69]),
    transport,
    websocket: { enabled: false },
  });

  try {
    assert.equal(connection.version, "moq-transport-21");
    // Stream writes are coalesced and handed to the transport right after the encoding task.
    await new Promise((resolve) => setTimeout(resolve, 20));
    assert.equal(
      containsBytes(transport.setupBytes(), new Uint8Array([0x03, 0x04, 0x03, 0x00, 0x68, 0x69])),
      true,
    );
  } finally {
    connection.close();
  }
});

test("authorized connect requires explicit Draft 21 WebTransport without WebSocket", async () => {
  const original = globalThis.WebTransport;
  let constructed = 0;
  globalThis.WebTransport = class {
    constructor() {
      constructed += 1;
      this.ready = Promise.reject(new Error("transport opened"));
      this.closed = Promise.resolve();
    }

    close() {}
  };

  try {
    await assert.rejects(
      Connection.connect(new URL("https://relay.invalid/media"), {
        authorization: new Uint8Array([1]),
        webtransport: { protocols: ["moqt-21"] },
      }),
      /authorization requires WebSocket fallback to be disabled/,
    );
    await assert.rejects(
      Connection.connect(new URL("https://relay.invalid/media"), {
        authorization: new Uint8Array([1]),
        websocket: { enabled: false },
        webtransport: { protocols: ["moqt-20"] },
      }),
      /authorization requires WebTransport protocol moqt-21 only/,
    );
    assert.equal(constructed, 0);
  } finally {
    globalThis.WebTransport = original;
  }
});

test("authorized connect closes a supplied transport that negotiated another draft", async () => {
  const transport = new SetupTransport(await encodePeerSetup(Version.DRAFT_20), "moqt-20");

  await assert.rejects(
    Connection.connect(new URL("https://relay.invalid/media"), {
      authorization: new Uint8Array([1]),
      transport,
      websocket: { enabled: false },
    }),
    /authorization requires negotiated protocol moqt-21/,
  );
  assert.equal(transport.wasClosed, true);
  assert.equal(transport.setupChunks.length, 0);
});

test("connect closes a supplied transport when the peer SETUP is malformed", async () => {
  const controller = new AbortController();
  const transport = new SetupTransport(new Uint8Array([0]), "moqt-21");

  await assert.rejects(
    Connection.connect(new URL("https://relay.invalid/media"), {
      authorization: new Uint8Array([1]),
      transport,
      websocket: { enabled: false },
      signal: controller.signal,
    }),
    /unexpected stream type/,
  );
  assert.equal(transport.closeCalls, 1);

  controller.abort(new Error("too late"));
  await Promise.resolve();
  assert.equal(transport.closeCalls, 1);
});

test("connect closes an internally dialed transport when the peer SETUP is malformed", async () => {
  const original = globalThis.WebTransport;
  let transport;
  globalThis.WebTransport = class extends SetupTransport {
    constructor(_url, options) {
      super(new Uint8Array([0]), options.protocols[0]);
      this.ready = Promise.resolve();
      transport = this;
    }
  };

  try {
    const controller = new AbortController();
    await assert.rejects(
      Connection.connect(new URL("https://relay.invalid/media"), {
        authorization: new Uint8Array([1]),
        websocket: { enabled: false },
        webtransport: { protocols: ["moqt-21"] },
        signal: controller.signal,
      }),
      /unexpected stream type/,
    );
    assert.equal(transport.closeCalls, 1);

    controller.abort(new Error("too late"));
    await Promise.resolve();
    assert.equal(transport.closeCalls, 1);
  } finally {
    globalThis.WebTransport = original;
  }
});
