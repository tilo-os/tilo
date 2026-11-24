// Reads a documentation book exported by `tools/docs/developer_docs.py export --out DIR` and
// renders its Markdown the way the exporter assumes: heading ids are github-slugger ids of the
// rendered heading text, so every anchor a page links to exists on the page that carries it.
//
// The sites that publish the books (tilolang.dev, docs.tilo.so) import this one module from the
// tilo checkout their build exports the books from, so the format and its reader change in one
// commit. A site provides `marked` and `github-slugger` from its own packages.

import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { Marked } from "marked";
import GithubSlugger from "github-slugger";

export interface BookPage {
  id: string;
  slug: string;
  url: string;
  title: string;
  nav: string;
  description: string;
  source: string;
  sourceUrl: string;
  section: string;
  order: number;
  markdown: string;
}

export interface BookSection {
  title: string;
  pages: BookPage[];
}

export interface Book {
  dir: string;
  title: string;
  origin: string;
  base: string;
  commit: string;
  repository: string;
  api: { title: string; url: string } | null;
  sections: BookSection[];
  pages: BookPage[];
}

export interface Heading {
  depth: number;
  text: string;
  id: string;
}

export interface Rendered {
  html: string;
  headings: Heading[];
  text: string;
}

const FRONT_MATTER = /^---\r?\n([\s\S]*?)\r?\n---\r?\n/;

function fail(book: string, message: string): never {
  throw new Error(`${book}: ${message}`);
}

function readPage(dir: string, id: string): BookPage {
  const file = path.join(dir, "pages", `${id}.md`);
  if (!existsSync(file)) fail(dir, `pages/${id}.md is missing, but manifest.json lists it`);
  const raw = readFileSync(file, "utf8");
  const matter = FRONT_MATTER.exec(raw);
  if (!matter) fail(file, "no front matter; a book page starts with --- and its fields");
  const fields: Record<string, unknown> = {};
  for (const line of matter[1].split(/\r?\n/)) {
    const at = line.indexOf(": ");
    if (at < 1) fail(file, `cannot read front matter line ${JSON.stringify(line)}`);
    try {
      fields[line.slice(0, at)] = JSON.parse(line.slice(at + 2));
    } catch {
      fail(file, `front matter value is not JSON: ${line}`);
    }
  }
  for (const key of ["id", "url", "title", "nav", "description", "source", "sourceUrl", "section"]) {
    if (typeof fields[key] !== "string") fail(file, `front matter has no ${key}`);
  }
  if (typeof fields.order !== "number") fail(file, "front matter has no order");
  return { ...(fields as unknown as BookPage), markdown: raw.slice(matter[0].length) };
}

/** Read one exported book directory, failing loudly when it is not a book. */
export function loadBook(dir: string): Book {
  const root = path.resolve(dir);
  const manifestFile = path.join(root, "manifest.json");
  if (!existsSync(manifestFile)) fail(root, "is not a book: manifest.json is missing");
  let manifest: any;
  try {
    manifest = JSON.parse(readFileSync(manifestFile, "utf8"));
  } catch (error) {
    fail(manifestFile, `is not valid JSON: ${error}`);
  }
  if (typeof manifest?.base !== "string" || !manifest.base.startsWith("/") || !manifest.base.endsWith("/")) {
    fail(manifestFile, "has no base path");
  }
  if (!Array.isArray(manifest.sections)) fail(manifestFile, "has no sections");

  const sections: BookSection[] = [];
  const pages: BookPage[] = [];
  for (const section of manifest.sections) {
    if (typeof section?.title !== "string" || !Array.isArray(section.pages)) {
      fail(manifestFile, "has a section without a title or pages");
    }
    const inSection: BookPage[] = [];
    for (const entry of section.pages) {
      if (typeof entry?.id !== "string" || typeof entry?.url !== "string") {
        fail(manifestFile, "has a page without an id or url");
      }
      const page = readPage(root, entry.id);
      for (const key of ["url", "title", "nav", "description", "source", "sourceUrl"] as const) {
        if (entry[key] !== undefined && entry[key] !== page[key]) {
          fail(root, `pages/${entry.id}.md disagrees with manifest.json on ${key}`);
        }
      }
      inSection.push(page);
    }
    sections.push({ title: section.title, pages: inSection });
    pages.push(...inSection);
  }
  if (pages.length === 0) fail(root, "has no pages");
  for (const page of pages) {
    if (!page.url.startsWith(manifest.base)) fail(root, `page url ${page.url} is outside the book base ${manifest.base}`);
  }
  return {
    dir: root,
    title: typeof manifest.title === "string" ? manifest.title : "",
    origin: typeof manifest.origin === "string" ? manifest.origin : "",
    base: manifest.base,
    commit: typeof manifest.commit === "string" ? manifest.commit : "",
    repository: typeof manifest.repository === "string" ? manifest.repository : "",
    api: manifest.api?.url ? { title: manifest.api.title ?? "API reference", url: manifest.api.url } : null,
    sections,
    pages,
  };
}

