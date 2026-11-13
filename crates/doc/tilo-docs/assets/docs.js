// Tilo docs: colour theme, the page list on phones, language tabs that stay
// in step across the page and across visits, copy buttons, the contents'
// current heading, and search. Every page works without it.
"use strict";

const base = document.body.dataset.base || "/";

function remember(key, value) {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch (_) { /* private windows keep nothing */ }
}

function recall(key) {
  try { return localStorage.getItem(key); } catch (_) { return null; }
}

// Colour theme: as the system has it, light, or dark.

const themes = ["system", "light", "dark"];
const themeNames = { system: "as the system", light: "light", dark: "dark" };
const themeButton = document.querySelector(".theme");

function showTheme(theme) {
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  const label = `Colour theme: ${themeNames[theme]}`;
  themeButton?.setAttribute("aria-label", label);
  themeButton?.setAttribute("title", label);
}

showTheme(recall("docs-theme") || "system");
themeButton?.addEventListener("click", () => {
  const current = recall("docs-theme") || "system";
  const next = themes[(themes.indexOf(current) + 1) % themes.length];
  remember("docs-theme", next === "system" ? null : next);
  showTheme(next);
});

// The page list, as a drawer on narrow screens.

const menu = document.querySelector(".menu");

function showNav(open) {
  document.body.classList.toggle("nav-open", open);
  menu?.setAttribute("aria-expanded", String(open));
  menu?.setAttribute("aria-label", open ? "Hide the pages" : "Show the pages");
}

menu?.addEventListener("click", () => showNav(!document.body.classList.contains("nav-open")));
document.addEventListener("click", (event) => {
  if (document.body.classList.contains("nav-open") && !event.target.closest(".nav, .menu")) showNav(false);
});
document.querySelector(".nav")?.addEventListener("click", (event) => {
  if (event.target.closest("a")) showNav(false);
});
document.querySelector('.nav [aria-current="page"]')?.scrollIntoView({ block: "nearest" });

// Language tabs: choosing a language shows it in every example that has it.

function showLanguage(language) {
  for (const group of document.querySelectorAll(".code[data-example]")) {
    if (!group.querySelector(`pre[data-lang="${CSS.escape(language)}"]`)) continue;
    for (const tab of group.querySelectorAll("[role=tab]")) {
      tab.setAttribute("aria-selected", String(tab.dataset.lang === language));
    }
    for (const pre of group.querySelectorAll("pre")) pre.hidden = pre.dataset.lang !== language;
  }
}

const preferred = recall("docs-language");
if (preferred) showLanguage(preferred);

document.addEventListener("click", (event) => {
  const tab = event.target.closest(".tabs [role=tab]");
  if (!tab) return;
  // The chosen example stays where it is on screen while the others change.
  const group = tab.closest(".code");
  const before = group.getBoundingClientRect().top;
  showLanguage(tab.dataset.lang);
  window.scrollBy(0, group.getBoundingClientRect().top - before);
  remember("docs-language", tab.dataset.lang);
});

document.addEventListener("keydown", (event) => {
  const tab = event.target.closest?.(".tabs [role=tab]");
  if (!tab || (event.key !== "ArrowRight" && event.key !== "ArrowLeft")) return;
  const tabs = [...tab.parentElement.children];
  const next = tabs[(tabs.indexOf(tab) + (event.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length];
  next.focus();
  next.click();
});

// Copy: the code shown, as it is.

document.addEventListener("click", async (event) => {
  const button = event.target.closest(".copy");
  if (!button) return;
  const pre = [...button.closest(".code").querySelectorAll("pre")].find((pre) => !pre.hidden);
  try {
    await navigator.clipboard.writeText(pre.textContent);
    button.textContent = "Copied";
    button.dataset.done = "";
  } catch (_) {
    button.textContent = "Select and copy";
  }
  setTimeout(() => {
    button.textContent = "Copy";
    delete button.dataset.done;
  }, 1600);
});

// On this page: the heading being read.

const tocLinks = new Map(
  [...document.querySelectorAll(".toc a")].map((link) => [decodeURIComponent(link.hash.slice(1)), link]),
);
if (tocLinks.size && "IntersectionObserver" in window) {
  const headings = [...tocLinks.keys()].map((id) => document.getElementById(id)).filter(Boolean);
  const visible = new Set();
  const observer = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (entry.isIntersecting) visible.add(entry.target);
      else visible.delete(entry.target);
    }
    const current = headings.find((heading) => visible.has(heading))
      || headings.filter((heading) => heading.getBoundingClientRect().top < 0).pop();
    for (const link of tocLinks.values()) link.removeAttribute("aria-current");
    if (current) tocLinks.get(current.id)?.setAttribute("aria-current", "true");
  }, { rootMargin: "-56px 0px -60% 0px" });
  headings.forEach((heading) => observer.observe(heading));
}

// Search.

const dialog = document.querySelector("dialog.search");
const input = dialog?.querySelector("input");
const results = dialog?.querySelector(".results");
const empty = dialog?.querySelector(".search-empty");
let index = null;
let selected = 0;

