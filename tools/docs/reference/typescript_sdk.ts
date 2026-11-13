#!/usr/bin/env bun
/**
 * Write the TypeScript SDK reference from the package's own sources.
 *
 * The public API is what `sdk/typescript/src/index.ts` exports: each name is
 * resolved to the declaration it comes from, and the page shows its signature
 * and its TSDoc. An export, or a public method of one, that has no TSDoc stops
 * the generator, unless the method is marked `@internal` (the SDK's own
 * plumbing, which a caller never calls). The generated `types` package under
 * the SDK is left out: `tilo sdk gen` writes it from the apps a workspace runs.
 *
 * The TypeScript compiler API does the reading, so the page follows the types
 * the SDK is built with rather than a parser of our own. `typescript` is the
 * SDK's devDependency; a run outside the SDK's directory names it:
 *
 *     TILO_TYPESCRIPT_MODULE=<path to typescript/lib/typescript.js> \
 *       bun tools/docs/reference/typescript_sdk.ts --root <repository> --out <page.md>
 */

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";

const MODULE = process.env.TILO_TYPESCRIPT_MODULE ?? "typescript";
let ts: typeof import("typescript");
try {
  ts = await import(MODULE);
} catch (error) {
  fail(
    `cannot load ${MODULE}: install the TypeScript SDK's packages ` +
      `(cd sdk/typescript && bun install), or set TILO_TYPESCRIPT_MODULE to its typescript module`,
  );
}

const LANGUAGES = new Set(["python", "typescript", "sh", "json"]);
// How a source's own name for a language is written on a page.
const ALIASES: Record<string, string> = { ts: "typescript", js: "typescript", bash: "sh", shell: "sh", py: "python" };

// The order a reader meets the SDK in: an export no group holds is an error,
// so a new one cannot reach the page without a place to sit.
const GROUPS: [string, string[]][] = [
  ["Client", ["Tilo", "TiloOptions", "Workspaces", "Keys", "SignIn", "urlFrom", "keyFrom"]],
  ["Workspaces", ["Workspace", "WorkspaceInfo", "WorkspaceList"]],
  ["Account", ["Usage", "WorkspaceUsage", "ApiInfo"]],
  ["Resources", [
    "Resource", "Step", "Surface", "resourceProxy", "kebab", "segments",
    "Listing", "Row", "Action", "Param", "Hit",
  ]],
  ["Runs", ["Run", "RunGroup", "withinRun", "ActionAnswer", "Event", "ProgramAnswer"]],
  ["Files", ["Files", "FileBytes", "Upload"]],
  ["Keys", ["WorkspaceKeys", "Key", "MintedKey"]],
  ["Members", ["Members", "Member", "Invitation", "Person"]],
  ["Connection", [
    "Transport", "HttpTransport", "NativeTransport", "HostOptions", "TransportRequest",
    "TransportReply", "HeadersLike", "HeaderMap", "FetchLike", "Endpoint",
    "RequestOptions", "Reply", "DEFAULT_URL", "NATIVE_PACKAGE", "VERSION", "API_VERSION",
  ]],
  ["Routes", ["routes", "encodePath", "splitAction"]],
  ["Errors", [
    "Refused", "RateLimited", "AskPending", "ProtocolError",
    "KeyRefused", "KeyRevoked", "KeyExpired", "SurfaceChanged",
  ]],
];

class ReferenceError extends Error {}

function fail(message: string): never {
  throw new ReferenceError(message);
}

type Declaration =
  | import("typescript").ClassDeclaration
  | import("typescript").InterfaceDeclaration
  | import("typescript").TypeAliasDeclaration
  | import("typescript").FunctionDeclaration
  | import("typescript").VariableDeclaration;

interface Item {
  name: string;
  node: Declaration;
  doc: string;
}

