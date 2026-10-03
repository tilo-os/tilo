// Run with `bun test` from this directory after `bun install --frozen-lockfile`.
import { expect, test } from "bun:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

async function build(output) {
  const child = Bun.spawn(["bun", path.join(import.meta.dir, "build.mjs"), output], {
    stdout: "pipe",
    stderr: "pipe",
  });
  const code = await child.exited;
  expect(code, await new Response(child.stderr).text()).toBe(0);
  return readFile(output);
}

test("the bundle is self-contained, reproducible and exposes @moq/net", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "moq-bundle-"));
  try {
    const first = await build(path.join(directory, "a.mjs"));
    const second = await build(path.join(directory, "b.mjs"));
    expect(Buffer.compare(first.subarray(first.indexOf(10)), second.subarray(second.indexOf(10)))).toBe(0);
    const source = first.toString("utf8");
    expect(source).not.toMatch(/^\s*import\s.*from\s*["'](?![./])/m);
    const library = await import(path.join(directory, "a.mjs"));
    expect(typeof library.Connection.connect).toBe("function");
    expect(typeof library.Origin.Producer).toBe("function");
    expect(typeof library.Path.Pattern.subtree).toBe("function");
    const licenses = await readFile(path.join(directory, "a.LICENSE.txt"), "utf8");
    expect(licenses).toContain("@moq/net@0.4.0");
    expect(licenses).toContain("MIT License");
    expect(licenses).toContain("Apache License");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