async function loadIndex() {
  if (!index) {
    const response = await fetch(`${base}search.json`);
    index = (await response.json()).map((page) => ({
      ...page,
      lower: {
        title: page.title.toLowerCase(),
        description: page.description.toLowerCase(),
        headings: page.headings.map(([, text]) => text.toLowerCase()),
        text: page.text.toLowerCase(),
      },
    }));
  }
  return index;
}

function openSearch() {
  if (!dialog || dialog.open) return;
  dialog.showModal();
  input.select();
  loadIndex().then(() => find(input.value));
}

function words(query) {
  return query.toLowerCase().split(/[^\p{L}\p{N}_#./-]+/u).filter(Boolean);
}

// A page matches when every word is somewhere in it; titles weigh most, then
// headings, then the description, then the text.
function score(page, terms) {
  let total = 0;
  for (const term of terms) {
    let best = 0;
    if (page.lower.title.includes(term)) best = page.lower.title.startsWith(term) ? 12 : 10;
    if (page.lower.headings.some((text) => text.includes(term))) best = Math.max(best, 6);
    if (page.lower.description.includes(term)) best = Math.max(best, 4);
    if (!best && page.lower.text.includes(term)) best = 1;
    if (!best) return null;
    total += best;
  }
  // The heading that matches the most words is where the hit opens.
  let heading = null;
  let most = 0;
  page.lower.headings.forEach((text, at) => {
    const count = terms.filter((term) => text.includes(term)).length;
    if (count > most) [heading, most] = [page.headings[at], count];
  });
  return { total, heading };
}

// Around the most telling word: the longest one the text has.
function excerpt(page, terms) {
  const found = terms.filter((term) => page.lower.text.includes(term)).sort((a, b) => b.length - a.length);
  const position = found.length ? page.lower.text.indexOf(found[0]) : 0;
  const start = Math.max(0, position - 60);
  const text = page.text.slice(start, start + 180);
  return (start > 0 ? "…" : "") + text + (start + 180 < page.text.length ? "…" : "");
}

function marked(text, terms) {
  const fragment = document.createDocumentFragment();
  const pattern = new RegExp(`(${terms.map((term) => term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`, "gi");
  for (const [number, part] of text.split(pattern).entries()) {
    if (number % 2) {
      const mark = document.createElement("mark");
      mark.textContent = part;
      fragment.append(mark);
    } else {
      fragment.append(part);
    }
  }
  return fragment;
}

function find(query) {
  if (!index) return;
  const terms = words(query);
  results.replaceChildren();
  empty.hidden = true;
  if (!terms.length) return;
  const hits = index
    .map((page) => ({ page, match: score(page, terms) }))
    .filter((hit) => hit.match)
    .sort((a, b) => b.match.total - a.match.total)
    .slice(0, 10);
  empty.hidden = hits.length > 0;
  selected = 0;
  hits.forEach(({ page, match }, number) => {
    const item = document.createElement("li");
    item.setAttribute("role", "option");
    item.id = `result-${number}`;
    item.setAttribute("aria-selected", String(number === 0));
    const link = document.createElement("a");
    link.href = match.heading ? `${page.url}#${match.heading[0]}` : page.url;
    const title = document.createElement("span");
    title.className = "result-title";
    title.append(marked(page.title, terms));
    const where = document.createElement("span");
    where.className = "result-where";
    where.textContent = match.heading ? match.heading[1] : page.section;
    const text = document.createElement("div");
    text.className = "result-text";
    text.append(marked(excerpt(page, terms) || page.description, terms));
    link.append(title, where, text);
    item.append(link);
    results.append(item);
  });
}

function select(step) {
  const items = [...results.children];
  if (!items.length) return;
  items[selected]?.setAttribute("aria-selected", "false");
  selected = (selected + step + items.length) % items.length;
  items[selected].setAttribute("aria-selected", "true");
  items[selected].scrollIntoView({ block: "nearest" });
}

document.querySelectorAll("[data-search]").forEach((button) => button.addEventListener("click", openSearch));
document.addEventListener("keydown", (event) => {
  const typing = event.target.closest?.("input, textarea, [contenteditable]");
  if ((event.key === "/" && !typing) || (event.key === "k" && (event.metaKey || event.ctrlKey))) {
    event.preventDefault();
    openSearch();
  } else if (event.key === "Escape" && document.body.classList.contains("nav-open")) {
    showNav(false);
  }
});
input?.addEventListener("input", () => find(input.value));
input?.addEventListener("keydown", (event) => {
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    select(event.key === "ArrowDown" ? 1 : -1);
  } else if (event.key === "Enter") {
    event.preventDefault();
    const link = results.children[selected]?.querySelector("a");
    if (link) {
      dialog.close();
      location.href = link.href;
    }
  }
});
dialog?.addEventListener("click", (event) => {
  if (event.target === dialog) dialog.close();
  if (event.target.closest(".results a")) dialog.close();
});