function main(): void {
  const args = argumentsOf(process.argv.slice(2));
  const root = resolve(args.root);
  const indexPath = resolve(root, "sdk/typescript/src/index.ts");
  const sdk = resolve(root, "sdk/typescript");
  const program = ts.createProgram([indexPath], {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    allowImportingTsExtensions: true,
    skipLibCheck: true,
    noEmit: true,
    types: [],
  });
  const checker = program.getTypeChecker();
  const source = program.getSourceFile(indexPath);
  if (source === undefined) fail(`${indexPath} is not there: is --root the repository?`);
  const moduleSymbol = checker.getSymbolAtLocation(source);
  if (moduleSymbol === undefined) fail(`${indexPath} exports nothing`);
  const items = new Map<string, Item>();
  for (const exported of checker.getExportsOfModule(moduleSymbol)) {
    const name = exported.getName();
    const symbol = exported.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(exported) : exported;
    const node = declarationOf(symbol, name, indexPath);
    items.set(name, { name, node, doc: docText(symbol.getDocumentationComment(checker)) });
  }
  checkPlaced(items);
  const undocumented = [...items.values()]
    .filter((item) => item.doc.trim() === "")
    .map((item) => item.name);
  if (undocumented.length > 0) {
    fail(`exports with no TSDoc:\n  ${undocumented.join("\n  ")}`);
  }
  checkMembers(items, checker);
  const packageJson = JSON.parse(readFileSync(resolve(sdk, "package.json"), "utf8")) as {
    name?: string;
    version?: string;
    description?: string;
  };
  const version = items.get("VERSION");
  const written = version === undefined ? "" : sourceText(version.node);
  const stated = /"([^"]+)"/.exec(written)?.[1] ?? "";
  if (packageJson.version !== stated) {
    fail(`sdk/typescript/src/index.ts says ${stated} and package.json says ${packageJson.version}`);
  }
  const sections: string[] = [
    prose(
      `${packageJson.description ?? ""} This page is generated from the package's own ` +
        `sources and documents ${packageJson.name} ${stated}.`,
    ),
    code("sh", "bun add tilo-sdk"),
  ];
  for (const [title, names] of GROUPS) {
    sections.push(heading(2, title));
    for (const name of names) {
      const item = items.get(name);
      if (item === undefined) fail(`${title} names ${name}, which index.ts does not export`);
      sections.push(...render(item, checker));
    }
  }
  sections.push(
    heading(2, "App types"),
    prose(
      "Every app a workspace runs publishes its resources and actions, and the SDK " +
        "can write them as TypeScript types so an editor completes a resource's " +
        "fields and its actions' arguments. The types follow the apps the workspace " +
        "runs, so they are written from it rather than shipped with the package: " +
        "`tilo sdk gen` reads a workspace and writes the `types` package.",
    ),
    code("sh", "tilo sdk gen --lang ts --out types"),
    prose(
      "Import a type from `tilo-sdk/types` and read a resource with it; the " +
        "workspace answers the fields the app publishes now. Run the command again " +
        "after an app updates, so the types name what it offers.",
    ),
  );
  const page = [
    heading(1, "TypeScript SDK"),
    prose(moduleDoc(source)),
    ...sections,
  ].join("\n\n");
  check(page);
  mkdirSync(dirname(args.out), { recursive: true });
  writeFileSync(args.out, page + "\n");
}

function argumentsOf(argv: string[]): { root: string; out: string } {
  const parsed: Record<string, string> = {};
  for (let index = 0; index < argv.length; index += 2) {
    const name = argv[index] ?? "";
    const value = argv[index + 1];
    if (!name.startsWith("--") || value === undefined) fail(`usage: --root <repository> --out <page.md>`);
    parsed[name.slice(2)] = value;
  }
  if (parsed.root === undefined || parsed.out === undefined) {
    fail("usage: --root <repository> --out <page.md>");
  }
  return { root: parsed.root, out: parsed.out };
}

function declarationOf(
  symbol: import("typescript").Symbol,
  name: string,
  indexPath: string,
): Declaration {
  const found = symbol.getDeclarations() ?? [];
  const node = found.find(
    (candidate) =>
      ts.isClassDeclaration(candidate) ||
      ts.isInterfaceDeclaration(candidate) ||
      ts.isTypeAliasDeclaration(candidate) ||
      ts.isFunctionDeclaration(candidate) ||
      ts.isVariableDeclaration(candidate),
  );
  if (node === undefined) {
    fail(`${name} is exported from ${indexPath} as something a reference cannot show`);
  }
  return node as Declaration;
}

/** Every documented member says something; an undocumented one is an error, not a gap. */
function checkMembers(items: Map<string, Item>, checker: import("typescript").TypeChecker): void {
  const missing: string[] = [];
  for (const item of items.values()) {
    if (!ts.isClassDeclaration(item.node)) continue;
    for (const member of item.node.members) {
      if (!isSurfaceMember(member)) continue;
      const symbol = checker.getSymbolAtLocation(member.name as import("typescript").Node);
      const doc = symbol === undefined ? "" : docText(symbol.getDocumentationComment(checker));
      if (doc.trim() === "") {
        missing.push(`${item.name}.${(member.name as import("typescript").Identifier).text}`);
      }
    }
  }
  if (missing.length > 0) {
    fail(`public members with no TSDoc:\n  ${missing.join("\n  ")}`);
  }
}