const loaded = new Map<string, Book>();

/** The book one site publishes, from its environment variable. */
export function bookFromEnv(variable: string, base?: string): Book {
  const dir = process.env[variable];
  if (!dir) {
    throw new Error(`${variable} is not set: point it at a directory written by developer_docs.py export --out.`);
  }
  const key = `${dir}\0${base ?? ""}`;
  const cached = loaded.get(key);
  if (cached) return cached;
  const book = loadBook(dir);
  if (base && book.base !== base) {
    throw new Error(`${variable}=${dir}: the book's base is ${book.base}, but this site serves it at ${base}`);
  }
  loaded.set(key, book);
  return book;
}

/** The books one site publishes, from a ':'-separated environment variable. */
const loadedBooks = new Map<string, Book[]>();

export function booksFromEnv(variable: string): Book[] {
  const spec = process.env[variable];
  if (!spec) {
    throw new Error(`${variable} is not set: point it at one or more book directories, separated by ':'.`);
  }
  const cached = loadedBooks.get(spec);
  if (cached) return cached;
  const books = spec.split(":").filter(Boolean).map(loadBook);
  const seen = new Map<string, Book>();
  for (const book of books) {
    for (const page of book.pages) {
      const other = seen.get(page.url);
      if (other) fail(book.dir, `page ${page.url} is also in ${other.dir}; two books cannot share an address`);
      seen.set(page.url, book);
    }
  }
  loadedBooks.set(spec, books);
  return books;
}

function decodeEntities(text: string): string {
  return text
    .replace(/&#(\d+);/g, (_, code) => String.fromCodePoint(Number(code)))
    .replace(/&#x([\da-f]+);/gi, (_, code) => String.fromCodePoint(parseInt(code, 16)))
    .replace(/&quot;/g, '"')
    .replace(/&nbsp;/g, " ")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&");
}

function escapeHtml(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** Render a page's Markdown with the exporter's heading anchors. */
export function renderMarkdown(markdown: string): Rendered {
  const slugger = new GithubSlugger();
  const headings: Heading[] = [];
  const marked = new Marked({
    gfm: true,
    renderer: {
      heading({ tokens, depth }) {
        const inner = this.parser.parseInline(tokens);
        const text = decodeEntities(inner.replace(/<[^>]+>/g, ""));
        const id = slugger.slug(text) || "section";
        if (depth === 2 || depth === 3) headings.push({ depth, text, id });
        return `<h${depth} id="${escapeHtml(id)}">${inner}</h${depth}>\n`;
      },
      code({ text, lang }) {
        const language = (lang ?? "").split(/\s+/)[0] ?? "";
        const label = language ? `<span class="code-lang">${escapeHtml(language)}</span>` : "";
        const classAttr = language ? ` class="language-${escapeHtml(language)}"` : "";
        return `<div class="code-block">${label}<pre><code${classAttr}>${escapeHtml(text)}</code></pre></div>\n`;
      },
      link({ href, title, tokens }) {
        const inner = this.parser.parseInline(tokens);
        const external = /^https?:\/\//i.test(href ?? "");
        const titleAttr = title ? ` title="${escapeHtml(title)}"` : "";
        const outer = external ? ` target="_blank" rel="noopener noreferrer"` : "";
        return `<a href="${escapeHtml(href ?? "")}"${titleAttr}${outer}>${inner}</a>`;
      },
    },
  });
  const html = (marked.parse(markdown) as string)
    .replace(/<table>/g, '<div class="table-scroll"><table>')
    .replace(/<\/table>/g, "</table></div>");
  const text = markdown
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/[#*_`>\[\]()]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 800);
  return { html, headings, text };
}

/** The title a book shows under: its own, or its first page's when it has none. */
export function bookTitle(book: Book): string {
  return book.title || book.pages[0]?.title || book.base;
}
