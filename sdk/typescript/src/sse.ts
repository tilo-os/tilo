/**
 * A server-sent event stream, read as its bytes arrive.
 *
 * `…/events` and `…/runs/{run}/output` answer `text/event-stream`: fields
 * (`id`, `event`, `data`), a blank line between frames, and `: keep-alive`
 * comment lines while nothing happens. An `id` is a cursor: it holds for the
 * frames after it, which is why only the last of a batch carries one.
 */

/** One frame of a stream: its cursor, its kind, and its JSON. */
export interface Frame {
  id: string | null;
  event: string | null;
  data: string;
}

/** The frames of `chunks`, in order, as they come. */
export async function* frames(chunks: AsyncIterable<Uint8Array>): AsyncGenerator<Frame> {
  const decoder = new TextDecoder();
  let buffer = "";
  let id: string | null = null;
  let kind: string | null = null;
  let data: string[] = [];
  for await (const chunk of chunks) {
    buffer += decoder.decode(chunk, { stream: true });
    for (;;) {
      const at = buffer.indexOf("\n");
      if (at < 0) {
        break;
      }
      const line = buffer.slice(0, at).replace(/\r$/, "");
      buffer = buffer.slice(at + 1);
      if (line === "") {
        if (data.length > 0 || kind !== null) {
          yield { id, event: kind, data: data.join("\n") };
        }
        kind = null;
        data = [];
        continue;
      }
      if (line.startsWith(":")) {
        continue;
      }
      const colon = line.indexOf(":");
      const field = colon < 0 ? line : line.slice(0, colon);
      const value = colon < 0 ? "" : line.slice(colon + 1).replace(/^ /, "");
      if (field === "id") {
        id = value === "" ? null : value;
      } else if (field === "event") {
        kind = value;
      } else if (field === "data") {
        data.push(value);
      }
    }
  }
}