function checkPlaced(items: Map<string, Item>): void {  const placed = new Set(GROUPS.flatMap(([, names]) => names));
  const unplaced = [...items.keys()].filter((name) => !placed.has(name)).sort();
  if (unplaced.length > 0) fail(`no group holds: ${unplaced.join(", ")}`);
}

function render(item: Item, checker: import("typescript").TypeChecker): string[] {
  const parts = [heading(3, item.name), code("typescript", declarationSignature(item.node, item.name, checker))];
  parts.push(prose(item.doc));
  if (ts.isClassDeclaration(item.node)) {
    for (const member of item.node.members) {
      if (!isSurfaceMember(member)) continue;
      parts.push(...renderMember(item.name, member, checker));
    }
  }
  return parts;
}

function isSurfaceMember(member: import("typescript").ClassElement): boolean {
  if (!ts.isMethodDeclaration(member) && !ts.isGetAccessorDeclaration(member)) return false;
  const name = member.name;
  if (name === undefined || !(ts.isIdentifier(name) || ts.isStringLiteral(name))) return false;
  const written = name.text;
  if (written.startsWith("#") || written.startsWith("_") || written === "toString") return false;
  return !hasTag(member, "internal");
}

function hasTag(node: import("typescript").Node, tag: string): boolean {
  const jsDoc = (node as { jsDoc?: import("typescript").JSDoc[] }).jsDoc ?? [];
  return jsDoc.some((comment) =>
    (comment.tags ?? []).some((found) => found.tagName.text === tag),
  );
}

function renderMember(
  owner: string,
  member: import("typescript").ClassElement,
  checker: import("typescript").TypeChecker,
): string[] {
  const name = (member.name as import("typescript").Identifier).text;
  const symbol = checker.getSymbolAtLocation(member.name as import("typescript").Node);
  const doc = symbol === undefined ? "" : docText(symbol.getDocumentationComment(checker));
  if (doc.trim() === "") {
    fail(`${owner}.${name} is public with no TSDoc: document it, or mark it @internal`);
  }
  const signature = checker.signatureToString(
    checker.getSignatureFromDeclaration(member as import("typescript").SignatureDeclaration),
  );
  const accessor = ts.isGetAccessorDeclaration(member);
  const line = accessor
    ? `${owner}.${name}: ${signature.replace(/^\(\): /, "")}`
    : `${owner}.${name}${signature}`;
  return [heading(4, `${owner}.${name}`), code("typescript", line), prose(doc)];
}

function declarationSignature(
  node: Declaration,
  name: string,
  checker: import("typescript").TypeChecker,
): string {
  if (ts.isFunctionDeclaration(node)) {
    const signature = checker.signatureToString(checker.getSignatureFromDeclaration(node));
    return `${name}${signature}`;
  }
  if (ts.isClassDeclaration(node)) {
    const heritage = node.heritageClauses?.map((clause) => clause.getText()).join(" ") ?? "";
    const lines = [`class ${name}${heritage ? ` ${heritage}` : ""}`];
    const constructor = node.members.find(ts.isConstructorDeclaration);
    if (constructor !== undefined) {
      lines.push(
        `${name}${checker.signatureToString(checker.getSignatureFromDeclaration(constructor))}`,
      );
    }
    return lines.join("\n");
  }
  return sourceText(node);
}

function sourceText(node: Declaration): string {
  if (ts.isVariableDeclaration(node)) {
    const statement = node.parent.parent;
    return statement.getText().replace(/^export\s+/, "");
  }
  return node.getText().replace(/^export\s+/, "");
}

/** The file's first `/** … *\/`, which says what the package is. */
function moduleDoc(source: import("typescript").SourceFile): string {
  const [comment] = ts.getLeadingCommentRanges(source.getFullText(), 0) ?? [];
  if (comment === undefined || !comment.hasTrailingNewLine) {
    fail("sdk/typescript/src/index.ts opens with the package's doc comment");
  }
  const text = source.getFullText().slice(comment.pos, comment.end);
  const inner = text.replace(/^\/\*\*?/, "").replace(/\*\/$/, "");
  const lines = inner.split("\n").map((line) => line.replace(/^\s*\* ?/, "").trimEnd());
  const doc = lines.join("\n").trim();
  if (doc === "") fail("sdk/typescript/src/index.ts's doc comment says nothing");
  return doc;
}

/** A TSDoc comment as prose, its inline code kept as code. */
function docText(parts: readonly import("typescript").SymbolDisplayPart[]): string {
  return parts
    .map((part) => (part.kind === "code" ? `\`${part.text}\`` : part.text))
    .join("")
    .trim();
}

function heading(level: number, text: string): string {
  if (text === "" || text.includes("\n")) fail(`a heading needs one line of text: ${JSON.stringify(text)}`);
  if (/[`[*]/.test(text)) fail(`heading ${JSON.stringify(text)}: keep it plain words, no markup`);
  return `${"#".repeat(level)} ${text}`;
}

/** A docstring as prose: dedented, every example block a fragment. */
function prose(text: string): string {
  if (text.trim() === "") fail("a documented item has no text");
  const lines = dedent(text).split("\n");
  const marked: string[] = [];
  let fence: string | null = null;
  for (const line of lines) {
    const opening = /^ {0,3}(`{3,})(.*)$/.exec(line);
    if (fence === null && opening !== null) {
      fence = opening[1] ?? "```";
      const info = (opening[2] ?? "").trim();
      const [named = "", ...rest] = info.split(" ");
      const language = ALIASES[named] ?? named;
      if (language !== "" && !LANGUAGES.has(language)) {
        fail(`a doc fences ${JSON.stringify(language)}, which no page fences`);
      }
      const words = rest.filter((word) => word !== "fragment");
      if (language !== "") words.push("fragment");
      marked.push([fence + language, ...words].join(" ").trim());
      continue;
    }
    if (fence !== null && opening !== null && (opening[2] ?? "").trim() === "") {
      fence = null;
    }
    marked.push(line);
  }
  if (fence !== null) fail("a doc opens a code fence and never closes it");
  return marked.join("\n").trim();
}

function dedent(text: string): string {
  const lines = text.replace(/\t/g, "    ").split("\n");
  while (lines.length > 0 && (lines[0] ?? "").trim() === "") lines.shift();
  while (lines.length > 0 && (lines[lines.length - 1] ?? "").trim() === "") lines.pop();
  const indents = lines
    .filter((line) => line.trim() !== "")
    .map((line) => (/^ */.exec(line)?.[0] ?? "").length);
  const least = indents.length === 0 ? 0 : Math.min(...indents);
  return lines.map((line) => line.slice(Math.min(least, /^ */.exec(line)?.[0].length ?? 0))).join("\n");
}

function code(language: string, body: string): string {
  const named = ALIASES[language] ?? language;
  if (!LANGUAGES.has(named)) fail(`${JSON.stringify(language)} is not a language a reference page fences`);
  if (body.trim() === "") fail("a fenced block needs something in it");
  if (body.includes("```")) fail("a fenced block cannot contain a fence of its own");
  return `\`\`\`${named} fragment\n${body.replace(/^\n+|\n+$/g, "")}\n\`\`\``;
}

/** One H1, and every block a fragment with a language. */
function check(page: string): void {
  let fence: string | null = null;
  let headings = 0;
  for (const line of page.split("\n")) {
    const opening = /^ {0,3}(`{3,})(.*)$/.exec(line);
    if (fence === null) {
      if (/^ {0,3}# /.test(line)) headings += 1;
      if (opening !== null) {
        fence = opening[1] ?? "```";
        const info = (opening[2] ?? "").trim().split(" ");
        if (!LANGUAGES.has(info[0] ?? "")) fail(`a fenced block names no language: ${JSON.stringify(line)}`);
        if (!info.includes("fragment")) fail(`a fenced block is not marked fragment: ${JSON.stringify(line)}`);
      }
    } else if (opening !== null && (opening[2] ?? "").trim() === "") {
      fence = null;
    }
  }
  if (fence !== null) fail("the page opens a code fence and never closes it");
  if (headings !== 1) fail(`a page has exactly one H1, not ${headings}`);
}

try {
  main();
} catch (error) {
  if (error instanceof ReferenceError) {
    console.error(`typescript_sdk.ts: ${error.message}`);
    process.exit(1);
  }
  throw error;
}
