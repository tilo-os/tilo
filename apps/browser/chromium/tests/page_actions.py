#!/usr/bin/env python3
"""Page reading and operation through the host's `page` op, in real Chromium."""
import base64
import html
import http.server
import json
import math
import os
import re
import sys
import tempfile
import threading
import time
import unittest
from urllib.parse import parse_qs, urlparse
from native_regressions import HANG_BOUND, Host

# The longest settle the host accepts (host.cc's settle validation); a
# longer one is a malformed command and the host closes.
SETTLE_MS = min(int(HANG_BOUND * 1000), 20000)

FORM = b'''<!doctype html><title>Checkout</title>
<style>.visually-hidden{position:absolute;opacity:0;width:1px;height:1px}
#shade{position:fixed;left:0;top:0;width:100%;height:60px;background:#0008}</style>
<header><h1>Fixture shop</h1><nav><a href="/next">Next page</a></nav></header>
<main>
<p>Enter your details.</p>
<form action="/done" method="get">
  <label>Email <input name="email" type="email" required></label>
  <label>Password <input name="password" type="password"></label>
  <label>Country <select name="country"><option value="NL">Netherlands</option><option value="BE">Belgium</option></select></label>
  <input id="terms" name="terms" type="checkbox" class="visually-hidden"><label for="terms">Accept terms</label>
  <label>Search <input name="q" type="search"></label>
  <label>Proof of address <input name="proof" type="file" accept=".pdf"></label>
  <label>Receipts <input name="receipts" type="file" multiple></label>
  <button>Place order</button>
</form>
<button id="covered" style="position:fixed;top:10px;left:10px">Covered</button>
<div id="shade"></div>
<shop-widget></shop-widget>
<p id="status"></p>
</main>
<script>
customElements.define('shop-widget', class extends HTMLElement {
  connectedCallback() {
    const root = this.attachShadow({mode: 'open'});
    root.innerHTML = '<button>Shadow button</button>';
    root.querySelector('button').onclick = () => document.getElementById('status').textContent = 'shadow clicked';
  }
});
document.querySelector('select').addEventListener('change', e => document.getElementById('status').textContent = 'country ' + e.target.value);
for (const field of document.querySelectorAll('input[type=file]'))
  field.addEventListener('change', e => document.getElementById('status').textContent =
    'files ' + [...e.target.files].map(f => f.name + ':' + f.size).join(','));
</script>'''


# File inputs at known places, for a person's own click. Each says what it
# heard, and whether the event was the browser's own.
CHOOSER = b'''<!doctype html><title>Upload</title>
<body style="margin:0">
<label style="position:absolute;left:20px;top:100px">Passport <input id="one" type="file" accept=".pdf,image/*" style="width:300px;height:40px"></label>
<label style="position:absolute;left:20px;top:200px">Receipts <input id="many" type="file" multiple style="width:300px;height:40px"></label>
<label style="position:absolute;left:20px;top:300px">Album <input id="folder" type="file" webkitdirectory style="width:300px;height:40px"></label>
<p id="status" style="position:absolute;left:20px;top:400px"></p>
<p id="blank" style="position:absolute;left:20px;top:460px;width:300px;height:40px"></p>
<script>
window.downs = 0;
window.heard = [];
addEventListener('mousedown', () => window.downs += 1, true);
for (const field of document.querySelectorAll('input'))
  for (const name of ['input', 'change', 'cancel'])
    field.addEventListener(name, e => {
      window.heard.push([field.id, name, e.isTrusted]);
      document.getElementById('status').textContent = field.id + ' ' + name + ' ' +
        [...field.files].map(f => f.name + ':' + f.size).join(',');
    });
</script>'''


AGENT = b'''<!doctype html><title>Agent</title>
<h1>Agent fixture</h1>
<button id="add" onclick="setTimeout(() => { const p = document.createElement('p'); p.textContent = 'added later'; document.body.append(p); }, 1000)">Add later</button>
<button id="log" onclick="console.log('clicked', 7); fetch('/data.json').then(r => r.text()); setTimeout(() => { throw new Error('boom'); }, 0)">Log</button>
<form id="away" action="https://elsewhere.test/collect" method="get"><label>Query <input name="q"></label><button>Send away</button></form>
<label>Pin <input name="pin" type="password"></label>
'''


NATIVE = b'''<!doctype html><title>Native inputs</title>
<h1>Preferences</h1>
<form action="/done" method="get">
  <label>Favourite colour <input name="colour" type="color" value="#000000"></label>
  <label>Proof <input name="proof" type="file"></label>
  <label>Medium <input name="medium" type="color" value="#ff0000"></label>
  <button>Save</button>
</form>
<p id="said"></p>
<script>
for (const field of document.querySelectorAll('input[type=color]'))
  for (const name of ['input', 'change'])
    field.addEventListener(name, e => document.getElementById('said').textContent =
      e.target.name + ' ' + name + ' ' + e.target.value);
</script>'''


# More conditions than one read carries, so the read has to say what it left
# out rather than stopping quietly at its cap.
LONG = ('<!doctype html><title>The long list</title><h1>The long list</h1>' +
        ''.join(f'<p id="c{number}">Condition {number}. You must tell us about any change '
                f'within 30 days, as section {number} of the Act requires.</p>'
                for number in range(1, 2001))).encode()


CANVAS = b'''<!doctype html><title>Sign</title>
<canvas id="c" width="500" height="150" aria-label="Signature pad" style="border:1px solid #000"></canvas>
<button type="button" id="s">Submit signature</button><p id="m"></p>
<script>let ink=0,down=false;const x=document.getElementById("c").getContext("2d");
const c=document.getElementById("c"),marks=[];
const at=e=>{const b=c.getBoundingClientRect();return [Math.round(e.clientX-b.left),Math.round(e.clientY-b.top)]};
c.onpointerdown=e=>{down=true;x.beginPath();x.moveTo(e.offsetX,e.offsetY)};
c.onpointermove=e=>{if(down){x.lineTo(e.offsetX,e.offsetY);x.stroke();ink++;marks.push(at(e))}};
window.onpointerup=()=>down=false;
window.strokes=()=>ink;
window.strokesAt=()=>JSON.stringify(marks);
document.getElementById("s").onclick=()=>{document.getElementById("m").textContent=ink<5?"Draw your signature first":"Signature received"};
</script>'''


GESTURES = b'''<!doctype html><title>Gestures</title>
<style>.menu{position:relative;display:inline-block}
.menu ul{display:none;position:absolute;background:#fff;list-style:none;padding:8px;margin:0}
.menu:hover ul{display:block}
li{border:1px solid #000;padding:8px;margin:4px;list-style:none}
.recast span{padding:6px;display:inline-block}
.recast span:hover{display:block;height:32px;padding:10px;background:#eef}</style>
<h1>Gestures</h1>
<nav class="menu"><span id="trigger" style="padding:8px;display:inline-block;border:1px solid #000">Licences menu</span>
<ul><li><a href="#" onclick="opened(\'fishing\');return false">Fishing licence</a></li></ul></nav>
<p class="recast"><span id="recast">Delivery notes</span></p>
<ol id="l"><li draggable="true">Riverton office</li><li draggable="true">Porthaven office</li><li draggable="true">Central office</li></ol>
<canvas id="c" width="400" height="150" style="border:2px solid #000" aria-label="Signature pad"></canvas>
<button type="button" id="s" onclick="submitSignature()">Submit signature</button>
<button type="button" id="save" onclick="saveOrder()">Save order</button>
<p id="m"></p>
<script>
let ink = 0, down = false;
const pad = document.getElementById('c'), ctx = pad.getContext('2d');
pad.onpointerdown = e => { down = true; ctx.beginPath(); ctx.moveTo(e.offsetX, e.offsetY); };
const marks = [];
pad.onpointermove = e => {
  if (!down) return;
  ctx.lineTo(e.offsetX, e.offsetY); ctx.stroke(); ink++;
  const box = pad.getBoundingClientRect();
  marks.push([Math.round(e.clientX - box.left), Math.round(e.clientY - box.top)]);
};
pad.onpointerup = () => { down = false; };
window.drawn = () => ink;
window.strokesAt = () => JSON.stringify(marks);
window.opened = name => { document.getElementById('m').textContent = 'opened ' + name; };
window.order = () => [...document.getElementById('l').children].map(x => x.textContent).join(' > ');
window.saveOrder = () => { document.getElementById('m').textContent = 'saved ' + order(); };
window.submitSignature = () => { document.getElementById('m').textContent = ink < 5 ? 'Draw your signature first' : 'Signature received'; };
let held;
document.querySelectorAll('#l li').forEach(li => {
  li.ondragstart = () => { held = li; };
  li.ondragover = e => e.preventDefault();
  li.ondrop = e => { e.preventDefault(); if (held !== li) document.getElementById('l').insertBefore(held, li.nextSibling); };
});
</script>'''


PARAGRAPH = ('Reading on a screen is easiest when the line is short, the type is calm and nothing '
             'else competes for attention. ') * 3
ARTICLE = f'''<!doctype html><title>The long read</title>
<style>p {{ color: red; font-size: 9px }}</style>
<nav><a href="/pricing">Pricing</a> <a href="/login">Sign in</a></nav>
<div class="layout"><article><h1>The long read</h1>
<p>{PARAGRAPH}</p><p>{PARAGRAPH}</p><p>{PARAGRAPH}</p>
<aside><a href="/related">Related story</a></aside>
<p>{PARAGRAPH}</p></article>
<div class="sidebar"><a href="/ad">Advertisement</a></div></div>
<input id="kept" value="typed by the person">'''.encode()


LATER = b'''<!doctype html><title>Later</title>
<button onclick="setTimeout(() => location.href = '/next', 1000)">Go later</button>'''
GESTURE = b'''<!doctype html><title>Gestures</title>
<style>
#menu{display:none} #hover:hover #menu{display:block}
li{padding:8px;margin:4px;border:1px solid #000;list-style:none}
</style>
<div id="hover"><span id="label">Licences</span><ul id="menu"><li><a href="/fishing" id="fishing">Fishing licence</a></li></ul></div>
<ol id="rank"><li draggable="true" id="a">Riverton office</li><li draggable="true" id="b">Porthaven office</li><li draggable="true" id="c">Central office</li></ol>
<canvas id="pad" width="300" height="120" aria-label="Signature pad" style="border:1px solid #000"></canvas>
<p id="status"></p>
<script>
const rank = document.getElementById('rank');
let dragged;
rank.querySelectorAll('li').forEach(li => {
  li.ondragstart = () => dragged = li;
  li.ondragover = e => e.preventDefault();
  li.ondrop = e => { e.preventDefault(); if (dragged !== li) rank.insertBefore(dragged, li); };
});
const pad = document.getElementById('pad'), ctx = pad.getContext('2d');
let drawing = false, strokes = 0;
pad.onpointerdown = e => { drawing = true; strokes++; ctx.beginPath(); ctx.moveTo(e.offsetX, e.offsetY); };
pad.onpointermove = e => { if (drawing) ctx.lineTo(e.offsetX, e.offsetY); ctx.stroke(); };
pad.onpointerup = () => { drawing = false; document.getElementById('status').textContent = 'strokes ' + strokes; };
</script>'''


FRAMED = b'''<!doctype html><title>Framed</title>
<h1>Framed page</h1>
<iframe src="/widget" title="Payment form" style="width:600px;height:240px;border:0"></iframe>
<button id="outside">Outside the frame</button>
'''


# A frame holding a frame: the outer one is frame 1, the inner one frame 1.1.
NESTED = b'''<!doctype html><title>Nested</title>
<h1>Nested frames</h1>
<iframe src="/outer" title="Outer frame" style="width:600px;height:400px;border:0"></iframe>
'''


OUTER = b'''<!doctype html><title>Outer</title>
<p>Outer document</p>
<iframe src="/widget" title="Payment form" style="width:560px;height:240px;border:0"></iframe>
'''


LATE = b'''<!doctype html><title>Late frame</title>
<h1>Late page</h1>
<p>The frame is made by script, after the page has loaded.</p>
<script>setTimeout(() => {
  const fr = document.createElement('iframe');
  fr.title = 'Late payment form';
  fr.src = '/widget-late';
  fr.style.cssText = 'width:600px;height:240px;border:0';
  document.body.append(fr);
}, 300);</script>
'''


# The frame's first document moves on to the form by itself, so the frame the
# page attached is navigated again before an action can name it.
WIDGET_LATE = b'''<!doctype html><title>Connecting</title>
<p id="state">Connecting to the payment service</p>
<script>setTimeout(() => location.replace('/widget'), 200);</script>
'''


# A frame the page sets up by script, reaching a document another origin
# serves, which then moves on to its form by itself. The page may hold the
# frame's element but never read its document, so the frame is reached the way
# a person reaches it: through the frame, never through the page around it.
SCRIPTED_FRAME = b'''<!doctype html><title>Scripted frame</title>
<h1>Extension page</h1>
<p id="status">Reading the address book</p>
<button id="outside">Outside the frame</button>
<script>setTimeout(() => {
  const fr = document.createElement('iframe');
  fr.title = 'Address finder';
  fr.src = 'http://localhost' + location.origin.slice(location.origin.lastIndexOf(':')) + '/widget-late';
  fr.style.cssText = 'width:600px;height:240px;border:0';
  document.body.append(fr);
}, 300);</script>
'''


WIDGET = b'''<!doctype html><title>Payment details</title>
<form action="/done" method="get">
  <label>IBAN <input name="iban"></label>
  <label>Account holder <input name="holder"></label>
  <button>Save bank details</button>
</form>
<canvas id="pad" width="200" height="120" style="width:100px;height:60px"></canvas>
<p id="pressed"></p>
<script>
pad.onpointerdown = e => {
  const left = pad.getBoundingClientRect().left, top = pad.getBoundingClientRect().top;
  document.getElementById('pressed').textContent =
    'pressed ' + Math.round(left + e.offsetX) + ',' + Math.round(top + e.offsetY);
};
</script>
'''


RICH = rb'''<!doctype html><title>Rich text</title>
<label id="note-label">Note</label>
<div id="note" contenteditable="true" role="textbox" aria-multiline="true" aria-labelledby="note-label"></div>
<label id="draft-label">Draft</label>
<div id="draft" contenteditable="true" role="textbox" aria-multiline="true" aria-labelledby="draft-label"></div>
<label id="report-label">Report</label>
<div id="report" contenteditable="true" role="textbox" aria-multiline="true" aria-labelledby="report-label"></div>
<label>Steps <textarea id="steps" aria-label="Steps"></textarea></label>
<script>
// What a page hears from a person, as "field:type:what", kept for the test.
window.heard = [];
function hear(element, name) {
  for (const type of ['beforeinput', 'input', 'keydown'])
    element.addEventListener(type, e => heard.push(name + ':' + type + ':' +
      (e.inputType || e.key || '') + (typeof e.data === 'string' ? '=' + e.data : '')));
}
hear(document.getElementById('note'), 'note');
hear(document.getElementById('steps'), 'steps');

// An editor whose own document is the truth: it rebuilds from beforeinput,
// as a page that never reads the DOM it is drawn into does.
const draftEl = document.getElementById('draft');
let draft = '';
function drawDraft() {
  draftEl.innerHTML = draft.split('\n').map(line => '<p>' + (line || '<br>') + '</p>').join('');
  const range = document.createRange(); range.selectNodeContents(draftEl); range.collapse(false);
  const selection = getSelection(); selection.removeAllRanges(); selection.addRange(range);
}
draftEl.addEventListener('beforeinput', e => {
  if (!e.cancelable) return;
  if (e.inputType === 'insertText' && typeof e.data === 'string') { e.preventDefault(); draft += e.data; drawDraft(); }
  else if (e.inputType === 'insertParagraph' || e.inputType === 'insertLineBreak') { e.preventDefault(); draft += '\n'; drawDraft(); }
});
hear(draftEl, 'draft');

// A block editor: each typed line becomes a block of its own.
const reportEl = document.getElementById('report');
const blocks = [];
function drawReport() {
  reportEl.innerHTML = blocks.map(b => '<div>' + (b || '<br>') + '</div>').join('') + '<div><br></div>';
  const range = document.createRange(); range.selectNodeContents(reportEl.lastElementChild); range.collapse(true);
  const selection = getSelection(); selection.removeAllRanges(); selection.addRange(range);
}
drawReport();
reportEl.addEventListener('keydown', e => {
  if (e.key !== 'Enter' || e.isComposing) return;
  e.preventDefault();
  blocks.push(reportEl.textContent); reportEl.textContent = ''; drawReport();
});
hear(reportEl, 'report');
</script>
'''

SCROLL = b'''<!doctype html><title>Long conditions</title>
<style>p{margin:0 0 200px} #lazy{display:block;min-height:200px}
.invoices{height:124px;overflow:auto} .invoices div{height:40px}</style>
<h1>Conditions of carriage</h1>
<button onclick="this.textContent = 'Terms shown'">Show terms</button>
<p id="first">Condition 1. Tickets are not transferable.</p>
<div style="height:1200px">space the page needs before it loads the rest</div>
<section id="lazy"><p>Nothing here yet.</p></section>
<div id="rows" role="region" aria-label="Invoices" class="invoices" tabindex="0"></div>
<script>
// A section that loads once the page is scrolled to it, as a lazy article
// below the fold does.
new IntersectionObserver(entries => {
  for (const entry of entries) if (entry.isIntersecting && !document.getElementById('conditions'))
    document.getElementById('lazy').insertAdjacentHTML('beforeend',
      '<p id="conditions">Condition 14. Bicycles travel at the owner&rsquo;s risk.</p>');
}).observe(document.getElementById('lazy'));
// A virtualized table: rows are rendered only around what is scrolled to.
const rows = document.getElementById('rows'), ROWS = 400, HEIGHT = 40;
const spacer = document.createElement('div');
spacer.style.height = ROWS * HEIGHT + 'px';
spacer.style.position = 'relative';
rows.append(spacer);
function render() {
  const start = Math.floor(rows.scrollTop / HEIGHT), end = Math.min(ROWS, start + 6);
  spacer.innerHTML = '';
  for (let i = start; i < end; i++) {
    const row = document.createElement('div');
    row.style.position = 'absolute';
    row.style.top = i * HEIGHT + 'px';
    row.textContent = 'Invoice ' + (i + 1) + (i === 59 ? ' (overdue)' : '');
    spacer.append(row);
  }
}
rows.onscroll = render;
render();
// What the page was sent, as the page itself sees it: a test can ask which
// key arrived instead of inferring it from what the key did.
window.pressed = [];
document.addEventListener('keydown', event => window.pressed.push(event.key));
</script>'''


# What a person sees on a form, named the way a person reads it, so the same
# description finds the element again after the page has been reloaded.
FINDS = b'''<!doctype html><title>Found</title>
<style>.hidden{position:absolute;opacity:0;width:1px;height:1px}</style>
<h1>Booking</h1>
<form action="/done" method="get">
  <fieldset><legend>Contact</legend>
    <label>Email address <input name="email" type="email"></label>
    <label>Telephone <input name="phone"></label>
  </fieldset>
  <fieldset><legend>Guests</legend>
    <ul id="guests"><li><span>Ada Lovelace</span><button type="button">Remove</button></li>
      <li><span>Grace Hopper</span><button type="button">Remove</button></li></ul>
    <label>Newsletter <input name="letter" type="checkbox"></label>
    <label>Seats <select name="seats"><option>One</option><option>Two</option></select></label>
  </fieldset>
  <button>Place booking</button>
</form>
<label>Notes <textarea name="notes"></textarea></label>
<p><a href="/help">Help with booking</a></p>
<div id="tick"></div>
<script>
// A page whose own document numbers every element by the time it is read:
// nothing an earlier read said can be addressed here.
document.getElementById('tick').textContent = 'ready ' + performance.now();
for (const button of document.querySelectorAll('#guests button'))
  button.onclick = () => button.closest('li').remove();
'''


# A frame's own document, found by the description alone: the caller reads
# nothing of the frame first.
EMBED = b'''<!doctype html><title>Embedded</title>
<iframe src="/embedded-form" title="Card form" style="width:600px;height:240px;border:0"></iframe>
'''


EMBEDDED = b'''<!doctype html><title>Card details</title>
<label>Card number <input name="card"></label>
<label>Security code <input name="cvc"></label>
<button>Save card</button>
'''


# A form as offices write them: questions that share a label inside named
# groups, one answer of several, any answers of several, and a question the
# form asks only once another is answered a certain way.
APPLY = b'''<!doctype html><title>Apply</title>
<h1>Apply for a permit</h1>
<div role="alert"><h2>Check your answers</h2><p>Answer every question.</p></div>
<form action="/done" method="get">
<fieldset><legend>Date of birth</legend><label>Day <input name="dob_day"></label><label>Month <input name="dob_month"></label></fieldset>
<fieldset><legend>Expiry date</legend><label>Day <input name="exp_day"></label><label>Month <input name="exp_month"></label></fieldset>
<fieldset><legend>Refused before?</legend><label><input type="radio" name="refused" value="yes"> Yes</label><label><input type="radio" name="refused" value="no"> No</label></fieldset>
<fieldset><legend>Languages</legend><label><input type="checkbox" name="lang" value="en"> English</label><label><input type="checkbox" name="lang" value="nl"> Dutch</label><label><input type="checkbox" name="lang" value="de"> German</label></fieldset>
<label>Nationality <select name="nationality" id="nat"><option value="">Select a country</option><option>Kenya</option><option>Other</option></select></label>
<div id="other" hidden><label>Which country? <input name="other" required></label></div>
<label>Arrival <input type="date" name="arrival"></label>
<label>Passcode <input type="password" name="code"></label>
<button>Send application</button>
</form>
<script>document.getElementById('nat').addEventListener('change', e => document.getElementById('other').hidden = e.target.value !== 'Other');</script>'''


# Fields that are secret by what they are: a password, and what the page says
# holds a card's number, its security code, a password under another type or
# a one-time code; beside one that is none of them.
SECRETS = b'''<!doctype html><title>Pay</title>
<h1>Pay</h1>
<form action="/done" method="get">
  <label>Card number <input name="card" autocomplete="cc-number"></label>
  <label>Security code <input name="csc" autocomplete="cc-csc"></label>
  <label>Account password <input name="pass" type="password"></label>
  <label>Old password <input name="old" autocomplete="current-password"></label>
  <label>New password <input name="new" autocomplete="section-login new-password"></label>
  <label>Code we sent you <input name="otp" autocomplete="one-time-code"></label>
  <label>Delivery note <input name="note"></label>
  <button>Pay</button>
</form>'''


# A paragraph as an essay writes them: longer than a line or two, with the
# point at its end.
ESSAY = ('<!doctype html><title>Essay</title><article><h1>On reading</h1><p>' +
         ' '.join(f'Sentence {number} of this paragraph says a little more than the one before it did.'
                  for number in range(1, 13)) +
         ' The last sentence is the one that matters: read to the end.</p></article>').encode()


# A page as old sites still serve them: no doctype, so the page scrolls its
# body, and a table for its layout.
OLD = ('<title>Old page</title><table width="600">' +
       ''.join(f'<tr><td height="60">Row {number} of an essay laid out in a table.</td></tr>' for number in range(1, 301)) +
       '</table>').encode()


# An essay as pages without paragraph elements write it: running text in one
# element, its paragraphs parted by two line breaks, one of them far longer
# than a line of the outline carries.
RUN_SENTENCES = [f'Sentence {number} of the long paragraph goes on for a while before it stops.' for number in range(1, 81)]
RUN = ('<title>Run</title><font>First paragraph, a short one.<br><br>' + ' '.join(RUN_SENTENCES) +
       '<br><br>Last paragraph: the point of it all.</font>').encode()


# A site and the other sites it brings in. `{OTHER}` is another site (another
# host and port, so its frames run in a process of their own) and `{NEAR}`
# another origin of this site (the same host on another port).
#
# A news page as sites serve them: a tracker's frame nobody sees, and the
# cookie question in a frame of another site, put there by script after the
# page loaded and taken away once it is answered.
NEWS = b"""<!doctype html><title>The Daily Fixture</title>
<h1>The Daily Fixture</h1>
<p>Rain is expected by the weekend.</p>
<iframe src="{OTHER}/tracker" title="pixel" width="1" height="1" style="border:0"></iframe>
<div id="slot"></div>
<p id="said"></p>
<script>
const slot = document.getElementById('slot');
addEventListener('message', event => {
  if (event.origin !== '{OTHER}') return;
  document.getElementById('said').textContent = 'consent ' + event.data;
  slot.replaceChildren();
});
setTimeout(() => {
  const frame = document.createElement('iframe');
  frame.title = 'Privacy choices';
  frame.src = '{OTHER}/consent';
  frame.style.cssText = 'position:fixed;left:100px;top:200px;width:600px;height:300px;border:2px solid #000;background:#fff';
  slot.append(frame);
}, 100);
</script>"""

CONSENT = b"""<!doctype html><title>Privacy</title>
<div role="dialog" aria-label="We value your privacy">
<h2>We value your privacy</h2>
<p>We and our 87 partners store information on your device.</p>
<button id="yes">Yes, I'm happy</button> <button id="no">No, thank you</button>
<a href="/consent-options">Manage options</a>
</div>
<script>
document.getElementById('yes').onclick = () => parent.postMessage('accepted', '*');
document.getElementById('no').onclick = () => parent.postMessage('refused', '*');
</script>"""

# The cookie question before it is ready: its document moves on to the
# question by itself.
CONSENT_LOADING = b"""<!doctype html><title>Privacy</title>
<p>Loading your choices</p>
<script>setTimeout(() => location.replace('/consent'), 200);</script>"""

# A page whose frames come and go while it loads: the frame that holds the
# cookie question is taken out and another put in its place, and an advert's
# frame arrives before it in the document and leaves again.
CHURN = b"""<!doctype html><title>Churn</title>
<h1>Still loading</h1>
<div id="ads"></div><div id="slot"></div>
<p id="said"></p>
<script>
const ads = document.getElementById('ads'), slot = document.getElementById('slot');
const frame = (title, path) => {
  const made = document.createElement('iframe');
  made.title = title;
  made.src = '{NEAR}' + path;
  made.style.cssText = 'display:block;width:600px;height:200px;border:0';
  return made;
};
addEventListener('message', event => {
  if (event.origin === '{NEAR}') document.getElementById('said').textContent = 'consent ' + event.data;
});
slot.append(frame('Privacy choices', '/consent-loading'));
window.churn = () => { ads.append(frame('Advert', '/advert')); slot.replaceChildren(frame('Privacy choices', '/consent')); return 1; };
window.calm = () => { ads.replaceChildren(); return 1; };
</script>"""

# A page with a field of its own and a form another site serves in a frame.
EMBEDDER = b"""<!doctype html><title>Letters</title>
<h1>Our letters</h1>
<label>Note <input name="note"></label>
<iframe src="{OTHER}/signup" title="Newsletter sign-up" style="width:600px;height:320px;border:0"></iframe>"""

SIGNUP = b"""<!doctype html><title>Sign up</title>
<h2>Newsletter</h2>
<form action="/signed" method="get">
  <label>Email <input name="email" type="email" required></label>
  <label>Edition <select name="edition"><option value="">Choose</option><option>Morning</option><option>Evening</option></select></label>
  <label>Promo code <input name="code" type="password"></label>
  <label><input type="checkbox" name="terms" value="yes"> Accept terms</label>
  <button>Sign up</button>
</form>
<label>Search the letters <input id="q"></label>
<p id="found"></p>
<script>
document.getElementById('q').onkeydown = event => {
  if (event.key === 'Enter') document.getElementById('found').textContent = 'searched ' + event.target.value;
};
</script>"""

# A long document another site serves in a frame, with a form at its end.
TERMS = b"""<!doctype html><title>Terms</title>
<h2>Terms of the letters</h2>
<div style="height:1500px">Scroll for the rest of the terms.</div>
<form action="/agreed" method="get"><label>Initials <input name="initials"></label><button>I agree</button></form>"""

TERMS_HOLDER = b"""<!doctype html><title>Before you read</title>
<h1>Before you read</h1>
<div style="height:120px"></div>
<iframe src="{OTHER}/terms" title="Terms" style="margin-left:150px;width:600px;height:300px;border:0"></iframe>"""

# Another site's page that starts with frames of its own before its content.
BLANKS = b"""<!doctype html><title>Second site</title>
<iframe src="/blank"></iframe><iframe src="/blank"></iframe><iframe src="/blank"></iframe>
<h1>Second site</h1>
<p>The page that loaded.</p>"""

# The gestures page in a frame of another site, set in from the page's corner
# behind a border and padding: a point of the frame is not a point of the page.
GESTURES_HOLDER = b"""<!doctype html><title>Holder</title>
<h1>Holder</h1>
<div style="height:40px"></div>
<iframe src="{OTHER}/gestures" title="Gestures" style="margin-left:60px;width:700px;height:560px;border:3px solid #000;padding:5px"></iframe>"""

# A file input another site serves in a frame.
UPLOAD = b"""<!doctype html><title>Upload</title>
<label>Passport scan <input name="scan" type="file" accept=".pdf"></label>
<p id="status">Nothing chosen</p>
<script>
document.querySelector('input').addEventListener('change', e => document.getElementById('status').textContent =
  'files ' + [...e.target.files].map(f => f.name + ':' + f.size).join(','));
</script>"""

UPLOAD_HOLDER = b"""<!doctype html><title>Documents</title>
<h1>Your documents</h1>
<iframe src="{OTHER}/upload" title="Upload" style="width:600px;height:200px;border:0"></iframe>"""

TWO_SITES = {'/news': NEWS, '/consent': CONSENT, '/consent-loading': CONSENT_LOADING, '/churn': CHURN,
             '/gestures-holder': GESTURES_HOLDER, '/upload': UPLOAD, '/upload-holder': UPLOAD_HOLDER,
             '/embedder': EMBEDDER, '/signup': SIGNUP, '/blanks': BLANKS,
             '/terms': TERMS, '/terms-holder': TERMS_HOLDER,
             '/blank': b'<!doctype html><title>blank</title>'}


# Components as libraries write them: what a person reads is among an
# element's own children, in its shadow tree, or handed from one to the other
# through a slot, and one shadow tree holds another.
SHADOW = b"""<!doctype html><title>Components</title>
<h1>Simple template</h1>
<template id="my-paragraph"><style>p { color: white; background: #666; padding: 5px }</style>
<p><slot name="my-text">My default text</slot></p></template>
<my-paragraph><span slot="my-text">Let's have some different text!</span></my-paragraph>
<my-paragraph><ul slot="my-text"><li>Slotted first item</li><li>In a list!</li></ul></my-paragraph>
<my-paragraph></my-paragraph>
<my-paragraph><span>Never given a slot</span></my-paragraph>
<outer-card><span slot="title">Card title</span><a slot="more" href="/next">Read more</a></outer-card>
<sealed-note></sealed-note>
<slot-button>Save draft</slot-button> <slot-button><span>Send <b>now</b></span></slot-button>
<p id="status"></p>
<script>
customElements.define('slot-button', class extends HTMLElement {
  constructor() {
    super();
    const root = this.attachShadow({mode: 'open'});
    root.innerHTML = '<button type="button" style="padding: 12px 24px"><slot></slot></button>';
    root.querySelector('button').onclick = () =>
      document.getElementById('status').textContent = 'pressed ' + this.textContent;
  }
});
customElements.define('my-paragraph', class extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({mode: 'open'}).appendChild(document.getElementById('my-paragraph').content.cloneNode(true));
  }
});
customElements.define('inner-box', class extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({mode: 'open'}).innerHTML = `<div><slot name="body">Nothing here</slot></div>
      <p><slot name="foot">No footer</slot></p><p><slot name="absent">Inner fallback</slot></p>
      <a href="/inner">Inner link</a>`;
  }
});
customElements.define('outer-card', class extends HTMLElement {
  constructor() {
    super();
    const root = this.attachShadow({mode: 'open'});
    root.innerHTML = `<section><h2><slot name="title">Untitled</slot></h2>
      <inner-box><slot name="more" slot="body">No link given</slot>
        <label slot="body">Member name <input name="member"></label>
        <label slot="body">Passphrase <input name="pass" type="password"></label>
        <label slot="body">Pin <input name="pin" type="password"></label>
        <em slot="foot">The card's own footer</em></inner-box></section>`;
    root.querySelector('[name=member]').addEventListener('input', event =>
      document.getElementById('status').textContent = 'member ' + event.target.value);
  }
});
customElements.define('sealed-note', class extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({mode: 'closed'}).innerHTML = '<p>Sealed words</p>';
  }
});
</script>"""


# Code as documentation shows it: a block whose line breaks and indentation
# are part of what it says, beside prose that names a word of code in passing.
CODE_BLOCK = '/* matches a password input */\ninput[type="password"] {\n  color: red;\n}\n\ninput[pattern] {\n}'
CODE = ('''<!doctype html><title>Snippets</title>
<style>.line { display: block } .numbers { display: none }</style>
<h1>Attribute selectors</h1>
<p>Match a control by its <code>type</code> or its <code>pattern</code>.</p>
<div class="code-example"><div class="example-header"><span class="language-name">css</span></div>
<pre class="brush: css"><code>''' + CODE_BLOCK + '''
</code></pre></div>
<pre><span class="numbers">1 2</span><span class="line">first();</span><span class="line">  second();</span></pre>
<pre><a href="/next">linked()</a> + 1</pre>
<pre>''' + ''.join(f'step({number});\n' for number in range(1, 401)) + '</pre>').encode()


# A page laid out as a documentation site lays its own out: the article's
# element is a part of a grid and has no box of its own (`display: contents`),
# so its header, its list of contents and its body are the grid's own cells.
LAYOUT = b"""<!doctype html><title>The input element</title>
<style>.grid { display: grid; grid-template-columns: 1fr 3fr 1fr; gap: 8px }
.content, .passthrough { display: contents }</style>
<header><nav><a href="/home">Home</a></nav></header>
<div class="grid"><main id="content" class="content">
  <div class="head"><h1>The input element</h1><p>Used to create interactive controls.</p></div>
  <aside><nav><h2>In this article</h2><a href="#attributes">Attributes</a></nav></aside>
  <div class="body"><section aria-labelledby="attributes"><h2 id="attributes">Attributes</h2>
    <p>The pattern attribute is a regular expression.</p>
    <table><tr><th>Type</th><th>Description</th></tr><tr><td>text</td><td>A single-line text field</td></tr></table>
    <div class="passthrough"><p>Words in a box of no box.</p></div>
    <pre><code>input[pattern] {
}</code></pre>
    <details><summary>More on patterns</summary>Folded words<div class="passthrough"><p>Folded paragraph</p>Folded run</div></details>
    <div style="display: none"><div class="passthrough"><p>Never shown</p>nor this</div></div>
    <div hidden><div class="passthrough"><p>Hidden by attribute</p></div></div>
    <div class="passthrough" aria-hidden="true"><p>Hidden from readers</p></div>
    <div style="visibility: hidden"><div class="passthrough"><p>Invisible words</p></div></div>
    <div style="content-visibility: hidden">Skipped run<div class="passthrough"><p>Skipped words</p>skipped too</div></div>
  </section></div>
</main>
<aside id="sidebar"><a href="/other">Another element</a></aside></div>
<footer><p>About this site</p></footer>"""

# A page whose mark-up leaves more text nodes than a read looks at elements:
# the spaces between a long page's elements, here set down by a script.
SPACED = b"""<!doctype html><title>Spaced</title>
<h1>Spaced</h1><div id="gaps"></div><p>The end of the page.</p>
<script>
const gaps = document.getElementById('gaps');
for (let number = 0; number < 45000; number++) gaps.append(' ');
</script>"""

READS = {'/shadow': SHADOW, '/code': CODE, '/layout': LAYOUT, '/spaced': SPACED}


# A shop, from its search to an order's confirmation: the stand-in for a real
# order, which is never placed on a real site. Its bag and its orders are the
# server's own, so a test reads what the shop received.
SHOP_ITEMS = {'walking-boots': ('Walking boots', 129), 'rain-jacket': ('Rain jacket', 89), 'wool-socks': ('Wool socks', 12)}
SHOP_PRODUCT = """<main><h1>{name}</h1><p>EUR {price}</p>
<label>Size <select id="size"><option value="">Choose a size</option><option>41</option><option>42</option><option>43</option><option>44</option></select></label>
<button type="button" id="add" disabled>Add to bag</button>
<p id="said"></p></main>
<script>
const size = document.getElementById('size'), add = document.getElementById('add');
size.onchange = () => { add.disabled = !size.value; };
add.onclick = async () => {
  document.getElementById('said').textContent = 'Adding to your bag';
  await fetch('/shop/add', {method: 'POST', body: new URLSearchParams({sku: '{sku}', size: size.value})});
  location.assign('/shop/bag');
};
</script>"""
SHOP_CHECKOUT = """<h1>Checkout</h1>
<form action="/shop/order" method="post">
<fieldset><legend>Delivery address</legend>
<label>Full name <input name="name" required autocomplete="name"></label>
<label>Street <input name="street" required></label>
<label>City <input name="city" required></label>
<label>Postcode <input name="postcode" required></label>
<label>Country <select name="country" required><option value="">Choose a country</option><option value="NL">Netherlands</option><option value="DE">Germany</option></select></label>
</fieldset>
<fieldset><legend>Delivery</legend>
<label><input type="radio" name="delivery" value="standard" checked> Standard</label>
<label><input type="radio" name="delivery" value="express"> Express</label>
</fieldset>
<fieldset><legend>Payment</legend>
<label>Card number <input name="card" inputmode="numeric" autocomplete="cc-number" required></label>
</fieldset>
<button>Place order</button>
</form>"""


class Handler(http.server.BaseHTTPRequestHandler):
    def answer(self, body, kind='text/html'):
        self.send_response(200)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def shop(self, method):
        state, url = self.server.shop, urlparse(self.path)
        path, query, sent = url.path, parse_qs(url.query), {}
        if method == 'POST':
            body = self.rfile.read(int(self.headers.get('Content-Length', 0))).decode()
            sent = {name: values[0] for name, values in parse_qs(body).items()}
        page = lambda title, body: self.answer((
            f'<!doctype html><title>{title}</title><header><a href="/shop">Fixture Outfitters</a> '
            f'<a href="/shop/bag">Bag ({len(state["bag"])})</a></header>{body}').encode())
        listed = lambda items: ''.join(
            f'<li>{SHOP_ITEMS[item["sku"]][0]}, size {item["size"]}, EUR {SHOP_ITEMS[item["sku"]][1]}</li>' for item in items)
        if path == '/shop':
            return page('Fixture Outfitters', '<h1>Fixture Outfitters</h1><form action="/shop/search" method="get">'
                        '<label>Search the shop <input name="q" type="search" required></label><button>Search</button></form>')
        if path == '/shop/search':
            wanted = query.get('q', [''])[0].lower()
            found = [(sku, name, price) for sku, (name, price) in SHOP_ITEMS.items() if wanted and wanted in name.lower()]
            results = ''.join(f'<li><a href="/shop/p/{sku}">{name}</a> EUR {price}</li>' for sku, name, price in found)
            return page('Search', f'<h1>Results for {html.escape(wanted)}</h1>' +
                        (f'<ul>{results}</ul>' if found else '<p>Nothing found</p>'))
        if path.startswith('/shop/p/') and path.rsplit('/', 1)[1] in SHOP_ITEMS:
            sku = path.rsplit('/', 1)[1]
            name, price = SHOP_ITEMS[sku]
            return page(name, SHOP_PRODUCT.replace('{name}', name).replace('{price}', str(price)).replace('{sku}', sku))
        if path == '/shop/add' and method == 'POST':
            state['bag'].append({'sku': sent['sku'], 'size': sent['size']})
            return self.answer(b'{"ok":true}', 'application/json')
        if path == '/shop/bag':
            total = sum(SHOP_ITEMS[item['sku']][1] for item in state['bag'])
            return page('Your bag', '<h1>Your bag</h1>' + (
                f'<ul>{listed(state["bag"])}</ul><p>Total EUR {total}</p><a href="/shop/checkout">Checkout</a>'
                if state['bag'] else '<p>Your bag is empty</p>'))
        if path == '/shop/checkout':
            return page('Checkout', SHOP_CHECKOUT)
        if path == '/shop/order' and method == 'POST':
            order = dict(sent, items=list(state['bag']), number=1001 + len(state['orders']))
            state['orders'].append(order)
            state['bag'].clear()
            self.send_response(303)
            self.send_header('Location', f'/shop/order/{order["number"]}')
            self.send_header('Content-Length', '0')
            self.end_headers()
            return None
        placed = [order for order in state['orders'] if path == f'/shop/order/{order["number"]}']
        if placed:
            order = placed[0]
            return page('Order confirmed', f'<h1>Order {order["number"]} confirmed</h1>'
                        f'<p>Thank you, {html.escape(order["name"])}.</p><ul>{listed(order["items"])}</ul>'
                        f'<p>Delivered to {html.escape(order["street"])}, {html.escape(order["postcode"])} '
                        f'{html.escape(order["city"])}, {html.escape(order["country"])} by {html.escape(order["delivery"])} delivery.</p>'
                        f'<p>Paid with the card ending {html.escape(order["card"][-4:])}.</p>')
        self.send_response(404)
        self.send_header('Content-Length', '0')
        self.end_headers()
        return None

    def do_POST(self):
        return self.shop('POST')

    def do_GET(self):
        path = urlparse(self.path).path
        if path.startswith('/shop'):
            return self.shop('GET')
        if path in READS:
            return self.answer(READS[path])
        if path in TWO_SITES:
            return self.answer(TWO_SITES[path].replace(b'{OTHER}', self.server.other.encode())
                               .replace(b'{NEAR}', self.server.near.encode()))
        # What the sign-up form of another site's frame sent.
        if path == '/signed':
            self.server.signed = parse_qs(urlparse(self.path).query)
        body = FORM if path == '/form' else AGENT if path == '/agent' else ARTICLE if path == '/article' \
            else APPLY if path == '/apply' else SECRETS if path == '/secrets' else ESSAY if path == '/essay' else OLD if path == '/old' else RUN if path == '/run' \
            else LATER if path == '/later' \
            else GESTURE if path == '/gesture' \
            else CANVAS if path == '/canvas' \
            else GESTURES if path == '/gestures' \
            else FRAMED if path == '/framed' else WIDGET if path == '/widget' \
            else LATE if path == '/late' else WIDGET_LATE if path == '/widget-late' \
            else SCRIPTED_FRAME if path == '/scripted-frame' \
            else NESTED if path == '/nested' else OUTER if path == '/outer' \
            else RICH if path == '/rich' \
            else SCROLL if path == '/scroll' \
            else NATIVE if path == '/native' else LONG if path == '/long' \
            else CHOOSER if path == '/chooser' \
            else FINDS if path == '/finds' else EMBED if path == '/embed' \
            else EMBEDDED if path == '/embedded-form' \
            else b'{"ok":true}' if path == '/data.json' \
            else f'<title>{path}</title><p>{path}</p>'.encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json' if path == '/data.json' else 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class PageActions(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Two servers on two loopback ports: the page's own site, and what it
        # brings in from elsewhere. Under another host name the second is
        # another site, whose frames run in a process of their own; under the
        # same one it is another origin of the page's site.
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        cls.other = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        for server, peer in ((cls.server, cls.other), (cls.other, cls.server)):
            server.other = f'http://localhost:{peer.server_port}'
            server.near = f'http://127.0.0.1:{peer.server_port}'
            server.shop = {'bag': [], 'orders': []}
            server.signed = None
            threading.Thread(target=server.serve_forever, daemon=True).start()
        cls.url = f'http://127.0.0.1:{cls.server.server_port}'

    @classmethod
    def tearDownClass(cls):
        for server in (cls.server, cls.other):
            server.shutdown()
            server.server_close()

    def setUp(self):
        self.h = Host()
        self.addCleanup(self.h.close)
        self.h.send(op='visibility', visible=True)
        self.h.send(op='resize', width=1000, height=800, scale_factor=1.0)
        self.request = 0

    def page(self, action, **fields):
        return self.asked(action, **fields)

    def asked(self, action, **fields):
        # What the engine says to one request, whether it answers or refuses.
        self.request += 1
        request = self.request
        self.h.send(op='page', request_id=request, action=action, **fields)
        return self.h.until(lambda e: e['event'] == 'page_result' and e['request_id'] == request)

    def ok(self, action, **fields):
        result = self.page(action, **fields)
        self.assertTrue(result['ok'], result)
        return result

    def outline(self):
        return self.ok('outline')['outline']

    def reads(self, wanted, what, **fields):
        # The read that says `wanted`: a page still putting itself together,
        # or a frame between two documents, is read again until it does.
        deadline = time.monotonic() + HANG_BOUND
        while True:
            read = self.page('outline', **fields)
            if read['ok'] and wanted in read['outline']:
                return read
            self.assertLess(time.monotonic(), deadline, f'{what}: {read}')
            time.sleep(0.1)

    def begin(self, action, **fields):
        # A request sent without waiting for its answer, as a second actor's
        # is while the first's still runs.
        self.request += 1
        self.h.send(op='page', request_id=self.request, action=action, **fields)
        return self.request

    def answers(self, *requests):
        # The answers to `requests`, in the order the engine gave them.
        wanted, got = set(requests), []
        while wanted:
            event = self.h.until(lambda e: e['event'] == 'page_result' and e['request_id'] in wanted)
            wanted.discard(event['request_id'])
            got.append(event)
        return got

    def typed(self, text):
        # What a person types: keys straight to the page, and no page action.
        for char in text:
            for down in (True, False):
                self.h.send(op='key', key=char, down=down, ctrl=False, shift=False, alt=False, meta=False)

    def person_clicks(self, field):
        # A person's own click on the element `field`: the pointer straight
        # to the page, and no page action. A click before the page first
        # paints lands nowhere, so it is made again until the page heard one;
        # a click the page heard is never made twice, as each opens a chooser.
        box = json.loads(self.main_world(
            'JSON.stringify(document.getElementById("%s").getBoundingClientRect())' % field))
        x, y = box['x'] + 40, box['y'] + box['height'] / 2
        deadline = time.monotonic() + HANG_BOUND
        while True:
            before = self.main_world('window.downs')
            for down in (True, False):
                self.h.send(op='mouse_button', button='left', down=down, x=x, y=y)
            heard = time.monotonic() + 3
            while time.monotonic() < heard:
                self.h.settle(0.2)
                if self.main_world('window.downs') > before:
                    return
            self.assertLess(time.monotonic(), deadline, 'the page never heard a click')

    def chooser(self, field):
        # The chooser a person's click on `field` opens, as the embedder is
        # told of it.
        after = len(self.h.seen)
        self.person_clicks(field)
        asked = self.told('file_chooser', after)
        self.assertGreater(asked['id'], 0, asked)
        return asked

    def answered(self, asked, paths):
        # Answer the chooser `asked` and see the page stop waiting on it.
        after = len(self.h.seen)
        self.h.send(op='file_chooser_reply', id=asked['id'], paths=paths)
        self.told('file_chooser_closed', after, lambda event: event['id'] == asked['id'])

    def status(self, wanted):
        # What the page last heard of its file inputs, once it is `wanted`.
        deadline = time.monotonic() + HANG_BOUND
        while True:
            said = self.main_world('document.getElementById("status").textContent')
            if said == wanted:
                return
            self.assertLess(time.monotonic(), deadline, f'the page says {said!r}, not {wanted!r}')
            time.sleep(0.1)

    def told(self, name, after, when=lambda event: True):
        # What the engine said of its own accord, `name`, since the place
        # `after` in what it has said, or says next.
        for event in self.h.seen[after:]:
            if event.get('event') == name and when(event):
                return event
        return self.h.until(lambda event: event.get('event') == name and when(event))

    def ref(self, outline, pattern):
        found = re.search(pattern + r'[^\n]*\[ref=(d\d+e\d+)\]', outline) or re.search(
            re.escape(pattern) + r'(?:[^\n]*\n)*?[^\n]*\[ref=(d\d+e\d+)\]', outline)
        self.assertIsNotNone(found, f'{pattern!r} not in outline:\n{outline}')
        return found.group(1)

    def _disabled(self, outline, pattern):
        for line in outline.splitlines():
            found = re.search(r'\[ref=(d\d+e\d+)\]', line) if re.search(pattern, line) else None
            if found:
                return found.group(1)
        self.fail(f'{pattern!r} not in outline:\n{outline}')

    def near(self, pattern):
        # A block holding text is not focusable, so a text line carries no ref:
        # what a caller scrolls to is the region around it.
        lines = self.outline().splitlines()
        index = [i for i, text in enumerate(lines) if re.search(pattern, text)]
        self.assertTrue(index, f'{pattern!r} not in outline:\n' + '\n'.join(lines))
        regions = [line for line in lines[:index[0] + 1] if re.search(r'\[ref=d\d+e\d+\]', line)]
        self.assertTrue(regions, f'no region before {pattern!r} in outline:\n' + '\n'.join(lines))
        return re.search(r'\[ref=(d\d+e\d+)\]', regions[-1]).group(1)

    def main_world(self, expression):
        # An answer is known by its own nonce: a title the page wrote for an
        # earlier question may still be on its way.
        nonce = str(time.monotonic_ns())
        self.h.send(op='scripts', scripts=['document.title=JSON.stringify({value:(' + expression + '),nonce:' + nonce + '})'])
        answered = self.h.until(lambda e: e['event'] == 'title' and self._answer(e['title']) == nonce)
        self.h.send(op='scripts', scripts=[])
        return json.loads(re.sub(r',"nonce":\d+}$', '}', answered['title']))['value']

    def _blocks(self, element):
        # What a region holds, a line to each block: an offscreen page lays
        # nothing out, so innerText would run the blocks into one.
        return json.loads(self.main_world(
            'JSON.stringify(Array.from(document.getElementById("%s").children)'
            '.map(b => b.textContent).filter(t => t))' % element))

    @staticmethod
    def _answer(title):
        # The nonce of a value the page wrote into its title, if it is one:
        # the text it carries may hold what JSON would escape.
        found = re.search(r'"nonce":(\d+)}$', title) if title.startswith('{') else None
        return found.group(1) if found else None

    def test_two_actors_in_one_tab_each_read_their_own_changes(self):
        # Two agents share a tab. What one reads must not take what changed
        # away from the other: each is told what changed since its own read.
        self.h.navigate(self.url + '/form')
        first = self.ok('outline', actor='ada')['outline']
        self.ok('outline', actor='ben')
        email = self.ref(first, 'textbox "Email"')
        self.ok('fill', element=email, text='ada@example.org', actor='ada')
        seen_by_ben = self.ok('changes', actor='ben')
        self.assertIn('+', seen_by_ben['outline'])
        self.assertIn('ada@example.org', seen_by_ben['outline'])
        # Ben's read is Ben's: Ada has not read since her fill, so she is
        # still told of it, where one shared baseline said nothing changed.
        seen_by_ada = self.ok('changes', actor='ada')
        self.assertIn('ada@example.org', seen_by_ada['outline'], seen_by_ada)
        self.assertEqual(self.ok('changes', actor='ada')['message'], 'Nothing changed')
        # A caller that names no actor keeps the one baseline it always had.
        self.assertTrue(self.ok('changes')['message'].startswith('First read'))
        self.assertEqual(self.ok('changes')['message'], 'Nothing changed')

    def test_an_action_behind_another_waits_its_turn_and_runs_in_order(self):
        # A second actor's action arriving while the first's still runs is
        # not refused: it runs after it, and each is answered as its own.
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        email, search = self.ref(outline, 'textbox "Email"'), self.ref(outline, 'searchbox "Search"')
        waiting = self.begin('settle', quiet_ms=1500, timeout_ms=6000, actor='ada')
        filled = self.begin('fill', element=email, text='ben@example.org', actor='ben')
        searched = self.begin('fill', element=search, text='kettle', actor='cy')
        first, second, third = self.answers(waiting, filled, searched)
        self.assertEqual([first['request_id'], second['request_id'], third['request_id']], [waiting, filled, searched])
        self.assertTrue(first['ok'] and second['ok'] and third['ok'], (first, second, third))
        outline = self.outline()
        self.assertIn('value="ben@example.org"', outline)
        self.assertIn('value="kettle"', outline)

    def test_a_waiting_action_cancelled_never_runs_and_the_running_one_goes_on(self):
        self.h.navigate(self.url + '/form')
        email = self.ref(self.outline(), 'textbox "Email"')
        running = self.begin('settle', quiet_ms=1200, timeout_ms=6000, actor='ada')
        waiting = self.begin('fill', element=email, text='never@example.org', actor='ben')
        self.h.send(op='page_cancel', request_id=waiting)
        cancelled, settled = self.answers(waiting, running)
        self.assertEqual(cancelled['request_id'], waiting)
        self.assertFalse(cancelled['ok'])
        self.assertEqual(cancelled['code'], 'cancelled')
        self.assertIn('changed nothing', cancelled['message'])
        # The cancel named Ben's request: Ada's ran to its end.
        self.assertEqual(settled['request_id'], running)
        self.assertTrue(settled['ok'], settled)
        self.assertNotIn('never@example.org', self.outline())

    def test_the_tab_keeps_a_record_of_who_did_what(self):
        # Whoever looks at a tab several actors worked in can read what each
        # did, by the description that still names a field after its page is
        # gone, with what was written and never a secret.
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        email, password = self.ref(outline, 'textbox "Email"'), self.ref(outline, 'textbox "Password"')
        country = self.ref(outline, 'combobox "Country"')
        self.ok('fill', element=email, text='ada@example.org', actor='ada')
        self.ok('fill', element=password, text='hunter2-secret', secret=True, actor='ada')
        chosen = self.ok('select', element=country, values=['Belgium'], actor='ben')
        # An answer says where its action stands in the record.
        self.assertEqual(chosen['seq'], 3)
        refused = self.page('fill', element=country, text='x', actor='ben')
        self.assertFalse(refused['ok'])
        record = self.ok('ledger', since=0)
        entries = record['data']['entries']
        self.assertEqual([(e['seq'], e['actor'], e['action'], e['ok']) for e in entries],
                         [(1, 'ada', 'fill', True), (2, 'ada', 'fill', True), (3, 'ben', 'select', True), (4, 'ben', 'fill', False)])
        self.assertEqual(entries[0]['target'], {'role': 'textbox', 'name': 'Email'})
        self.assertEqual(entries[0]['text'], 'ada@example.org')
        self.assertEqual(entries[0]['url'], self.url + '/form')
        # The secret is in the page and nowhere in its record.
        self.assertNotIn('text', entries[1])
        self.assertNotIn('hunter2', json.dumps(record))
        self.assertEqual(entries[2]['values'], ['Belgium'])
        self.assertEqual(entries[3]['code'], 'not_editable')
        self.assertNotIn('text', entries[3])
        # The description the record keeps names the field again later.
        found = self.ok('find', **entries[0]['target'])
        self.assertEqual(found['data']['element'], email)
        # A reader asks for what came after the place it has read to.
        later = self.ok('ledger', since=record['data']['next'])
        self.assertEqual(later['data']['entries'], [])
        self.assertEqual(self.ok('ledger', since=2)['data']['entries'][0]['seq'], 3)
        # Reading is not acting: no read took a place in the record.
        self.assertEqual(record['data']['next'], 4)

    def test_the_record_answers_while_an_action_runs(self):
        self.h.navigate(self.url + '/form')
        email = self.ref(self.outline(), 'textbox "Email"')
        self.ok('fill', element=email, text='ada@example.org', actor='ada')
        running = self.begin('settle', quiet_ms=1200, timeout_ms=6000, actor='ada')
        asked = self.begin('ledger', since=0, actor='rev')
        first = self.h.until(lambda e: e['event'] == 'page_result' and e['request_id'] in (running, asked))
        self.assertEqual(first['request_id'], asked, 'the record waits on nothing the page is doing')
        self.assertEqual(len(first['data']['entries']), 1)
        self.answers(running)

    def test_an_actor_is_told_what_the_others_did_since_it_last_heard(self):
        # The page an agent acts on next may not be the page it read. Its
        # next answer says what the others did meanwhile, once.
        self.h.navigate(self.url + '/form')
        outline = self.ok('outline', actor='ada')['outline']
        email, search = self.ref(outline, 'textbox "Email"'), self.ref(outline, 'searchbox "Search"')
        self.ok('fill', element=email, text='ben@example.org', actor='ben')
        self.ok('fill', element=search, text='lamp', actor='ben')
        mine = self.ok('fill', element=search, text='kettle', actor='ada')
        self.assertEqual([(o['actor'], o['action'], o['target']['name']) for o in mine['others']],
                         [('ben', 'fill', 'Email'), ('ben', 'fill', 'Search')])
        # Told once: nothing more came from Ben since.
        self.assertNotIn('others', self.ok('outline', actor='ada'))
        # Ben in turn hears of Ada's fill, and never of his own.
        theirs = self.ok('changes', actor='ben')
        self.assertEqual([(o['actor'], o['text']) for o in theirs['others']], [('ada', 'kettle')])
        # A caller with no name is told nothing of others, as before.
        self.assertNotIn('others', self.ok('outline'))

    def test_a_review_reads_every_field_with_who_set_it(self):
        # A form several actors filled is reviewed field by field: what each
        # holds now, who set it, and whether anything changed it afterwards.
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        email, search = self.ref(outline, 'textbox "Email"'), self.ref(outline, 'searchbox "Search"')
        country, terms = self.ref(outline, 'combobox "Country"'), self.ref(outline, 'checkbox "Accept terms"')
        password = self.ref(outline, 'textbox "Password"')
        self.ok('fill', element=email, text='ada@example.org', actor='ada')
        self.ok('select', element=country, values=['Belgium'], actor='ben')
        self.ok('click', element=terms, actor='ben')
        self.ok('fill', element=password, text='hunter2-secret', secret=True, actor='ada')
        self.ok('fill', element=search, text='kettle', actor='ada')
        # The page's own script rewrites one answer behind everyone's back.
        self.main_world('(document.querySelector("[name=q]").value = "toaster", 1)')
        review = self.ok('fields', actor='rev')
        fields = {field['name']: field for field in review['data']['fields']}
        self.assertEqual(fields['Email']['value'], 'ada@example.org')
        self.assertEqual(fields['Email']['set'], {'how': 'action', 'by': 'ada', 'seq': 1})
        self.assertTrue(fields['Email']['required'])
        self.assertEqual(fields['Country']['value'], 'Belgium')
        self.assertEqual(fields['Country']['set'], {'how': 'action', 'by': 'ben', 'seq': 2})
        self.assertEqual(fields['Accept terms']['value'], 'checked')
        self.assertEqual(fields['Accept terms']['set'], {'how': 'action', 'by': 'ben', 'seq': 3})
        # A secret is reviewed by who set it, never by what it is.
        self.assertEqual(fields['Password']['value'], '<14 characters hidden>')
        self.assertEqual(fields['Password']['set']['by'], 'ada')
        self.assertNotIn('hunter2', json.dumps(review))
        # What the field holds now is the truth, and the review says it is no
        # longer what Ada wrote.
        self.assertEqual(fields['Search']['value'], 'toaster')
        self.assertEqual(fields['Search']['set'], {'how': 'action', 'by': 'ada', 'seq': 5, 'changed': True})
        self.assertIn('changed after it was set by "ada", action 5', review['outline'])
        # A field nobody touched says so.
        self.assertNotIn('set', fields['Proof of address'])
        self.assertIn('1 changed after they were set', review['message'])
        # Reviewing changed nothing and took no place in the record.
        self.assertEqual(self.ok('ledger', since=0)['data']['next'], 5)

    def test_typing_into_the_page_is_reviewed_as_direct_input(self):
        # A person at the page is not an action: what they type is told apart
        # from what any agent's action wrote.
        self.h.navigate(self.url + '/form')
        outline = self.ok('outline', actor='ada')['outline']
        email, search = self.ref(outline, 'textbox "Email"'), self.ref(outline, 'searchbox "Search"')
        self.ok('fill', element=email, text='ada@example.org', actor='ada')
        time.sleep(2)
        self.main_world('(document.querySelector("[name=q]").focus(), 1)')
        for char in 'lamp':
            for down in (True, False):
                self.h.send(op='key', key=char, down=down, ctrl=False, shift=False, alt=False, meta=False)
        self.h.settle(1)
        fields = {field['name']: field for field in self.ok('fields')['data']['fields']}
        self.assertEqual(fields['Search']['value'], 'lamp')
        self.assertEqual(fields['Search']['set'], {'how': 'input'})
        self.assertEqual(fields['Email']['set'], {'how': 'action', 'by': 'ada', 'seq': 1})

    def test_a_page_s_forms_are_read_as_the_questions_they_ask(self):
        # A form is what a page asks of whoever fills it in. Read that way,
        # each question has a name, a kind and the answers it offers, and the
        # form is known again by what it asks.
        self.h.navigate(self.url + '/apply')
        read = self.ok('forms')
        self.assertEqual(len(read['data']['forms']), 1)
        form = read['data']['forms'][0]
        # The form is named by the heading it stands under, not by the
        # heading of what the page tells its reader in between.
        self.assertEqual((form['form'], form['label'], form['method'], form['action']),
                         ('form1', 'Apply for a permit', 'GET', '/done'))
        asked = {field['name']: field for field in form['fields']}
        # A label two questions share is said with the group each is in.
        self.assertEqual(list(asked), ['Date of birth: Day', 'Date of birth: Month', 'Expiry date: Day',
                                       'Expiry date: Month', 'Refused before?', 'Languages', 'Nationality',
                                       'Arrival', 'Passcode'])
        self.assertEqual((asked['Refused before?']['kind'], asked['Refused before?']['choices']), ('choice', ['Yes', 'No']))
        self.assertEqual((asked['Languages']['kind'], asked['Languages']['choices']), ('choices', ['English', 'Dutch', 'German']))
        # The option that stands for no answer is not one of the answers.
        self.assertEqual((asked['Nationality']['kind'], asked['Nationality']['choices']), ('choice', ['Kenya', 'Other']))
        self.assertEqual(asked['Arrival']['kind'], 'date')
        self.assertEqual(asked['Passcode']['kind'], 'secret')
        self.assertEqual(form['submit'], ['Send application'])
        self.assertIn('- form1 "Apply for a permit" (GET /done) signature=' + form['signature'], read['outline'])
        # An answer that is a control of its own carries the ref its own
        # action takes; a select's options are not controls and carry none.
        self.assertRegex(read['outline'], r'  - "Refused before\?": one of "Yes" \[ref=d\d+e\d+\], "No" \[ref=d\d+e\d+\]')
        self.assertIn('  - "Nationality": one of "Kenya", "Other", [ref=', read['outline'])
        # A question the form does not ask yet is not listed; the same form
        # read again on a later visit is the same form.
        self.assertNotIn('Which country?', asked)
        self.h.navigate(self.url + '/apply')
        self.assertEqual(self.ok('forms')['data']['forms'][0]['signature'], form['signature'])
        # Reading asked the page nothing and is no part of the tab's record.
        self.assertEqual(self.ok('ledger', since=0)['data']['entries'], [])

    def test_a_form_is_answered_by_its_questions_names_and_sent(self):
        self.h.navigate(self.url + '/apply')
        signature = self.ok('forms')['data']['forms'][0]['signature']
        answers = {'Date of birth: Day': '9', 'Date of birth: Month': 11, 'Expiry date: Day': '14',
                   'Expiry date: Month': '3', 'Refused before?': 'no', 'Languages': ['English', 'German'],
                   'Nationality': 'Other', 'Arrival': '2027-02-03'}
        first = self.ok('fill_form', form='form1', answers=answers, submit=True, actor='ada')
        # Every answer was placed, and answering opened a question the form
        # had not asked: it is said, and the form is not sent without it.
        self.assertEqual([result['ok'] for result in first['data']['results']], [True] * 8)
        self.assertFalse(first['data']['submitted'])
        self.assertEqual(first['data']['now_asks'], ['Which country?'])
        self.assertEqual(first['data']['unanswered'], ['Which country?'])
        self.assertIn('the form now also asks "Which country?"', first['message'])
        self.assertEqual(first['navigation'], 'possible')
        # The form asks something new now, so it is no longer the form that
        # signature named: a caller holding the old one is told, not obeyed.
        # The answer says what the form became, by the signature a read of
        # it gives now, beside the one it was answered under.
        self.assertEqual(first['data']['signature'], signature)
        now = self.ok('forms')['data']['forms'][0]
        self.assertNotEqual(now['signature'], signature)
        self.assertEqual(first['data']['became'], {'signature': now['signature'], 'offers': now['offers']})
        stale = self.page('fill_form', form=signature, answers={'Which country?': 'Portugal'}, submit=True, actor='ada')
        self.assertFalse(stale['ok'])
        # The page has forms and none is the one named: not the refusal of
        # a page that has no form at all.
        self.assertEqual(stale['code'], 'form_changed')
        sent = self.ok('fill_form', form='form1', answers={'Which country?': 'Portugal'}, submit='Send application', actor='ada')
        self.assertTrue(sent['data']['submitted'])
        self.assertEqual(sent['navigation'], 'started')
        query = parse_qs(urlparse(sent['url']).query)
        self.assertEqual(urlparse(sent['url']).path, '/done')
        self.assertEqual(query, {'dob_day': ['9'], 'dob_month': ['11'], 'exp_day': ['14'], 'exp_month': ['3'],
                                 'refused': ['no'], 'lang': ['en', 'de'], 'nationality': ['Other'],
                                 'other': ['Portugal'], 'arrival': ['2027-02-03']})
        # The tab's record holds each answer by the name it was asked under.
        entries = self.ok('ledger', since=0)['data']['entries']
        self.assertEqual([(entry['actor'], entry['action'], entry['ok']) for entry in entries],
                         [('ada', 'fill_form', True), ('ada', 'fill_form', False), ('ada', 'fill_form', True)])
        self.assertIn({'name': 'Languages', 'value': 'English, German'}, entries[0]['answers'])
        self.assertEqual(entries[2]['answers'], [{'name': 'Which country?', 'value': 'Portugal'}])
        self.assertEqual(entries[2]['to_url'], sent['url'])
        # The page the form led to asks nothing, which is another refusal.
        nothing = self.page('fill_form', form=signature, answers={'Which country?': 'Portugal'})
        self.assertEqual((nothing['ok'], nothing['code'], nothing['message']), (False, 'no_form', 'This page has no form'))

    def test_a_form_says_when_the_answers_a_choice_offers_change(self):
        # A list of answers that is another list later is the same form
        # asking the same questions: its signature stays, and the value
        # beside it says the answers it offers are others.
        self.h.navigate(self.url + '/apply')
        before = self.ok('forms')['data']['forms'][0]
        self.assertRegex(before['offers'], r'^[0-9a-f]{8}$')
        self.assertIn(f'signature={before["signature"]} offers={before["offers"]}', self.ok('forms')['outline'])
        self.main_world('(document.getElementById("nat").add(new Option("Chile")), 1)')
        after = self.ok('forms')['data']['forms'][0]
        self.assertEqual(after['signature'], before['signature'])
        self.assertNotEqual(after['offers'], before['offers'])
        asked = {field['name']: field for field in after['fields']}
        self.assertEqual(asked['Nationality']['choices'], ['Kenya', 'Other', 'Chile'])
        # An answer names the form by its signature still, and is told what
        # the form offers now.
        answered = self.ok('fill_form', form=before['signature'], answers={'Nationality': 'Chile'})
        self.assertEqual(answered['data']['became'], {'signature': before['signature'], 'offers': after['offers']})

    def test_answers_are_placed_in_the_form_s_order_so_one_can_open_the_next(self):
        # A caller gives a form's answers together. The form takes them in
        # the order it asks, so the answer that opens a question is placed
        # before the answer to it, whatever order they were given in.
        self.h.navigate(self.url + '/apply')
        sent = self.ok('fill_form', form='form1', submit=True, answers={
            'Which country?': 'Portugal', 'Arrival': '2027-02-03', 'Nationality': 'Other'})
        self.assertEqual([(result['name'], result['ok']) for result in sent['data']['results']],
                         [('Nationality', True), ('Which country?', True), ('Arrival', True)])
        self.assertTrue(sent['data']['submitted'], sent)
        query = parse_qs(urlparse(sent['url']).query)
        self.assertEqual((query['nationality'], query['other'], query['arrival']), (['Other'], ['Portugal'], ['2027-02-03']))

    def test_an_answer_that_opens_a_question_says_so(self):
        # Choosing "Other" makes the form ask which country. The action that
        # chose it says so, so its caller does not go on without reading it.
        self.h.navigate(self.url + '/apply')
        nationality = self.ref(self.outline(), 'combobox "Nationality"')
        chosen = self.ok('select', element=nationality, values=['Other'])
        self.assertEqual(chosen['message'], 'Selected "Other" in combobox "Nationality"; the form now also asks "Which country?"')
        # An answer that opens nothing says nothing more.
        again = self.ok('select', element=nationality, values=['Other'])
        self.assertEqual(again['message'], 'Selected "Other" in combobox "Nationality"')
        country = self.ref(self.outline(), 'textbox "Which country\\?"')
        self.assertEqual(self.ok('fill', element=country, text='Portugal')['message'], 'Filled textbox "Which country?"')

    def test_a_form_says_which_answers_it_could_not_place(self):
        # What a form cannot take by name is said answer by answer, with the
        # ref the field's own action takes, and nothing is sent half answered.
        self.h.navigate(self.url + '/apply')
        answered = self.ok('fill_form', form='form1', submit=True, actor='ada', answers={
            'Arrival': '2027-02-03', 'Nationality': 'Atlantis', 'Passcode': 'hunter2-secret',
            'Shoe size': '44', 'refused before?': 'Yes'})
        results = {result['name']: result for result in answered['data']['results']}
        self.assertTrue(results['Arrival']['ok'])
        # A name is matched whatever its letter case when only one is like it.
        self.assertTrue(results['Refused before?']['ok'])
        self.assertEqual(results['Nationality']['code'], 'no_option')
        self.assertIn('"Kenya", "Other"', results['Nationality']['message'])
        self.assertEqual(results['Passcode']['code'], 'secret_field')
        self.assertRegex(results['Passcode']['element'], r'^d\d+e\d+$')
        self.assertEqual(results['Shoe size']['code'], 'no_field')
        self.assertFalse(answered['data']['submitted'])
        self.assertIn('not sent, since not every answer was placed', answered['message'])
        # The secret was not placed, and is nowhere in the answer or the record.
        self.assertNotIn('hunter2', json.dumps(answered))
        self.assertNotIn('hunter2', json.dumps(self.ok('ledger', since=0)))
        self.assertEqual(urlparse(answered['url']).path, '/apply')
        # The fields it did place are reviewed as set by its actor.
        fields = {field['name']: field for field in self.ok('fields')['data']['fields']}
        self.assertEqual(fields['Arrival']['set'], {'how': 'action', 'by': 'ada', 'seq': 1})
        self.assertEqual(fields['Yes']['set'], {'how': 'action', 'by': 'ada', 'seq': 1})
        # A review names a field by the question the form asks, the name an
        # answer was given under: the two speak of one field one way.
        self.assertEqual((fields['Yes']['form'], fields['Yes']['question'], fields['Yes']['answer']),
                         ('form1', 'Refused before?', 'Yes'))
        self.assertEqual(fields['Arrival']['question'], 'Arrival')
        days = [field['question'] for field in self.ok('fields')['data']['fields'] if field['name'] == 'Day']
        self.assertEqual(days, ['Date of birth: Day', 'Expiry date: Day'])
        self.assertIn('- "Refused before?": ', self.ok('fields')['outline'])
        # A call that places nothing is a failure, and says of what.
        nothing = self.page('fill_form', form='form1', answers={'Shoe size': '44'}, actor='ada')
        self.assertFalse(nothing['ok'])
        self.assertEqual(nothing['code'], 'no_field')
        self.assertFalse(self.page('fill_form', form='form9', answers={'Arrival': '2027-02-03'})['ok'])

    def test_no_secret_is_in_the_record_whatever_wrote_it(self):
        # What is written into a field that is secret by its nature is in
        # the page and nowhere else: not in the tab's record, in no answer
        # and in no note to another actor, whichever action wrote it.
        self.h.navigate(self.url + '/secrets')
        said = []

        def heard(answer):
            said.append(answer)
            return answer
        outline = heard(self.ok('outline', actor='ada'))['outline']
        filled = {'Card number': 'card-4111111111111111', 'Security code': 'csc-737-x', 'Old password': 'correct-horse',
                  'New password': 'battery-staple', 'Code we sent you': 'otp-918273-x'}
        for name, text in filled.items():
            field = self.ref(outline, f'textbox "{name}"')
            # Consent is asked for by what the field is, and the record keeps
            # nothing of the text by what the field is.
            refused = heard(self.page('fill', element=field, text=text, actor='ada'))
            self.assertEqual((refused['ok'], refused['code']), (False, 'secret_field'), refused)
            heard(self.ok('fill', element=field, text=text, secret=True, actor='ada'))
        # Typed key by key: into the field a press names, then into the
        # field that has the keyboard.
        keys = dict(ctrl=False, shift=False, alt=False, meta=False)
        password = self.ref(outline, 'textbox "Account password"')
        for char in 'hunter2':
            pressed = heard(self.ok('press', element=password, key=char, actor='ben', **keys))
            self.assertEqual(pressed['message'], 'Pressed a key in a secret field', pressed)
        for char in '-secret':
            pressed = heard(self.ok('press', key=char, actor='ben', **keys))
            self.assertEqual(pressed['message'], 'Pressed a key in a secret field', pressed)
        # A key that writes nothing is named, and so is one written elsewhere.
        self.assertEqual(heard(self.ok('press', key='End', actor='ben', **keys))['message'], 'Pressed End')
        note = self.ref(outline, 'textbox "Delivery note"')
        self.assertEqual(heard(self.ok('press', element=note, key='x', actor='ben', **keys))['message'], 'Pressed x')
        # A form's answers by name place no secret, and say so of each.
        answered = heard(self.ok('fill_form', form='form1', actor='ada',
                                 answers={'Delivery note': 'Ring twice', 'Card number': 'card-4000056655665556'}))
        self.assertEqual({result['name']: result.get('code') for result in answered['data']['results']},
                         {'Delivery note': None, 'Card number': 'secret_field'})
        # What the other actor did meanwhile is told with that answer, and
        # says no more of its keys than the record does.
        self.assertEqual(([other['message'] for other in answered['others']], answered['others_earlier']),
                         (['Pressed a key in a secret field'] * 6 + ['Pressed End', 'Pressed x'], 8))
        # The page did receive every one of them.
        held = json.loads(self.main_world(
            'JSON.stringify(Object.fromEntries([...document.querySelectorAll("input")].map(i => [i.name, i.value])))'))
        self.assertEqual(held, {'card': 'card-4111111111111111', 'csc': 'csc-737-x', 'pass': 'hunter2-secret',
                                'old': 'correct-horse', 'new': 'battery-staple', 'otp': 'otp-918273-x', 'note': 'Ring twice'})
        entries = heard(self.ok('ledger', since=0, actor='cy'))['data']['entries']
        fills = [entry for entry in entries if entry['action'] == 'fill' and entry['ok']]
        self.assertEqual([entry['target']['name'] for entry in fills], list(filled))
        for entry in fills:
            self.assertEqual((entry.get('text'), entry['secret']), (None, True), entry)
        presses = [entry for entry in entries if entry['action'] == 'press']
        self.assertEqual([entry['message'] for entry in presses],
                         ['Pressed a key in a secret field'] * 14 + ['Pressed End', 'Pressed x'])
        # A key pressed without naming a field is set down with the field
        # that had the keyboard.
        self.assertEqual([entry['target'] for entry in presses[:15]], [{'role': 'textbox', 'name': 'Account password'}] * 15)
        self.assertEqual(entries[-1]['answers'], [{'name': 'Delivery note', 'value': 'Ring twice'}])
        # Every read says a secret is set, by its length alone.
        review = heard(self.ok('fields', actor='rev'))
        fields = {field['name']: field for field in review['data']['fields']}
        self.assertEqual((fields['Account password']['value'], fields['Code we sent you']['value'], fields['Delivery note']['value']),
                         ('<14 characters hidden>', '<12 characters hidden>', 'Ring twice'))
        self.assertEqual(fields['Account password']['set']['by'], 'ben')
        asked = {field['name']: field['kind'] for field in heard(self.ok('forms'))['data']['forms'][0]['fields']}
        self.assertEqual(asked, {'Card number': 'secret', 'Security code': 'secret', 'Account password': 'secret',
                                 'Old password': 'secret', 'New password': 'secret', 'Code we sent you': 'secret',
                                 'Delivery note': 'text'})
        self.assertEqual(heard(self.ok('search', text='hunter'))['data']['total'], 0)
        # A page that shows a password on request turns its field into plain
        # text: what it holds is no less a secret.
        self.main_world('(document.querySelector("[name=pass]").type = "text", 1)')
        self.assertIn('textbox "Account password" [ref=' + password + '] value=<14 characters hidden>',
                      heard(self.ok('outline', actor='rev'))['outline'])
        everything = json.dumps(said)
        for text in list(filled.values()) + ['hunter2', '-secret', 'card-4000056655665556']:
            self.assertNotIn(text, everything)

    def test_what_a_person_types_is_set_down_in_the_record(self):
        # A person at the page is no action, and what they type is in the
        # tab's record all the same: one entry to a burst of editing a
        # field, in its place among the actions around it.
        self.assertTrue(next(event for event in self.h.seen if event['event'] == 'ready')['page_events'])
        self.h.navigate(self.url + '/form')
        outline = self.ok('outline', actor='ada')['outline']
        email, search = self.ref(outline, 'textbox "Email"'), self.ref(outline, 'searchbox "Search"')
        self.ok('fill', element=email, text='ada@example.org', actor='ada')
        time.sleep(2)
        self.main_world('(document.querySelector("[name=q]").focus(), 1)')
        mark = len(self.h.seen)
        self.typed('lamp')
        first = self.told('page_record', mark)
        entry = first['entry']
        self.assertEqual((first['seq'], entry['seq'], entry['how'], entry['actor'], entry['ok']), (2, 2, 'input', '', True), first)
        self.assertNotIn('action', entry)
        # The field is named as an action's is, so `find` names it again.
        self.assertEqual(entry['target'], {'role': 'searchbox', 'name': 'Search'})
        self.assertEqual((entry['text'], entry['message'], entry['url']),
                         ('lamp', 'Direct input into searchbox "Search"', self.url + '/form'))
        # Typing on after a pause is a burst of its own, set down with what
        # the field then holds.
        mark = len(self.h.seen)
        self.typed(' shade')
        self.assertEqual(self.told('page_record', mark)['entry']['text'], 'lamp shade')
        # An agent's typing is its action's own and is not set down twice:
        # a key pressed into the field it names, and one pressed where the
        # keyboard is.
        keys = dict(ctrl=False, shift=False, alt=False, meta=False)
        self.ok('press', element=search, key='s', actor='ada', **keys)
        self.ok('press', key='x', actor='ada', **keys)
        self.h.settle(1.6)
        # A box a person ticks is direct input as well, set down at once.
        mark = len(self.h.seen)
        self.main_world('(document.getElementById("terms").focus(), 1)')
        self.typed(' ')
        ticked = self.told('page_record', mark)['entry']
        self.assertEqual((ticked['target'], ticked['text']), ({'role': 'checkbox', 'name': 'Accept terms'}, 'checked'))
        # A secret a person types is set down as set, and no more.
        mark = len(self.h.seen)
        self.main_world('(document.querySelector("[name=password]").focus(), 1)')
        self.typed('pw-9431')
        secret = self.told('page_record', mark)['entry']
        self.assertEqual((secret['how'], secret['secret'], secret['target']),
                         ('input', True, {'role': 'textbox', 'name': 'Password'}), secret)
        self.assertNotIn('text', secret)
        self.assertEqual(self.main_world('document.querySelector("[name=password]").value'), 'pw-9431')
        record = self.ok('ledger', since=0, actor='ben')
        self.assertEqual([(listed['seq'], listed.get('action'), listed.get('how')) for listed in record['data']['entries']],
                         [(1, 'fill', None), (2, None, 'input'), (3, None, 'input'), (4, 'press', None), (5, 'press', None),
                          (6, None, 'input'), (7, None, 'input')])
        own = [event for event in self.h.seen[mark:] if event.get('event') in ('page_record', 'page_changed')]
        self.assertEqual(len(own), 2, own)
        self.assertNotIn('pw-9431', json.dumps(record) + json.dumps(own))
        # Another actor is told of it as of anyone's part in the tab.
        heard = self.ok('changes', actor='ada')
        self.assertEqual([(other['actor'], other['how'], other['seq']) for other in heard['others']],
                         [('', 'input', 6), ('', 'input', 7)])
        # The review says of each field what the record says.
        fields = {field['name']: field for field in self.ok('fields')['data']['fields']}
        self.assertEqual((fields['Accept terms']['set'], fields['Password']['set']), ({'how': 'input'}, {'how': 'input'}))
        self.assertEqual(fields['Search']['set'], {'how': 'action', 'by': 'ada', 'seq': 5})

    def test_what_a_person_types_into_another_site_s_frame_is_set_down_with_its_frame(self):
        self.h.navigate(self.url + '/embedder')
        outline = self.reads('textbox "Email"', 'the frame was never read with the page')['outline']
        frame = re.search(r'iframe "Newsletter sign-up" \[frame=(\d+)\]', outline).group(1)
        query = self.ref(outline, 'textbox "Search the letters"')
        # The keyboard is put into the frame's field (an agent's click
        # stands in for the person's), and the person types once that
        # action's own moment has passed.
        self.ok('click', element=query, actor='ada')
        time.sleep(2)
        mark = len(self.h.seen)
        self.typed('budget')
        entry = self.told('page_record', mark)['entry']
        self.assertEqual((entry['how'], entry['text'], entry['target']),
                         ('input', 'budget', {'role': 'textbox', 'name': 'Search the letters'}), entry)
        self.assertEqual((entry['frame'], entry['frame_name'], entry['frame_url']),
                         (frame, 'Newsletter sign-up', self.server.other + '/signup'), entry)
        # The app is told the frame's field changed as well, and not what
        # it holds.
        changed = self.told('page_changed', mark, lambda event: event.get('frame') == frame)
        self.assertEqual([(field['name'], field['how']) for field in changed['fields']], [('Search the letters', 'input')])
        self.assertEqual((changed['frame_name'], changed['frame_url']), ('Newsletter sign-up', self.server.other + '/signup'))
        self.assertNotIn('budget', json.dumps(changed))

    def test_the_engine_says_when_the_page_changes_a_field_or_a_form_itself(self):
        # Whoever reviews a page is told when the page's own script changes
        # what a field holds or what a form asks, rather than reading the
        # page again and again to find out.
        self.h.navigate(self.url + '/apply')
        form = self.ok('forms')['data']['forms'][0]
        # A script writes a value: no event of the page's own says so.
        mark = len(self.h.seen)
        self.main_world('(document.querySelector("[name=arrival]").value = "2027-01-02", 1)')
        told = self.told('page_changed', mark)
        self.assertEqual([(field['role'], field['name'], field['how'], field['form'], field['question']) for field in told['fields']],
                         [('textbox', 'Arrival', 'page', 'form1', 'Arrival')], told)
        self.assertEqual((told['forms'], told['tab']), ([], 1), told)
        self.assertNotIn('frame', told)
        # It names the field by a ref that acts on it, and says nothing of
        # what the field holds: that is read.
        self.assertNotIn('2027-01-02', json.dumps(told))
        arrival = self.ref(self.outline(), 'textbox "Arrival"')
        self.assertEqual((told['fields'][0]['element'], int(arrival[1:arrival.index('e')])), (arrival, told['document']))
        # A script makes the form ask more: the form, by what it is now.
        mark = len(self.h.seen)
        self.main_world('(document.getElementById("other").hidden = false, 1)')
        told = self.told('page_changed', mark, lambda event: event['forms'])
        grown = self.ok('forms')['data']['forms'][0]
        self.assertNotEqual(grown['signature'], form['signature'])
        self.assertEqual(told['forms'], [{'form': 'form1', 'signature': grown['signature'], 'offers': grown['offers']}])
        # A script offers another answer: the same form, offering others.
        mark = len(self.h.seen)
        self.main_world('(document.getElementById("nat").add(new Option("Chile")), 1)')
        told = self.told('page_changed', mark, lambda event: event['forms'])
        self.assertEqual((told['forms'][0]['signature'], told['forms'][0]['offers'] != grown['offers']), (grown['signature'], True))
        # A secret a script writes is named as changed and never told.
        mark = len(self.h.seen)
        self.main_world('(document.querySelector("[name=code]").value = "hunter2-secret", 1)')
        told = self.told('page_changed', mark)
        self.assertEqual([(field['name'], field['how']) for field in told['fields']], [('Passcode', 'page')])
        self.assertNotIn('hunter2', json.dumps(told))
        # A busy page does not flood: forty writes in two seconds are told
        # a few times.
        mark = len(self.h.seen)
        self.main_world('(window.writes = 0, window.busy = setInterval(() => {'
                        ' document.querySelector("[name=dob_day]").value = String(++window.writes);'
                        ' if (window.writes >= 40) clearInterval(window.busy); }, 50), 1)')
        self.h.settle(3.5)
        self.assertEqual(self.main_world('window.writes'), 40)
        busy = [event for event in self.h.seen[mark:] if event.get('event') == 'page_changed']
        self.assertTrue(1 <= len(busy) <= 10, busy)
        self.assertEqual({field['name'] for event in busy for field in event['fields']}, {'Day'})
        # An action's own edit is told by its answer, not as the page's.
        mark = len(self.h.seen)
        self.ok('fill', element=arrival, text='2027-03-04', actor='ada')
        self.h.settle(1.2)
        self.assertEqual([event for event in self.h.seen[mark:] if event.get('event') == 'page_changed'], [])
        # What a person types is told as theirs.
        time.sleep(1)
        self.main_world('(document.querySelector("[name=dob_month]").focus(), 1)')
        mark = len(self.h.seen)
        self.typed('11')
        told = self.told('page_changed', mark)
        self.assertEqual([(field['name'], field['how'], field['question']) for field in told['fields']],
                         [('Month', 'input', 'Date of birth: Month')])

    def test_what_a_gesture_of_pointer_steps_edits_is_the_gesture_s_own(self):
        # A box ticked by the steps of a gesture is ticked by that gesture:
        # the review says so, and the record holds the gesture once, with no
        # person's input beside it.
        self.h.navigate(self.url + '/finds')
        box = self.ref(self.ok('outline', actor='ada')['outline'], 'checkbox "Newsletter"')
        self.ok('pointer', button='left', actor='ada', steps=[dict(kind='move', element=box, x=4, y=4),
                                                             dict(kind='down', element=box, x=4, y=4),
                                                             dict(kind='up', element=box, x=4, y=4)])
        self.h.settle(1.6)
        fields = {field['name']: field for field in self.ok('fields')['data']['fields']}
        self.assertEqual((fields['Newsletter']['value'], fields['Newsletter']['set']),
                         ('checked', {'how': 'action', 'by': 'ada', 'seq': 1}))
        self.assertEqual([(entry.get('action'), entry.get('how')) for entry in self.ok('ledger', since=0)['data']['entries']],
                         [('pointer', None)])

    def test_outline_lists_structure_with_refs_and_hides_secrets(self):
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        self.assertIn('- banner', outline)
        self.assertIn('heading "Fixture shop" level=1', outline)
        self.assertRegex(outline, r'link "Next page" \[ref=d\d+e\d+\] -> http://127\.0\.0\.1:\d+/next')
        self.assertIn('text "Enter your details."', outline)
        self.assertRegex(outline, r'textbox "Email" \[ref=d\d+e\d+\] type=email required')
        self.assertRegex(outline, r'combobox "Country" \[ref=d\d+e\d+\] value="Netherlands"')
        self.assertIn('option "Belgium"', outline)
        self.assertIn('button "Shadow button"', outline)
        email = self.ref(outline, 'textbox "Email"')
        self.assertEqual(email, self.ref(self.outline(), 'textbox "Email"'), 'refs are stable across reads')
        password = self.ref(outline, 'textbox "Password"')
        refused = self.page('fill', element=password, text='hunter2')
        self.assertEqual((refused['ok'], refused['code']), (False, 'secret_field'), refused)
        self.ok('fill', element=password, text='hunter2', secret=True)
        again = self.outline()
        self.assertIn('value=<7 characters hidden>', again)
        self.assertNotIn('hunter2', again)
        self.assertEqual(self.main_world('typeof globalThis.__tiloPage'), 'undefined',
                         'page script must not reach the automation world')

    def test_a_frame_is_named_by_the_outline_and_operated_in_its_own_document(self):
        self.h.navigate(self.url + '/framed')
        outline = self.outline()
        # The frame is one line of the page, naming the id an action takes to
        # run in it and the address it would reach.
        self.assertRegex(outline, r'iframe "Payment form" \[frame=1\] src="http://127\.0\.0\.1:\d+/widget"')
        # Inside the frame the outline is the frame's own document, and the
        # page around it is not part of it.
        inside = self.ok('outline', frame='1')
        self.assertRegex(inside['outline'], r'textbox "IBAN" \[ref=d\d+e\d+\]')
        self.assertNotIn('Outside the frame', inside['outline'])
        iban = self.ref(inside['outline'], 'textbox "IBAN"')
        self.ok('fill', element=iban, text='EX89370400440532013000', frame='1')
        self.assertIn('value="EX89370400440532013000"', self.ok('outline', frame='1')['outline'])
        # A ref says which document it was read in: an action on an element of
        # a frame runs in that frame without naming it.
        self.ok('fill', element=iban, text='EX11')
        self.assertIn('value="EX11"', self.ok('outline', frame='1')['outline'])
        # The page's own read shows what the frame holds under the line that
        # lists it, with the refs the frame's own read gives.
        self.assertRegex(self.outline(), r'iframe "Payment form" \[frame=1\][^\n]*\n(?:  [^\n]*\n)*?  +- textbox "IBAN" \[ref=' + iban + r'\]')
        # A frame id the outline never printed is refused, not read as the page.
        invented = self.page('outline', frame='7')
        self.assertEqual((invented['ok'], invented['code']), (False, 'no_frame'), invented)

    def test_a_frame_of_a_frame_has_a_number_of_its_own(self):
        self.h.navigate(self.url + '/nested')
        # Every frame a read lists is given a number, whichever document holds
        # it: the page's frame, and the frame inside that one. The page's read
        # shows what its own frame holds, and that frame's line for the frame
        # inside it says how that one is read: by its number.
        outline = self.outline()
        self.assertRegex(outline, r'iframe "Outer frame" \[frame=1\]')
        self.assertRegex(outline, r'\n  - text "Outer document"\n  - iframe "Payment form" \[frame=2\]')
        self.assertNotIn('IBAN', outline, 'a frame inside a frame is read by its own number')
        outer = self.ok('outline', frame='1')['outline']
        self.assertNotIn('Nested frames', outer, 'the frame reads its own document alone')
        self.assertRegex(outer, r'iframe "Payment form" \[frame=2\]')
        inner = self.ok('outline', frame='2')['outline']
        self.assertRegex(inner, r'textbox "IBAN" \[ref=d\d+e\d+\]')
        self.assertNotIn('Outer document', inner, 'the inner frame reads its own document alone')
        iban = self.ref(inner, 'textbox "IBAN"')
        self.ok('fill', element=iban, text='EX89370400440532013000')
        self.assertIn('value="EX89370400440532013000"', self.ok('outline', frame='2')['outline'])
        # A frame named beside a ref is the document the action runs in, and
        # the outer frame holds no IBAN of its own.
        held = self.page('fill', element=iban, text='EX11', frame='1')
        self.assertEqual((held['ok'], held['code']), (False, 'stale_element'), held)
        # A path of numbers names no frame: every frame has one number.
        path = self.page('outline', frame='1.1')
        self.assertEqual((path['ok'], path['code']), (False, 'no_frame'), path)
        # A click in the frame inside the frame lands where the two frames
        # put it: the form is sent, and the inner frame goes on alone.
        self.ok('click', element=self.ref(inner, 'button "Save bank details"'))
        after = self.reads('text "/done"', 'the inner frame never went on', frame='2')
        self.assertNotIn('Save bank details', after['outline'])
        self.assertIn('Nested frames', self.outline(), 'the page stayed where it was')

    def test_a_frame_the_page_adds_later_is_named_and_operated_in(self):
        self.h.navigate(self.url + '/late')
        # The frame is made by script once the page is up, so it is in no frame
        # tree a read of the page already made; its own document then moves on
        # to the form by itself. Every read of the page from here on must print
        # an id an action can run in, and that frame must be reachable by it.
        deadline = time.monotonic() + HANG_BOUND
        while 'iframe' not in (outline := self.outline()):
            self.assertLess(time.monotonic(), deadline, 'the late frame never appeared')
            time.sleep(0.2)
        numbers = re.findall(r'iframe "Late payment form" \[frame=([\d.]+)\]', outline)
        self.assertEqual(len(numbers), 1, outline)
        frame = numbers[0]
        # The frame's first document leaves for the form by itself, and the read
        # follows it: the id names the frame, never one document of it.
        while 'IBAN' not in (inside := self.ok('outline', frame=frame))['outline']:
            self.assertLess(time.monotonic(), deadline, 'the frame never reached its form')
            time.sleep(0.1)
        self.assertRegex(inside['outline'], r'textbox "IBAN" \[ref=d\d+e\d+\]')
        iban = self.ref(inside['outline'], 'textbox "IBAN"')
        self.ok('fill', element=iban, text='EX89370400440532013000', frame=frame)
        self.assertIn('value="EX89370400440532013000"', self.ok('outline', frame=frame)['outline'])
        holder = self.ref(inside['outline'], 'textbox "Account holder"')
        self.ok('fill', element=holder, text='Maria Fischer', frame=frame)
        save = self.ref(inside['outline'], 'button "Save bank details"')
        self.ok('click', element=save, frame=frame)
        self.assertNotIn('Save bank details', self.ok('outline', frame=frame)['outline'])

    def test_a_frame_the_page_set_up_by_script_is_reached_through_the_frame(self):
        self.h.navigate(self.url + '/scripted-frame')
        # The frame is made by script, and the document it reaches is served by
        # another origin: the page holds the frame's element and never its
        # document. It is still one line of the page, and the id it prints
        # addresses it, so the address an action takes is never one the page
        # can read.
        deadline = time.monotonic() + HANG_BOUND
        while 'iframe' not in (outline := self.outline()):
            self.assertLess(time.monotonic(), deadline, 'the scripted frame never appeared')
            time.sleep(0.2)
        numbers = re.findall(r'iframe "Address finder" \[frame=([\d.]+)\]', outline)
        self.assertEqual(len(numbers), 1, outline)
        frame = numbers[0]
        while 'IBAN' not in (inside := self.ok('outline', frame=frame))['outline']:
            self.assertLess(time.monotonic(), deadline, 'the frame never reached its form')
            time.sleep(0.1)
        self.assertRegex(inside['outline'], r'textbox "IBAN" \[ref=d\d+e\d+\]')
        self.assertNotIn('Extension page', inside['outline'], 'the frame reads its own document alone')
        # The frame's own document is written in through the frame's own
        # outline, and the page around it never runs a line of it.
        iban = self.ref(inside['outline'], 'textbox "IBAN"')
        self.ok('fill', element=iban, text='EX89370400440532013000', frame=frame)
        self.assertIn('value="EX89370400440532013000"', self.ok('outline', frame=frame)['outline'])
        self.assertNotIn('Extension page', self.ok('outline', frame=frame)['outline'])

    def test_a_frame_the_page_set_up_by_script_is_a_document_that_arrived(self):
        self.h.navigate(self.url + '/late')
        # The frame's document is fetched after the page loaded. A caller
        # reading the page's requests finds it as a document, never as one
        # still on its way: a request that arrived and a request that never
        # will must not read the same.
        # Recording starts with the first request made of the log, so the page
        # is reached once the log is listening: its documents, and the frame's,
        # are then the ones this read covers.
        self.h.navigate(self.url + '/late')
        deadline = time.monotonic() + HANG_BOUND
        used = 0
        while True:
            network = self.ok('network', since=used)
            used = network['data']['next']
            documents = [e for e in network['data']['entries']
                         if e.get('type') == 'Document' and 'widget-late' in e.get('url', '')]
            if documents and all('state' in e for e in documents) and documents[-1]['state'] != 'pending':
                break
            self.assertLess(time.monotonic(), deadline, f'the frame\'s document never arrived: {network}')
            time.sleep(0.2)
        self.assertEqual(documents[-1]['state'], 'done', documents[-1])
        # Every document the page holds is finished: nothing is left pending
        # once its own document has loaded.
        every = self.ok('network', since=0)['data']['entries']
        documents = [e for e in every if e.get('type') == 'Document']
        self.assertTrue(documents, every)
        self.assertEqual([e for e in documents if e.get('state') == 'pending'], [], documents)

    def test_a_consent_dialog_in_another_site_s_frame_is_read_and_answered(self):
        # A site asks its cookie question in a frame of another site, which
        # runs in a process of its own. The page's read shows the question
        # under the line that lists the frame, with refs that act in it, so
        # it is answered without the frame being named at all.
        self.h.navigate(self.url + '/news')
        read = self.reads('We value your privacy', 'the consent frame was never read with the page')
        outline = read['outline']
        listed = re.search(r'^- iframe "Privacy choices" \[frame=(\d+)\] src="http://localhost:\d+/consent"\n'
                           r'  - dialog "We value your privacy"\n', outline, re.M)
        self.assertIsNotNone(listed, outline)
        frame = listed.group(1)
        self.assertRegex(read['message'], rf'^Read \d+ lines, and \d+ lines of frame {frame}$')
        # A frame nobody sees is listed and not read.
        self.assertRegex(outline, r'iframe "pixel" \[frame=\d+\]')
        self.assertNotIn('/tracker"\n  ', outline)
        yes = self.ref(outline, 'button "Yes, I\'m happy"')
        # The frame's own document is read by the frame's number: the whole of
        # it, found in, searched and compared as the page's own is.
        inside = self.ok('outline', frame=frame, actor='ada')
        self.assertEqual(inside['message'], f'Read 6 lines in frame {frame}')
        self.assertRegex(inside['outline'], r'^- dialog "We value your privacy"\n'
                         r'  - heading "We value your privacy" level=2\n'
                         r'  - text "We and our 87 partners store information on your device\."\n'
                         r'  - button "Yes, I\'m happy" \[ref=d\d+e\d+\]\n'
                         r'  - button "No, thank you" \[ref=d\d+e\d+\]\n'
                         r'  - link "Manage options" \[ref=d\d+e\d+\] -> http://localhost:\d+/consent-options$')
        self.assertNotIn('The Daily Fixture', inside['outline'])
        self.assertEqual(self.ref(inside['outline'], 'button "Yes, I\'m happy"'), yes)
        no = self.find(role='button', name='No, thank you', frame=frame)
        self.assertEqual(no['data']['element'], self.ref(outline, 'button "No, thank you"'))
        said = self.ok('search', text='87 partners', frame=frame)
        self.assertEqual(said['data']['total'], 1, said)
        self.assertEqual(self.ok('changes', frame=frame, actor='ada')['message'], 'Nothing changed')
        # The page's own document holds none of the frame's words: a search
        # that names no frame looks on in the frame the page shows, and says
        # which frame holds them.
        there = self.ok('search', text='87 partners')
        self.assertEqual((there['data']['total'], there['data']['page_total'], there['data']['matches'][0]['frame']),
                         (1, 0, frame), there)
        self.assertIn(f'1 in frame {frame} ("Privacy choices")', there['message'])
        # Words no document of the page says are found nowhere, and the
        # answer names the frame a caller may read.
        absent = self.ok('search', text='88 partners')
        self.assertEqual(absent['data']['total'], 0, absent)
        self.assertIn(f'frame={frame} ("Privacy choices")', absent['message'])
        # The question is answered by the ref alone, and the page hears it:
        # it takes the frame away.
        clicked = self.ok('click', element=yes, actor='ada')
        self.assertEqual((clicked['navigation'], clicked['navigated']), ('none', False), clicked)
        after = self.reads('consent accepted', 'the page never heard the answer')['outline']
        self.assertNotIn('Privacy choices', after)
        # The tab's record says who answered, what, and in which frame, by
        # the name and the address that still say which frame it was.
        entry = self.ok('ledger', since=0)['data']['entries'][-1]
        self.assertEqual((entry['actor'], entry['action'], entry['ok']), ('ada', 'click', True), entry)
        self.assertEqual(entry['target'], {'role': 'button', 'name': "Yes, I'm happy"})
        self.assertEqual((entry['frame'], entry['frame_name'], entry['frame_url']),
                         (frame, 'Privacy choices', self.server.other + '/consent'), entry)
        # The frame has left: its number names nothing now and says so, and a
        # ref read in it is stale.
        gone = self.page('outline', frame=frame)
        self.assertEqual((gone['ok'], gone['code']), (False, 'frame_gone'), gone)
        self.assertIn(f'Frame {frame} "Privacy choices" has left the page', gone['message'])
        stale = self.page('click', element=yes)
        self.assertEqual((stale['ok'], stale['code']), (False, 'stale_element'), stale)

    def test_a_frame_replaced_while_the_page_loads_keeps_no_number_of_the_one_before(self):
        # Frames come and go while a page loads. A frame keeps its number
        # while its own document moves on and while other frames arrive and
        # leave around it; a frame taken out is gone, and the one put in its
        # place is another frame with another number.
        self.h.navigate(self.url + '/churn')
        first = re.search(r'iframe "Privacy choices" \[frame=(\d+)\]', self.outline()).group(1)
        # The frame's document moves on to the question by itself: the number
        # names the frame, never one document of it.
        question = self.reads('We value your privacy', 'the frame never reached its question', frame=first)['outline']
        old = self.ref(question, 'button "Yes, I\'m happy"')
        # The page takes that frame out, puts another in its place and adds
        # an advert's frame before it in the document.
        self.assertEqual(self.main_world('churn()'), 1)
        gone = self.page('outline', frame=first)
        self.assertEqual((gone['ok'], gone['code']), (False, 'frame_gone'), gone)
        acted = self.page('click', element=old)
        self.assertEqual((acted['ok'], acted['code']), (False, 'stale_element'), acted)
        self.assertEqual(self.main_world('document.getElementById("said").textContent'), '',
                         'nothing was answered in a frame that is gone')
        # Read again, the page lists the frames it has now, each with a number
        # of its own: the one put in is not the one taken out.
        outline = self.reads('We value your privacy', 'the frame put in was never read')['outline']
        numbers = dict(re.findall(r'iframe "(Privacy choices|Advert)" \[frame=(\d+)\]', outline))
        self.assertEqual(len({first, numbers['Privacy choices'], numbers['Advert']}), 3, outline)
        # The advert's frame leaves again: the frame after it in the document
        # keeps its number, and a ref read in it still acts in it.
        yes = self.ref(outline, 'button "Yes, I\'m happy"')
        self.assertEqual(self.main_world('calm()'), 1)
        self.assertNotIn('Advert', self.outline())
        self.assertIn('We value your privacy', self.ok('outline', frame=numbers['Privacy choices'])['outline'])
        left = self.page('outline', frame=numbers['Advert'])
        self.assertEqual((left['ok'], left['code']), (False, 'frame_gone'), left)
        self.ok('click', element=yes)
        self.assertIn('consent accepted', self.reads('consent accepted', 'the page never heard the answer')['outline'])

    def test_reading_and_acting_reach_a_form_in_another_site_s_frame(self):
        self.h.navigate(self.url + '/embedder')
        outline = self.reads('textbox "Email"', 'the frame was never read with the page')['outline']
        frame = re.search(r'iframe "Newsletter sign-up" \[frame=(\d+)\] src="http://localhost:', outline).group(1)
        email, edition, query, note = (self.ref(outline, name) for name in (
            'textbox "Email"', 'combobox "Edition"', 'textbox "Search the letters"', 'textbox "Note"'))
        # Every action on an element of the frame runs in the frame: a fill, a
        # choice and a key, each by the ref alone.
        filled = self.ok('fill', element=email, text='ada@example.org', actor='ada')
        self.ok('select', element=edition, values=['Evening'], actor='ada')
        searched = self.ok('fill', element=query, text='budget', actor='ben')
        pressed = self.ok('press', element=query, key='Enter', ctrl=False, shift=False, alt=False, meta=False, actor='ben')
        self.assertEqual(pressed['navigation'], 'none', pressed)
        inside = self.reads('searched budget', 'the key never reached the frame', frame=frame)
        self.assertIn(f'in frame {frame}', inside['message'])
        self.assertIn('value="ada@example.org"', inside['outline'])
        self.assertNotIn('Our letters', inside['outline'], 'the frame reads its own document alone')
        noted = self.ok('fill', element=note, text='ask about the weekend edition', actor='ben')
        # The frame's form is read as what it asks and answered by its
        # questions' names, the frame named; its secret goes the secret's way.
        asked = self.ok('forms', frame=frame)['data']['forms'][0]
        self.assertEqual([(field['name'], field['kind']) for field in asked['fields']],
                         [('Email', 'email'), ('Edition', 'choice'), ('Promo code', 'secret'), ('Accept terms', 'flag')])
        answered = self.ok('fill_form', form=asked['form'], frame=frame, actor='ada',
                           answers={'Email': 'ada@lovelace.example', 'Accept terms': True})
        self.assertEqual([result['ok'] for result in answered['data']['results']], [True, True], answered)
        code = next(field['element'] for field in asked['fields'] if field['name'] == 'Promo code')
        self.ok('fill', element=code, text='LETTERS-2026', secret=True, actor='ada')
        # A review of the page covers the frame: each field with what it holds
        # now and who set it, the frame's by the frame they are in.
        review = self.ok('fields')
        held = {(field.get('frame'), field['name']): field for field in review['data']['fields']}
        self.assertEqual(held[(None, 'Note')]['set'], {'how': 'action', 'by': 'ben', 'seq': noted['seq']})
        self.assertEqual((held[(frame, 'Email')]['value'], held[(frame, 'Email')]['set']),
                         ('ada@lovelace.example', {'how': 'action', 'by': 'ada', 'seq': answered['seq']}))
        self.assertEqual(held[(frame, 'Search the letters')]['set'], {'how': 'action', 'by': 'ben', 'seq': searched['seq']})
        self.assertEqual(held[(frame, 'Accept terms')]['set']['by'], 'ada')
        self.assertIn(f'\n- iframe "Newsletter sign-up" [frame={frame}]\n  - ', review['outline'])
        self.assertIn(f'in frame {frame}', review['message'])
        self.assertNotIn('LETTERS-2026', json.dumps(review))
        alone = self.ok('fields', frame=frame)
        self.assertEqual([field['name'] for field in alone['data']['fields']],
                         ['Email', 'Edition', 'Promo code', 'Accept terms', 'Search the letters'])
        # The tab's record sets each action in the frame down with the frame,
        # and the secret is nowhere in it.
        entries = self.ok('ledger', since=0)['data']['entries']
        there = [entry for entry in entries if entry.get('frame') == frame]
        self.assertEqual([(entry['actor'], entry['action'], entry['ok']) for entry in there],
                         [('ada', 'fill', True), ('ada', 'select', True), ('ben', 'fill', True), ('ben', 'press', True),
                          ('ada', 'fill_form', True), ('ada', 'fill', True)])
        self.assertEqual(there[0]['seq'], filled['seq'])
        self.assertEqual(there[0]['target'], {'role': 'textbox', 'name': 'Email'})
        for entry in there:
            self.assertEqual((entry['frame_name'], entry['frame_url']),
                             ('Newsletter sign-up', self.server.other + '/signup'), entry)
        self.assertEqual([entry['action'] for entry in entries if 'frame' not in entry], ['fill'])
        self.assertNotIn('LETTERS-2026', json.dumps(entries))
        # Sending the form takes the frame to another document, never the page.
        sent = self.ok('fill_form', form=asked['form'], frame=frame, answers={'Edition': 'Morning'}, submit=True, actor='ada')
        self.assertTrue(sent['data']['submitted'], sent)
        self.assertEqual((sent['navigation'], urlparse(sent['url']).path), ('none', '/embedder'), sent)
        self.reads('text "/signed"', 'the frame never went on', frame=frame)
        self.assertEqual(self.other.signed, {'email': ['ada@lovelace.example'], 'edition': ['Morning'],
                                             'code': ['LETTERS-2026'], 'terms': ['yes']})
        self.assertIn('value="ask about the weekend edition"', self.outline(), 'the page kept what it held')

    def test_a_frame_of_another_site_is_scrolled_pictured_and_its_form_submitted(self):
        self.h.navigate(self.url + '/terms-holder')
        outline = self.reads('textbox "Initials"', 'the frame was never read with the page')['outline']
        frame = re.search(r'iframe "Terms" \[frame=(\d+)\]', outline).group(1)
        initials = self.ref(outline, 'textbox "Initials"')
        # The wheel turns at a point of the page, where the frame sits: the
        # frame's own document goes down and up, and the page around it stays.
        down = self.ok('scroll', frame=frame, to='bottom')
        self.assertRegex(down['message'], r'^Scrolled \d+px down, to \d+ of \d+')
        up = self.ok('scroll', frame=frame, to='top')
        self.assertRegex(up['message'], r'^Scrolled \d+px up, to 0 of \d+')
        self.assertEqual(self.main_world('scrollY'), 0, 'the page around the frame did not move')
        # A picture of an element of the frame is that element's own box.
        shot = self.ok('capture', element=initials)
        self.assertTrue(base64.b64decode(shot['data']['capture'], validate=True).startswith(b'\x89PNG\r\n\x1a\n'))
        self.assertTrue(50 < shot['data']['width'] < 600 and 10 < shot['data']['height'] < 100, shot['data']['width'])
        # The frame's form is sent from one of its fields, and the frame goes
        # on to the next document alone.
        self.ok('fill', element=initials, text='AL')
        sent = self.ok('submit', element=initials)
        self.assertEqual((sent['navigation'], urlparse(sent['url']).path), ('none', '/terms-holder'), sent)
        self.reads('text "/agreed"', 'the frame never went on', frame=frame)
        self.assertIn('heading "Before you read" level=1', self.outline())

    def test_the_first_read_after_moving_to_another_site_reads_that_site(self):
        # A page that was read keeps a script world of this side's. The next
        # site runs in another process, which numbers its script contexts
        # from the start again: the world kept from the page before must not
        # be taken for one of them, a frame's or the page's own.
        self.h.navigate(self.url + '/agent')
        self.assertIn('heading "Agent fixture" level=1', self.outline())
        self.h.navigate(self.server.other + '/blanks')
        outline = self.outline()
        self.assertIn('heading "Second site" level=1', outline)
        self.assertIn('text "The page that loaded."', outline)
        self.assertEqual(self.main_world('[window, ...Array.from(frames)].map(each => typeof each.__tiloPage).join()'),
                         'undefined,undefined,undefined,undefined', 'no page script sees the automation world')

    def test_a_frame_is_named_the_way_its_outline_writes_it(self):
        self.h.navigate(self.url + '/framed')
        printed = re.search(r'iframe "Payment form" \[frame=(\d+)\]', self.outline()).group(1)
        # Every form a caller copies an id in is the frame it names: the number,
        # the number as text, and the outline's own bracketed `[frame=1]`.
        for named in (printed, f'frame={printed}', f'[frame={printed}]', f' {printed} '):
            with self.subTest(frame=named):
                inside = self.page('outline', frame=named)
                self.assertTrue(inside['ok'], inside)
                self.assertRegex(inside['outline'], r'textbox "IBAN" \[ref=d\d+e\d+\]')
        # A frame the outline never printed is still refused, however it is
        # written: an id is a number, never a selector or a script.
        for named in ('7', 'frame=7', '[frame=7]', 'iframe', '1.e', '1.1', 'top'):
            with self.subTest(frame=named):
                refused = self.page('outline', frame=named)
                self.assertEqual((refused['ok'], refused['code']), (False, 'no_frame'), refused)
                self.assertIn(printed, refused['message'], 'the answer names a frame the page has')

    def test_a_click_inside_a_frame_is_a_click_in_the_page(self):
        self.h.navigate(self.url + '/framed')
        # A frame is given its number by the read that lists it.
        self.assertIn('[frame=1]', self.outline())
        inside = self.ok('outline', frame='1')
        button = self.ref(inside['outline'], 'button "Save bank details"')
        self.ok('click', element=button, frame='1')
        # The frame's widget submitted the form: reading the frame again shows
        # the page it reached. The page around it did not move, so nothing is
        # reported as a page navigation.
        self.assertNotIn('Save bank details', self.ok('outline', frame='1')['outline'])

    def test_a_pointer_step_in_a_frame_lands_where_the_frame_sits(self):
        self.h.navigate(self.url + '/framed')
        # A frame is given its number by the read that lists it.
        self.assertIn('[frame=1]', self.outline())
        inside = self.ok('outline', frame='1')
        pad = self.ref(inside['outline'], 'canvas')
        # A step naming an element of a frame is placed by the box the frame
        # gives it, moved by where the frame sits in the page, as a click in
        # that frame is: the press reaches the frame's own element, never the
        # page above it.
        self.pointer([dict(kind='move', element=pad, frame='1', x=5, y=5),
                      dict(kind='down', element=pad, frame='1', x=5, y=5),
                      dict(kind='up', element=pad, frame='1', x=5, y=5)])
        pressed = self.main_world('(frame => frame && frame.contentDocument'
                                  '.getElementById("pressed").textContent)'
                                  '(document.querySelector("iframe"))')
        # The canvas's own box starts where the frame's document lays it out,
        # in the frame's own viewport, which starts at the frame's client
        # origin: 8 across, and as far down as the form above it is tall,
        # which is one line of fields in one system's fonts and two in
        # another's. A point 5 by 5 into the canvas is that far into its box,
        # as the frame's own element reads it. (This asserted 13 by 34, the
        # layout of macOS's fonts, which is no part of where a step lands.)
        left, top = json.loads(self.main_world(
            '(box => JSON.stringify([box.left, box.top]))(document.querySelector("iframe").contentDocument'
            '.getElementById("pad").getBoundingClientRect())'))
        self.assertEqual(left, 8, 'the canvas starts at the frame document\'s own margin')
        at = lambda x, y: f'pressed {math.floor(left + x + 0.5)},{math.floor(top + y + 0.5)}'
        self.assertEqual(pressed, at(5, 5), 'the press reached the frame\'s own element')
        # The ref says which frame it was read in, so a step need not name it.
        self.pointer([dict(kind='down', element=pad, x=7, y=9), dict(kind='up', element=pad, x=7, y=9)])
        self.assertEqual(self.main_world('document.querySelector("iframe").contentDocument'
                                         '.getElementById("pressed").textContent'), at(7, 9))
        wrong = self.page('pointer', button='left', steps=[dict(kind='down', element=pad, frame='2', x=5, y=5)])
        self.assertEqual((wrong['ok'], wrong['code']), (False, 'no_frame'), wrong)

    def test_reader_view_shows_the_article_alone_and_puts_the_page_back(self):
        self.h.navigate(self.url + '/article')
        self.main_world('document.getElementById("kept").value = "typed later"')
        shown = self.ok('reader', enabled=True)
        self.assertTrue(shown['data']['reader'], shown)
        self.assertGreater(shown['data']['words'], 100, shown)
        outline = self.outline()
        self.assertIn('heading "The long read" level=1', outline)
        self.assertIn('Reading on a screen', outline)
        for gone in ('Pricing', 'Related story', 'Advertisement'):
            self.assertNotIn(gone, outline, f'{gone} is not part of the article')
        self.assertEqual(self.main_world('getComputedStyle(document.body.firstElementChild.shadowRoot'
                                         '.querySelector("p:not(.site)")).fontSize'), '20px',
                         'the page\'s style sheets do not reach the reading layout')
        closed = self.ok('reader', enabled=False)
        self.assertFalse(closed['data']['reader'], closed)
        outline = self.outline()
        self.assertIn('Pricing', outline)
        self.assertEqual(self.main_world('document.getElementById("kept").value'), 'typed later',
                         'the page comes back as it was')
        self.h.navigate(self.url + '/agent')
        refused = self.page('reader', enabled=True)
        self.assertEqual((refused['ok'], refused['code']), (False, 'no_article'), refused)

    def test_reader_view_reads_an_essay_laid_out_as_old_pages_lay_them_out(self):
        # An old page has no article and no paragraph elements. One sets its
        # essay in the cells of a table: reader view shows every row of it.
        self.h.navigate(self.url + '/old')
        shown = self.ok('reader', enabled=True)
        self.assertTrue(shown['data']['reader'], shown)
        self.assertEqual(shown['data']['title'], 'Old page')
        self.assertGreater(shown['data']['words'], 2500, shown)
        outline = self.outline()
        self.assertIn('heading "Old page" level=1', outline)
        rows = re.findall(r'Row (\d+) of an essay laid out in a table\.', outline)
        self.assertEqual(rows, [str(number) for number in range(1, 301)], 'the whole essay, in its order')
        self.assertFalse(self.ok('reader', enabled=False)['data']['reader'])
        self.assertEqual(len(re.findall(r'Row \d+ of an essay laid out in a table\.', self.outline())), 300,
                         'the page comes back as it was')
        # Another runs its text on in one block, font tags around it and two
        # line breaks between its paragraphs: reader view shows it to its
        # last word.
        self.h.navigate(self.url + '/run')
        shown = self.ok('reader', enabled=True)
        self.assertTrue(shown['data']['reader'], shown)
        outline = self.outline()
        self.assertIn('First paragraph, a short one.', outline)
        for sentence in RUN_SENTENCES:
            self.assertIn(sentence, outline)
        self.assertIn('Last paragraph: the point of it all.', outline)
        # A page of controls and little text is still no article.
        self.h.navigate(self.url + '/form')
        refused = self.page('reader', enabled=True)
        self.assertEqual((refused['ok'], refused['code']), (False, 'no_article'), refused)

    def test_fill_select_and_click_operate_the_page_like_a_person(self):
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        self.ok('fill', element=self.ref(outline, 'textbox "Email"'), text='ada@example.test')
        self.ok('select', element=self.ref(outline, 'combobox "Country"'), values=['Belgium'])
        self.ok('click', element=self.ref(outline, 'checkbox "Accept terms"'))
        self.ok('click', element=self.ref(outline, 'button "Shadow button"'))
        after = self.outline()
        self.assertIn('value="ada@example.test"', after)
        self.assertIn('value="Belgium"', after)
        self.assertRegex(after, r'checkbox "Accept terms" \[ref=d\d+e\d+\] checked')
        self.assertIn('text "shadow clicked"', after)
        self.assertEqual(self.main_world("new FormData(document.forms[0]).get('country')"), 'BE')
        missing = self.page('select', element=self.ref(outline, 'combobox "Country"'), values=['France'])
        self.assertEqual((missing['ok'], missing['code']), (False, 'no_option'), missing)
        covered = self.page('click', element=self.ref(outline, 'button "Covered"'))
        self.assertEqual((covered['ok'], covered['code']), (False, 'obscured'), covered)
        self.assertIn('shade', covered['message'])
        not_text = self.page('fill', element=self.ref(outline, 'button "Place order"'), text='x')
        self.assertEqual(not_text['code'], 'not_editable', not_text)

    def test_submit_validates_and_enter_submits_through_a_navigation(self):
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        search = self.ref(outline, 'searchbox "Search"')
        invalid = self.page('submit', element=search)
        self.assertEqual((invalid['ok'], invalid['code']), (False, 'invalid_form'), invalid)
        self.assertIn(self.ref(outline, 'textbox "Email"'), invalid['message'])
        self.ok('fill', element=self.ref(outline, 'textbox "Email"'), text='ada@example.test')
        self.ok('fill', element=search, text='tea')
        pressed = self.ok('press', element=search, key='Enter', ctrl=False, shift=False, alt=False, meta=False)
        # The reply waits for the navigation Enter started, so it names the new page.
        self.assertEqual((pressed['navigation'], pressed['navigated'], pressed['loading']), ('started', True, False), pressed)
        self.assertTrue(any(e['event'] == 'loaded' and '/done' in e['url'] for e in self.h.seen))
        query = parse_qs(urlparse(pressed['url']).query)
        self.assertEqual((query['email'], query['q']), (['ada@example.test'], ['tea']))
        stale = self.page('click', element=search)
        self.assertEqual((stale['ok'], stale['code']), (False, 'stale_element'), stale)

    def test_fill_a_region_writes_the_text_as_a_person_types_it(self):
        self.h.navigate(self.url + '/rich')
        outline = self.outline()
        filled = self.ok('fill', element=self.ref(outline, 'textbox "Note"'), text='The rent is going up\nfrom October.')
        self.assertTrue(filled['ok'], filled)
        self.assertEqual(self.main_world('document.getElementById("note").textContent'),
                         'The rent is going upfrom October.')
        self.assertEqual(self.main_world('document.getElementById("note").children[0].textContent'),
                         'from October.')
        # The page heard each key, and heard the edit before it was made.
        heard = self.main_world("window.heard.filter(e => e.startsWith('note:'))")
        self.assertIn('note:keydown:Enter', heard)
        self.assertIn('note:beforeinput:insertParagraph', heard)
        typed = [e.split('=', 1)[1] if '=' in e else '\n' for e in heard
                 if e.startswith('note:beforeinput:')]
        self.assertEqual(''.join(typed), 'The rent is going up\nfrom October.', heard[:6])

    def test_fill_reaches_editors_with_a_model_and_blocks_of_their_own(self):
        self.h.navigate(self.url + '/rich')
        outline = self.outline()
        self.ok('fill', element=self.ref(outline, 'textbox "Draft"'), text='First line\nSecond line')
        self.ok('fill', element=self.ref(outline, 'textbox "Report"'), text='What I did\nWhat I will do')
        self.assertEqual(self._blocks('draft'), ['First line', 'Second line'])
        heard = self.main_world("window.heard.filter(e => e.startsWith('draft:'))")
        self.assertTrue(any(e.startswith('draft:beforeinput:insertText') for e in heard), heard[:4])
        self.assertIn('draft:beforeinput:insertParagraph', heard)
        # A block editor heard a real Enter, and started a block for each line.
        self.assertEqual(self._blocks('report'), ['What I did', 'What I will do'])
        self.assertIn('report:keydown:Enter', self.main_world(
            "window.heard.filter(e => e.startsWith('report:'))"))

    def test_enter_in_a_textarea_starts_a_new_line(self):
        self.h.navigate(self.url + '/rich')
        outline = self.outline()
        steps = self.ref(outline, 'textbox "Steps"')
        self.ok('fill', element=steps, text='Sand the frame\nApply two coats')
        # A press after the fill adds a line rather than submitting anything.
        pressed = self.ok('press', element=steps, key='Enter', ctrl=False, shift=False, alt=False, meta=False)
        self.assertFalse(pressed['navigated'], pressed)
        self.ok('fill', element=steps, text='Sand the frame\nApply two coats\nReassemble')
        self.assertEqual(self.main_world('document.getElementById("steps").value'),
                         'Sand the frame\nApply two coats\nReassemble')
        self.assertIn('steps:keydown:Enter', self.main_world(
            "window.heard.filter(e => e.startsWith('steps:'))"))

    def test_clicking_a_link_reports_the_navigation_it_started(self):
        self.h.navigate(self.url + '/form')
        read = self.ok('outline')
        self.assertEqual((read['navigation'], read['navigated']), ('none', False), read)
        link = self.ref(read['outline'], 'link "Next page"')
        clicked = self.ok('click', element=link)
        self.assertEqual((clicked['navigation'], clicked['navigated'], clicked['loading']), ('started', True, False), clicked)
        self.assertTrue(clicked['url'].endswith('/next'), clicked)
        self.assertIn('text "/next"', self.outline())
        idle = self.ok('press', key='Tab', ctrl=False, shift=False, alt=False, meta=False)
        self.assertEqual((idle['navigation'], idle['navigated']), ('possible', False), idle)

    def test_a_navigation_the_page_starts_later_is_its_own_event(self):
        self.h.navigate(self.url + '/later')
        button = self.ref(self.outline(), 'button "Go later"')
        clicked = self.ok('click', element=button)
        # The page navigates a second after it handled the click: the reply
        # cannot know that yet, and says so.
        self.assertEqual((clicked['navigation'], clicked['navigated']), ('possible', False), clicked)
        self.assertTrue(clicked['url'].endswith('/later'), clicked)
        self.h.until(lambda e: e['event'] == 'loaded' and e['url'].endswith('/next'))
        entries = self.ok('network', since=0)['data']['entries']
        later = [entry for entry in entries if entry.get('navigation') and entry['url'].endswith('/next')]
        self.assertEqual(len(later), 1, entries)
        # Chromium attributes it to the click's user activation.
        self.assertTrue(later[0]['user_gesture'], later)


    def test_changes_report_only_what_appeared_after_the_page_settles(self):
        self.h.navigate(self.url + '/agent')
        add = self.ref(self.outline(), 'button "Add later"')
        self.ok('click', element=add)
        # The page adds its line a second after the click; settling waits for it.
        settled = self.ok('settle', quiet_ms=1500, timeout_ms=SETTLE_MS)
        self.assertTrue(settled['data']['settled'], settled)
        self.assertGreater(settled['data']['mutations'], 0, settled)
        changed = self.ok('changes')
        self.assertEqual(changed['outline'], '+ - text "added later"', changed)
        self.assertEqual(self.ok('changes')['message'], 'Nothing changed')

    def test_element_at_names_what_a_click_there_would_hit(self):
        self.h.navigate(self.url + '/agent')
        add = self.ref(self.outline(), 'button "Add later"')
        x, y = self.main_world("(r => [r.x + r.width / 2, r.y + r.height / 2])(document.getElementById('add').getBoundingClientRect())")
        hit = self.ok('at', x=x, y=y)
        self.assertEqual((hit['data']['ref'], hit['data']['interactive']), (add, True), hit)
        empty = self.ok('at', x=990, y=790)
        self.assertFalse(empty['data']['interactive'], empty)

    def test_a_hover_reveals_a_menu_and_a_click_then_opens_it(self):
        self.h.navigate(self.url + '/gestures')
        outline = self.outline()
        self.assertNotIn('Fishing licence', outline, 'the menu starts closed')
        # A trigger a hover reveals something under is named by ref: the rule
        # that opens the menu is what a gesture has to address.
        trigger = self.ref(outline, '(?:hint|hoverable) "Licences menu"')
        hovered = self.ok('hover', element=trigger)
        self.assertTrue(hovered['data']['changed'], hovered)
        self.assertIn('+ - list', hovered['outline'], hovered)
        self.assertIn('Fishing licence', self.outline())
        self.ok('click', element=self.ref(self.outline(), 'link "Fishing licence"'))
        self.assertIn('text "opened fishing"', self.outline())
        # A hover keeps the button up: it reveals a menu, it does not press.
        self.assertEqual(self.main_world('drawn()'), 0)

    def test_a_hover_that_redraws_a_shown_box_takes_nothing_out_of_hiding(self):
        self.h.navigate(self.url + '/gestures')
        outline = self.outline()
        # The rule only redraws the words it hovers — a size, padding, a
        # colour — and takes nothing out of hiding. A hand reaching for them
        # finds what was already there, so they are not a hoverable.
        self.assertNotIn('hoverable "Delivery notes"', outline)
        self.assertIn('text "Delivery notes"', outline)

    def test_a_drag_reorders_a_draggable_list(self):
        self.h.navigate(self.url + '/gestures')
        outline = self.outline()
        first = self.ref(outline, 'draggable "Riverton office"')
        third = self.ref(outline, 'draggable "Central office"')
        dragged = self.ok('drag', element=first, to=third, steps=6)
        self.assertEqual(self.main_world('order()'), 'Porthaven office > Central office > Riverton office', dragged)
        self.assertIn('Dragged', dragged['message'], dragged)

    def test_a_path_draws_on_a_canvas_whose_page_starts_the_line(self):
        # A pad that begins its stroke on pointerdown only draws if the page is
        # the one the pointer is working: an unfocused view drops the moves.
        self.h.navigate(self.url + '/canvas')
        canvas = self.ref(self.outline(), 'canvas "Signature pad"')
        points = [[50, 100], [80, 40], [110, 110], [140, 45], [170, 105], [200, 60], [240, 100]]
        drawn = self.ok('path', element=canvas, points=points)
        # The page hears the press at the first point and a stroke to each
        # point after it, in order and where each is: a line through seven
        # points is six strokes. (This asserted seven: the six, and one of no
        # length from a move the engine repeated at the last point before
        # letting go, which reached the page by itself or together with the
        # move before it as the page had time. The engine no longer repeats
        # it, and each stroke is asserted by where it landed.)
        self.assertEqual(json.loads(self.main_world('strokesAt()')), points[1:], drawn)
        self.assertEqual(self.main_world('strokes()'), len(points) - 1, drawn)

    def test_a_click_still_lands_after_a_drag(self):
        # A drag leaves the engine's own drag behind it; the click that follows
        # has to reach the page as any click does.
        self.h.navigate(self.url + '/gestures')
        outline = self.outline()
        first = self.ref(outline, 'draggable "Riverton office"')
        third = self.ref(outline, 'draggable "Central office"')
        self.ok('drag', element=first, to=third, steps=6)
        save = self.ref(self.outline(), 'button "Save order"')
        clicked = self.ok('click', element=save)
        self.assertEqual(self.main_world("document.getElementById('m').textContent"),
                         'saved Porthaven office > Central office > Riverton office', clicked)

    def test_a_pointer_path_draws_on_a_canvas(self):
        self.h.navigate(self.url + '/gestures')
        outline = self.outline()
        self.assertIn('canvas "Signature pad" 400x150', outline)
        pad = self.ref(outline, 'canvas "Signature pad"')
        points = [[20, 110], [40, 60], [60, 100], [80, 50], [100, 105], [120, 70], [140, 95]]
        drawn = self.ok('path', element=pad, points=points)
        # Every point after the press is a move the page strokes, where the
        # point is; the reply names how many points it drew so an agent can
        # tell ink landed.
        self.assertEqual(drawn['data']['drew'], len(points), drawn)
        inked = json.loads(self.main_world('strokesAt()'))
        self.assertEqual(len(inked), len(points) - 1, (inked, drawn))
        for (x, y), (wanted_x, wanted_y) in zip(inked, points[1:]):
            self.assertTrue(abs(x - wanted_x) <= 1 and abs(y - wanted_y) <= 1, (inked, drawn))
        self.assertEqual(self.main_world('drawn()'), len(points) - 1, drawn)
        self.ok('click', element=self.ref(self.outline(), 'button "Submit signature"'))
        self.assertIn('text "Signature received"', self.outline())

    def test_a_capture_paints_what_the_page_only_shows(self):
        # A page can say something in pixels that no outline can carry: a
        # chart, a scanned letter, a code drawn rather than written. The
        # capture is the engine painting the page itself, so what comes back
        # is what a person would see there.
        self.h.navigate(self.url + '/canvas')
        view = self.ok('capture')
        picture = base64.b64decode(view['data']['capture'], validate=True)
        self.assertEqual(view['data']['format'], 'png')
        self.assertTrue(picture.startswith(b'\x89PNG\r\n\x1a\n'), view['data'])
        # The viewport, as the window was told it: the picture is the whole
        # of what is on screen, and no wider than any capture may be.
        self.assertEqual((view['data']['x'], view['data']['y']), (0, 0))
        self.assertEqual(view['data']['width'], 1000)
        self.assertEqual(view['data']['height'], 800)

        # An element's own box, in the page's coordinates: what the outline
        # named, painted where it sits, not the whole screen around it.
        canvas = self.ref(self.outline(), 'canvas "Signature pad"')
        box = self.ok('capture', element=canvas)
        self.assertEqual(box['data']['format'], 'png')
        # The pad's own 500x150 canvas, with the border a person sees around
        # it: the box is the element's, borders and all.
        self.assertEqual(box['data']['width'], 502)
        self.assertEqual(box['data']['height'], 152)
        painted = base64.b64decode(box['data']['capture'], validate=True)
        self.assertTrue(painted.startswith(b'\x89PNG\r\n\x1a\n'), box['data'])
        self.assertIn('Signature pad', box['message'])

        # A caller who asks for a narrower picture gets one: the page is
        # scaled, never cut, and never widened.
        narrow = self.ok('capture', element=canvas, max_width=200)
        self.assertEqual(narrow['data']['width'], 200)
        self.assertEqual(narrow['data']['height'], round(152 * 200 / 502))

        # A ref the outline never printed names nothing to picture, and is
        # refused before the page is asked to paint anything.
        refused = self.page('capture', element='not-a-ref')
        self.assertFalse(refused['ok'], refused)
        self.assertEqual(refused['code'], 'invalid_request')
        self.assertNotIn('capture', refused)

        # A click that lands on the pad is drawn ink: the capture is of the
        # page as it is now, not as it was loaded.
        points = [[40, 60], [90, 40], [140, 100]]
        self.ok('path', element=canvas, points=points)
        ink = self.ok('capture', element=canvas)
        self.assertNotEqual(ink['data']['capture'], box['data']['capture'],
                            'a picture is painted when it is asked for')

    def test_console_and_network_reach_the_agent_and_not_the_page(self):
        self.h.navigate(self.url + '/agent')
        log = self.ref(self.outline(), 'button "Log"')
        self.ok('click', element=log)
        self.ok('settle', quiet_ms=300, timeout_ms=SETTLE_MS)
        console = self.ok('console', since=0)['data']
        texts = [entry['text'] for entry in console['entries']]
        self.assertIn('clicked 7', texts, console)
        self.assertTrue(any('boom' in entry['text'] and entry.get('uncaught') for entry in console['entries']), console)
        self.assertEqual(console['provenance'], 'web-content')
        network = self.ok('network', since=0)['data']
        data = [entry for entry in network['entries'] if entry['url'].endswith('/data.json')]
        self.assertEqual(len(data), 1, network)
        self.assertEqual((data[0]['status'], data[0]['state']), (200, 'done'), data)
        self.assertNotIn('headers', data[0])
        self.assertEqual(self.ok('console', since=console['next'])['data']['entries'], [], 'nothing new since')
        self.assertEqual(self.main_world('typeof globalThis.__tiloPage'), 'undefined')

    def test_file_inputs_show_themselves_and_take_chosen_files(self):
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        self.assertRegex(outline, r'file "Proof of address" \[ref=d\d+e\d+\] type=file accept="\.pdf" files=none')
        self.assertRegex(outline, r'file "Receipts" \[ref=d\d+e\d+\] type=file multiple files=none')
        receipts = self.ref(outline, 'file "Receipts"')
        proof = self.ref(outline, 'file "Proof of address"')
        with tempfile.TemporaryDirectory() as folder:
            first = os.path.join(folder, 'proof.pdf')
            second = os.path.join(folder, 'one.png')
            third = os.path.join(folder, 'two.png')
            for path, body in ((first, b'%PDF-1.4'), (second, b'\x89PNG one'), (third, b'\x89PNG two')):
                with open(path, 'wb') as file:
                    file.write(body)
            # The element's own rule is the page's rule: a script cannot pick
            # a file the page would have refused, nor a text field at all.
            refused = self.page('fill_files', element=proof, paths=[second])
            self.assertEqual((refused['ok'], refused['code']), (False, 'not_accepted'), refused)
            text = self.ref(outline, 'textbox "Email"')
            refused = self.page('fill_files', element=text, paths=[first])
            self.assertFalse(refused['ok'], refused)
            self.assertIn(text, refused['message'])
            chosen = self.ok('fill_files', element=proof, paths=[first])
            self.assertEqual(chosen['data']['files'], ['proof.pdf'], chosen)
            # The page receives the picker's own trusted events.
            self.assertEqual(self.main_world('document.getElementById("status").textContent'),
                             'files proof.pdf:8')
            self.assertRegex(self.outline(), r'file "Proof of address" .*files="proof.pdf"')
            chosen = self.ok('fill_files', element=receipts, paths=[second, third])
            self.assertEqual(chosen['data']['files'], ['one.png', 'two.png'], chosen)
            self.assertEqual(self.main_world('document.getElementById("status").textContent'),
                             'files one.png:8,two.png:8')
            # A click on a file input asks the page's own chooser for files;
            # nothing is chosen for it, so the outline stays as it was.
            clicked = self.page('click', element=receipts)
            self.assertTrue(clicked['ok'], clicked)

    def test_chosen_files_reach_the_page_with_and_without_a_held_chooser(self):
        # Choosing files names the element in the inspected document, not in a
        # node id: a ref read from the outline resolves there, and the page
        # receives the picker's own input and change events either way.
        self.h.navigate(self.url + '/form')
        with tempfile.TemporaryDirectory() as folder:
            first = os.path.join(folder, 'proof.pdf')
            second = os.path.join(folder, 'second.pdf')
            for path, body in ((first, b'%PDF-1.4'), (second, b'%PDF-1.5')):
                with open(path, 'wb') as file:
                    file.write(body)
            proof = self.ref(self.outline(), 'file "Proof of address"')
            chosen = self.ok('fill_files', element=proof, paths=[first])
            self.assertEqual(chosen['data']['files'], ['proof.pdf'], chosen)
            self.assertEqual(self.main_world('document.querySelector("[name=proof]").files[0].name'),
                             'proof.pdf')
            self.assertEqual(self.main_world('document.getElementById("status").textContent'),
                             'files proof.pdf:8')
            # A click holds the page's chooser; answering it with the same
            # action reaches the same field, so the page sees the second file.
            clicked = self.ok('click', element=proof)
            self.assertIn('file chooser', clicked['message'], clicked)
            chosen = self.ok('fill_files', element=proof, paths=[second])
            self.assertEqual(chosen['data']['files'], ['second.pdf'], chosen)
            self.assertEqual(self.main_world('document.querySelector("[name=proof]").files[0].name'),
                             'second.pdf')

    def test_a_persons_click_holds_the_chooser_and_the_embedders_answer_reaches_the_page(self):
        # A person's own click on a file input is held for the embedder that
        # asked for them: it is told what the input takes, and the paths it
        # answers with reach the page as a platform panel's would, with the
        # browser's own input and change events.
        ready = next(event for event in self.h.seen if event['event'] == 'ready')
        self.assertIs(ready.get('file_choosers'), True, ready)
        self.h.send(op='file_choosers', enabled=True)
        self.h.navigate(self.url + '/chooser')
        with tempfile.TemporaryDirectory() as folder:
            first = os.path.join(folder, 'passport.pdf')
            second = os.path.join(folder, 'one.png')
            third = os.path.join(folder, 'two.png')
            for path, body in ((first, b'%PDF-1.4'), (second, b'\x89PNG one'), (third, b'\x89PNG two')):
                with open(path, 'wb') as file:
                    file.write(body)
            asked = self.chooser('one')
            self.assertEqual(asked['mode'], 'open', asked)
            self.assertLessEqual(len(asked['title']), 256, asked)
            self.assertIn('.pdf', asked['accept'], asked)
            self.assertIn('image/*', asked['accept'], asked)
            # An answer to another chooser is no answer to this one.
            self.h.send(op='file_chooser_reply', id=asked['id'] + 1000, paths=[second])
            self.answered(asked, [first])
            self.status('one change passport.pdf:8')
            self.assertEqual(json.loads(self.main_world('JSON.stringify(window.heard)')),
                             [['one', 'input', True], ['one', 'change', True]])
            # Answered once: the same answer again reaches nothing.
            self.h.send(op='file_chooser_reply', id=asked['id'], paths=[second])

            several = self.chooser('many')
            self.assertNotEqual(several['id'], asked['id'])
            self.assertEqual((several['mode'], several['accept']), ('open_multiple', []), several)
            self.answered(several, [second, third])
            self.status('many change one.png:8,two.png:8')
            self.assertEqual(self.main_world('document.getElementById("one").files[0].name'),
                             'passport.pdf', 'the first input keeps its own file')

            # An input for one file is never handed two: the page hears a
            # cancel and keeps what it had.
            again = self.chooser('one')
            self.answered(again, [second, third])
            self.status('one cancel passport.pdf:8')

            album = self.chooser('folder')
            self.assertEqual(album['mode'], 'open_folder', album)
            self.answered(album, [])
            self.status('folder cancel ')

    def test_a_cancelled_or_abandoned_chooser_leaves_the_input_empty(self):
        self.h.send(op='file_choosers', enabled=True)
        self.h.navigate(self.url + '/chooser')
        with tempfile.TemporaryDirectory() as folder:
            late = os.path.join(folder, 'late.pdf')
            with open(late, 'wb') as file:
                file.write(b'%PDF-1.4')
            asked = self.chooser('one')
            self.answered(asked, [])
            self.status('one cancel ')
            self.assertEqual(self.main_world('document.getElementById("one").files.length'), 0)
            self.assertEqual(json.loads(self.main_world('JSON.stringify(window.heard)')),
                             [['one', 'cancel', True]])
            # The page going elsewhere lets an open chooser go, and an answer
            # that comes after reaches no page.
            waiting = self.chooser('many')
            after = len(self.h.seen)
            self.h.send(op='navigate', url=self.url + '/chooser')
            self.told('file_chooser_closed', after, lambda event: event['id'] == waiting['id'])
            self.h.until(lambda e: e['event'] == 'loaded' and e['url'] == self.url + '/chooser')
            self.h.settle()
            self.h.send(op='file_chooser_reply', id=waiting['id'], paths=[late])
            self.assertEqual(self.main_world('document.getElementById("many").files.length'), 0)
            self.assertEqual(self.main_world('window.heard.length'), 0)
            # An embedder that stops answering lets the open chooser go too.
            waiting = self.chooser('one')
            after = len(self.h.seen)
            self.h.send(op='file_choosers', enabled=False)
            self.told('file_chooser_closed', after, lambda event: event['id'] == waiting['id'])
            self.status('one cancel ')

    def test_no_page_action_answers_a_persons_chooser(self):
        # The embedder's picker is a person's alone: files an action chooses
        # while a person's chooser is open go to the input the action names,
        # never into that chooser, which goes on waiting for the person.
        self.h.send(op='file_choosers', enabled=True)
        self.h.navigate(self.url + '/chooser')
        with tempfile.TemporaryDirectory() as folder:
            first = os.path.join(folder, 'passport.pdf')
            second = os.path.join(folder, 'one.png')
            for path, body in ((first, b'%PDF-1.4'), (second, b'\x89PNG one')):
                with open(path, 'wb') as file:
                    file.write(body)
            one = self.ref(self.outline(), 'file "Passport"')
            waiting = self.chooser('many')
            after = len(self.h.seen)
            self.ok('fill_files', element=one, paths=[second])
            self.status('one change one.png:8')
            self.h.settle()
            self.assertFalse([event for event in self.h.seen[after:] if event['event'] == 'file_chooser_closed'],
                             'an action answered a person\'s chooser')
            self.assertEqual(self.main_world('document.getElementById("many").files.length'), 0)
            self.answered(waiting, [first])
            self.status('many change passport.pdf:8')

    @unittest.skipIf(sys.platform == 'darwin',
                     'a chooser that comes after its action answered gets the macOS host\'s own panel')
    def test_an_actions_click_never_opens_the_embedders_picker(self):
        # A page often opens its chooser only once the click that asked for
        # it has answered. Held for fill_files or come too late for that, a
        # chooser an action's click opened is never told to the embedder, so
        # nothing an agent does puts a picker in front of the person; the
        # person's own click after it still does.
        self.h.send(op='file_choosers', enabled=True)
        self.h.navigate(self.url + '/chooser')
        with tempfile.TemporaryDirectory() as folder:
            first = os.path.join(folder, 'passport.pdf')
            with open(first, 'wb') as file:
                file.write(b'%PDF-1.4')
            one = self.ref(self.outline(), 'file "Passport"')
            # The page hears clicks, and the last it heard was a person's:
            # the action's own click after it is still the action's.
            self.person_clicks('blank')
            for turn in ('on a page a person only clicked', 'after a person\'s own chooser'):
                after = len(self.h.seen)
                clicked = self.ok('click', element=one)
                if 'file chooser' in clicked['message']:
                    self.ok('fill_files', element=one, paths=[first])
                    self.status('one change passport.pdf:8')
                else:
                    self.status('one cancel ' + ('' if turn.startswith('on') else 'passport.pdf:8'))
                self.h.settle(1)
                self.assertFalse([event for event in self.h.seen[after:] if event['event'] == 'file_chooser'],
                                 f'an action\'s chooser reached the embedder {turn}')
                if turn.startswith('on'):
                    waiting = self.chooser('one')
                    self.answered(waiting, [first])
                    self.status('one change passport.pdf:8')

    @unittest.skipIf(sys.platform == 'darwin', 'a tab that never asked gets the macOS host\'s own panel')
    def test_a_tab_that_never_asked_has_a_persons_chooser_cancelled_at_once(self):
        # Every page is offscreen here and the host has no panel: without an
        # embedder to answer, the page hears a cancel rather than waiting.
        self.h.navigate(self.url + '/chooser')
        after = len(self.h.seen)
        self.person_clicks('one')
        self.status('one cancel ')
        self.assertFalse([event for event in self.h.seen[after:] if event['event'] == 'file_chooser'])

    def test_an_agent_buys_one_item_in_a_shop_to_the_order_s_confirmation(self):
        # A purchase from the search to the confirmation, through page actions
        # alone: a form is read and answered as a form, what stands in no form
        # is operated by its own action, and the card number goes the secret's
        # way and is nowhere in what the tab keeps or answers.
        shop = self.server.shop = {'bag': [], 'orders': []}
        card = '4242424242424242'
        self.h.navigate(self.url + '/shop')
        # The search is a form.
        search = self.ok('forms', actor='ada')['data']['forms'][0]
        self.assertEqual([field['name'] for field in search['fields']], ['Search the shop'])
        found = self.ok('fill_form', form=search['form'], answers={'Search the shop': 'boots'}, submit=True, actor='ada')
        self.assertEqual((found['data']['submitted'], found['navigation'], urlparse(found['url']).path),
                         (True, 'started', '/shop/search'), found)
        # The results ask nothing: the product is a link, found by its name.
        self.assertEqual(self.ok('forms', actor='ada')['data']['forms'], [])
        product = self.find(role='link', name='Walking boots', actor='ada')
        opened = self.ok('click', element=product['data']['element'], actor='ada')
        self.assertEqual((opened['navigation'], urlparse(opened['url']).path), ('started', '/shop/p/walking-boots'), opened)
        # The product page keeps its size and its button in no form: each is
        # operated by its own action, and the button takes no click before a
        # size is chosen.
        outline = self.ok('outline', actor='ada')['outline']
        self.assertIn('heading "Walking boots" level=1', outline)
        size, add = self.ref(outline, 'combobox "Size"'), self._disabled(outline, 'button "Add to bag"')
        early = self.page('click', element=add, actor='ada')
        self.assertFalse(early['ok'], early)
        self.assertEqual(shop['bag'], [])
        self.ok('select', element=size, values=['43'], actor='ada')
        self.ok('click', element=add, actor='ada')
        # The page adds the item and goes to the bag by itself.
        bag = self.reads('heading "Your bag" level=1', 'the bag never opened', actor='ada')['outline']
        self.assertIn('Walking boots, size 43, EUR 129', bag)
        self.assertIn('Total EUR 129', bag)
        self.assertEqual(shop['bag'], [{'sku': 'walking-boots', 'size': '43'}])
        checkout = self.ok('click', element=self.ref(bag, 'link "Checkout"'), actor='ada')
        self.assertEqual(urlparse(checkout['url']).path, '/shop/checkout')
        # The checkout is a form: its questions by name, the card number a
        # secret the form action does not place.
        asked = self.ok('forms', actor='ada')['data']['forms'][0]
        fields = {field['name']: field for field in asked['fields']}
        self.assertEqual(list(fields), ['Full name', 'Street', 'City', 'Postcode', 'Country', 'Delivery', 'Card number'])
        self.assertEqual((fields['Card number']['kind'], fields['Card number']['required']), ('secret', True))
        self.assertEqual(fields['Delivery']['choices'], ['Standard', 'Express'])
        answered = self.ok('fill_form', form=asked['form'], actor='ada', answers={
            'Full name': 'Ada Lovelace', 'Street': '12 Linden Street', 'City': 'Utrecht', 'Postcode': '3511 AB',
            'Country': 'Netherlands', 'Delivery': 'Express'})
        self.assertEqual([result['ok'] for result in answered['data']['results']], [True] * 6, answered)
        self.assertEqual(answered['data']['unanswered'], ['Card number'])
        # Written as ordinary text the card number is refused, and what was
        # refused is no part of the record; it is filled as a secret.
        refused = self.page('fill', element=fields['Card number']['element'], text=card, actor='ada')
        self.assertEqual((refused['ok'], refused['code']), (False, 'secret_field'), refused)
        self.ok('fill', element=fields['Card number']['element'], text=card, secret=True, actor='ada')
        # A reviewer reads the form before it is sent: every field with what
        # it holds and who set it, the card number by its length alone.
        review = self.ok('fields', actor='ben')
        held = {field['name']: field for field in review['data']['fields']}
        self.assertEqual((held['Full name']['value'], held['Full name']['set']['by']), ('Ada Lovelace', 'ada'))
        self.assertEqual(held['Card number']['set']['by'], 'ada')
        self.assertIn('16', held['Card number']['value'])
        self.assertNotIn(card, json.dumps(review))
        self.assertNotIn(card, self.ok('outline', actor='ben')['outline'])
        # The form is the one that was read, and it is sent by its button.
        sent = self.ok('fill_form', form=asked['signature'], answers={}, submit='Place order', actor='ada')
        self.assertEqual((sent['data']['submitted'], sent['navigation'], urlparse(sent['url']).path),
                         (True, 'started', '/shop/order/1001'), sent)
        confirmation = self.ok('outline', actor='ada')['outline']
        self.assertIn('heading "Order 1001 confirmed" level=1', confirmation)
        self.assertIn('Walking boots, size 43, EUR 129', confirmation)
        self.assertIn('Delivered to 12 Linden Street, 3511 AB Utrecht, NL by express delivery.', confirmation)
        self.assertIn('Paid with the card ending 4242.', confirmation)
        # The shop received one order, as a person's would have arrived.
        self.assertEqual(shop['orders'], [{
            'name': 'Ada Lovelace', 'street': '12 Linden Street', 'city': 'Utrecht', 'postcode': '3511 AB',
            'country': 'NL', 'delivery': 'express', 'card': card,
            'items': [{'sku': 'walking-boots', 'size': '43'}], 'number': 1001}])
        self.assertEqual(shop['bag'], [])
        # The record holds every step of it in order, and the card number is
        # in none of them, nor in any answer.
        record = self.ok('ledger', since=0)
        entries = record['data']['entries']
        self.assertEqual([(entry['actor'], entry['action'], entry['ok']) for entry in entries], [
            ('ada', 'fill_form', True), ('ada', 'click', True), ('ada', 'click', False), ('ada', 'select', True),
            ('ada', 'click', True), ('ada', 'click', True), ('ada', 'fill_form', True), ('ada', 'fill', False),
            ('ada', 'fill', True), ('ada', 'fill_form', True)])
        self.assertEqual(entries[8]['target'], {'role': 'textbox', 'name': 'Card number'})
        self.assertNotIn('text', entries[7])
        self.assertNotIn('text', entries[8])
        self.assertEqual(entries[9]['to_url'], sent['url'])
        for answer in (record, refused, answered, sent, confirmation):
            self.assertNotIn(card, json.dumps(answer))

    def test_secrets_and_cross_site_submissions_need_explicit_consent(self):
        self.h.navigate(self.url + '/agent')
        outline = self.outline()
        pin = self.ref(outline, 'textbox "Pin"')
        self.assertEqual(self.page('fill', element=pin, text='1234')['code'], 'secret_field')
        self.ok('fill', element=pin, text='1234', secret=True)
        query = self.ref(outline, 'textbox "Query"')
        for action, fields in (('submit', {}), ('click', {})):
            target = query if action == 'submit' else self.ref(outline, 'button "Send away"')
            refused = self.page(action, element=target, **fields)
            self.assertFalse(refused['ok'], (action, refused))
            self.assertEqual((refused['ok'], refused['code']), (False, 'cross_origin_submit'), refused)
            self.assertIn('elsewhere.test', refused['message'])
        entered = self.page('press', element=query, key='Enter', ctrl=False, shift=False, alt=False, meta=False)
        self.assertEqual(entered['code'], 'cross_origin_submit', entered)

    def pointer(self, steps, button='left'):
        return self.ok('pointer', steps=steps, button=button)

    def test_pointer_hover_reveals_a_menu_and_the_click_opens_it(self):
        self.h.navigate(self.url + '/gesture')
        outline = self.outline()
        self.assertNotIn('Fishing licence', outline, 'the menu is closed until a hand rests on it')
        # The page keeps the menu shut until the pointer rests on its owner,
        # so the outline names that owner and a gesture reaches it by ref.
        resting = self.ref(outline, 'hoverable "Licences"')
        self.pointer([dict(kind='move', element=resting, x=5, y=5)])
        revealed = self.outline()
        fishing = self.ref(revealed, 'link "Fishing licence"')
        self.pointer([dict(kind='move', element=fishing, x=5, y=5),
                      dict(kind='down', element=fishing, x=5, y=5),
                      dict(kind='up', element=fishing, x=5, y=5)])
        self.assertEqual(self.main_world('location.pathname'), '/fishing', 'the click followed the link')

    def test_pointer_drags_a_page_element_onto_another(self):
        self.h.navigate(self.url + '/gesture')
        outline = self.outline()
        central = self.ref(outline, 'draggable "Central office"')
        riverton = self.ref(outline, 'draggable "Riverton office"')
        self.pointer([dict(kind='down', element=central, x=5, y=5),
                      dict(kind='move', element=riverton, x=5, y=5),
                      dict(kind='up', element=riverton, x=5, y=5)])
        order = self.main_world("[...document.querySelectorAll('#rank li')].map(li => li.textContent).join(' > ')")
        self.assertEqual(order, 'Central office > Riverton office > Porthaven office', order)

    def test_pointer_draws_on_a_canvas(self):
        self.h.navigate(self.url + '/gesture')
        pad = self.ref(self.outline(), 'canvas "Signature pad"')
        steps = []
        for line in range(3):
            steps.append(dict(kind='down', element=pad, x=20, y=20 + line * 20))
            steps.append(dict(kind='move', element=pad, x=120, y=20 + line * 20))
            steps.append(dict(kind='up', element=pad, x=120, y=20 + line * 20))
        self.pointer(steps)
        self.assertIn('text "strokes 3"', self.outline(), 'each press and release is one stroke')

    def test_scrolling_to_a_lazy_section_loads_it(self):
        self.h.navigate(self.url + '/scroll')
        self.assertNotIn('Condition 14', self.outline(), 'the section loads only once it is scrolled to')
        # The wheel loads the section below the fold as a hand's scroll would:
        # the section's observer runs when the turn puts it on screen, and the
        # page it loads goes on to extend the page, which the wheel follows.
        self.ok("scroll", by=1000)
        self.assertIn('Condition 14', self.outline(), 'the wheel loaded the section it scrolled to')
        self.assertTrue(self.main_world('scrollY > 0'), 'the page itself moved')

    def test_scrolling_by_a_distance_goes_that_far_each_time_and_says_where_it_is(self):
        # A reader goes down a long page a screen at a time. Each scroll by a
        # distance goes that far from wherever the page stands, down or up,
        # and says where it left the page, on a page of today and on one that
        # scrolls its body as old pages do.
        for page in ('/long', '/old'):
            with self.subTest(page=page):
                self.h.navigate(self.url + page)
                self.ok('outline')
                for stands in (700, 1400, 2100):
                    moved = self.ok('scroll', by=700)
                    self.assertEqual(self.main_world('scrollY'), stands, moved['message'])
                    self.assertTrue(moved['message'].startswith(f'Scrolled 700px down, to {stands} of '), moved['message'])
                back = self.ok('scroll', by=-1000)
                self.assertEqual(self.main_world('scrollY'), 1100, back['message'])
                self.assertTrue(back['message'].startswith('Scrolled 1000px up, to 1100 of '), back['message'])
                # At the top there is nowhere up to go, and the answer says so.
                self.ok('scroll', to='top')
                self.assertEqual(self.main_world('scrollY'), 0)
                still = self.ok('scroll', by=-500)
                self.assertTrue(still['message'].startswith('Did not move, at 0 of '), still['message'])
                self.assertEqual(self.main_world('scrollY'), 0)

    def test_a_scroll_of_a_page_not_read_yet_does_not_answer_with_the_whole_page(self):
        self.h.navigate(self.url + '/long')
        moved = self.ok('scroll', by=700)
        self.assertTrue(moved['message'].startswith('Scrolled 700px down, to 700 of '), moved['message'])
        self.assertIn('had not been read before', moved['message'])
        self.assertFalse(moved['outline'])
        # What the page showed then is what the next scroll is told against.
        self.assertIn('nothing changed', self.ok('scroll', by=700)['message'])

    def test_scrolling_back_up_returns_to_the_top(self):
        self.h.navigate(self.url + '/scroll')
        self.ok('scroll', to='bottom')
        self.assertTrue(self.main_world('scrollY > 0'))
        self.ok('scroll', to='top')
        self.assertEqual(self.main_world('scrollY'), 0)

    def test_a_virtualized_table_renders_the_rows_scrolled_to(self):
        self.h.navigate(self.url + '/scroll')
        self.assertIn('Invoice 6', self.main_world("[...document.querySelectorAll('#rows div')].map(d => d.textContent).join(', ')"))
        # Rows are rendered as the list is scrolled, and its own scroll
        # listener renders the rows further on as the wheel reaches them.
        invoices = self.ref(self.outline(), 'region "Invoices"')
        bottom = self.ok('scroll', to='bottom')
        scrolled = self.main_world('scrollY')
        # The wheel turns over the list until Invoice 60 comes into view.
        turned = self.ok('scroll', element=invoices, by=2300)
        self.assertIn('Invoice 60 (overdue)', self.outline(),
                      f'the rows the wheel reached are rendered: {bottom["message"]}; then {turned["message"]}')
        self.assertEqual(self.main_world('scrollY'), scrolled, 'the list scrolled, not the page')

    def test_an_element_that_cannot_hear_a_scroll_is_refused(self):
        self.h.navigate(self.url + '/scroll')
        # The list's box is below the fold, so its own wheel cannot turn yet:
        # the request says so instead of turning the page's wheel instead.
        invoices = self.ref(self.outline(), 'region "Invoices"')
        refused = self.page('scroll', element=invoices, by=400)
        self.assertEqual((refused['ok'], refused['code']), (False, 'not_visible'), refused)
        self.assertEqual(self.main_world("document.getElementById('rows').scrollTop"), 0)

    def test_a_scroll_that_says_nothing_or_two_things_is_refused(self):
        self.h.navigate(self.url + '/scroll')
        # Where one scroll goes is said once, and what is outside the bounds a
        # scroll may ask for is answered rather than done. A refusal answers in
        # its turn, so the keys of a scroll are only the ones that make one:
        # what is not refused here is a scroll the page itself turns down.
        for fields in ({}, {'to': 'sideways'}, {'to': 'top', 'by': 100},
                       {'into_view': 'd1e2', 'by': 100}, {'by': 20000}):
            refused = self.page('scroll', **fields)
            self.assertEqual((refused['ok'], refused['code']), (False, 'invalid_request'),
                             (fields, refused))
            self.assertEqual(self.main_world('scrollY'), 0, f'{fields} moved the page')
        self.ok('scroll', by=200)
        self.assertTrue(self.main_world('scrollY') > 0, 'a scroll after a refusal still runs')

    def test_an_element_is_brought_into_view(self):
        self.h.navigate(self.url + '/scroll')
        # A row the page has rendered but the list clips is brought into view
        # by the same wheel a person turns over it.
        invoices = self.ref(self.outline(), 'region "Invoices"')
        bottom = self.ok('scroll', to='bottom')
        # The wheel turns over the list until Invoice 60 comes into view.
        turned = self.ok('scroll', element=invoices, by=2300)
        self.assertRegex(self.outline(), 'Invoice 6[0-9]',
                         f'the wheel reached the rows: {bottom["message"]}; then {turned["message"]}')
        later = self.near('Invoice 6[0-9]')
        self.ok('scroll', into_view=later)
        self.assertIn('Invoice 60 (overdue)', self.outline(), 'the wheel brought the row into view')

    def test_a_scroll_past_a_focused_control_moves_the_page(self):
        self.h.navigate(self.url + '/scroll')
        # A click focuses a control, and from then on the page's own focused
        # element eats End and PageDown; the wheel does not care who has the
        # keyboard.
        button = self.ref(self.outline(), 'button "Show terms"')
        self.ok('click', element=button)
        self.ok('scroll', by=600)
        self.assertGreater(self.main_world('scrollY'), 0)

    def test_a_scroll_that_says_nothing_bounded_is_refused(self):
        self.h.navigate(self.url + '/scroll')
        # A distance is bounded, an end of the page is one of two names, and a
        # scroll says where it goes once; a page request outside those bounds
        # is answered as failed, not run.
        for fields in (dict(by=20000), dict(to='sideways'), dict(to='top', by=100), dict()):
            result = self.page('scroll', **fields)
            self.assertFalse(result['ok'], result)
        self.assertEqual(self.main_world('scrollY'), 0, 'the wheel moved the page')

    def test_a_colour_input_is_read_as_a_colour_and_filled_with_one(self):
        self.h.navigate(self.url + '/native')
        outline = self.outline()
        # A colour input is not a button: it holds one colour, and the outline
        # names it so an agent knows what it is and what it holds.
        self.assertRegex(outline, r'color "Favourite colour" \[ref=d\d+e\d+\] type=color value="#000000"')
        self.assertNotIn('button "Favourite colour"', outline)
        colour = self.ref(outline, 'color "Favourite colour"')
        filled = self.ok('fill', element=colour, text='#1d70b8')
        self.assertIn('#1d70b8', filled['message'], filled)
        # The page hears it as the person choosing the colour: input then change.
        self.assertEqual(self.main_world('document.querySelector("[name=colour]").value'), '#1d70b8')
        self.assertEqual(self.main_world('document.getElementById("said").textContent'),
                         'colour change #1d70b8')
        self.assertIn('value="#1d70b8"', self.outline(), 'the outline shows the colour it now holds')
        # Named colours and the shorthand are the same colour.
        self.ok('fill', element=self.ref(self.outline(), 'color "Medium"'), text='navy')
        self.assertEqual(self.main_world('document.querySelector("[name=medium]").value'), '#000080')
        # The shorthand names the same colour, which an input holds in full.
        self.ok('fill', element=colour, text='#abc')
        self.assertEqual(self.main_world('document.querySelector("[name=colour]").value'), '#aabbcc')
        wrong = self.page('fill', element=colour, text='reddish')
        self.assertEqual((wrong['ok'], wrong['code']), (False, 'invalid_color'), wrong)
        self.assertIn('#rrggbb', wrong['message'])
        self.assertIn('#1d70b8', wrong['message'], 'the error shows what a colour looks like')

    def test_an_action_never_opens_the_platform_colour_or_file_chooser(self):
        self.h.navigate(self.url + '/native')
        outline = self.outline()
        colour = self.ref(outline, 'color "Favourite colour"')
        proof = self.ref(outline, 'file "Proof"')
        # A colour or file input opens a chooser the agent cannot see, so an
        # action that would open one says which action does the job instead.
        for action in ('click', 'press'):
            fields = dict(element=colour) if action == 'click' else dict(element=colour, key='Enter',
                                                                        ctrl=False, shift=False, alt=False, meta=False)
            refused = self.page(action, **fields)
            self.assertEqual((refused['ok'], refused['code']), (False, 'native_chooser'), refused)
            self.assertIn('fill-page', refused['message'], refused)
        # A file input's chooser is not the platform's to open either: the
        # press finishes rather than waiting on a dialog no agent can answer,
        # and fill-page-files is what puts files in the input.
        clicked = self.page('press', element=proof, key='Enter',
                            ctrl=False, shift=False, alt=False, meta=False)
        self.assertTrue(clicked['ok'], clicked)

    def test_a_key_is_named_in_any_letter_case(self):
        self.h.navigate(self.url + '/scroll')
        rows = self.ref(self.outline(), 'region "Invoices"')
        self.ok('click', element=rows)
        # The name the caller writes is the key the page is sent: the case is
        # not part of the name, so END is End rather than an E, an N and a D.
        NAMED = [('END', 'End'), ('End', 'End'), ('end', 'End'), ('ENd', 'End'),
                 ('PAGEDOWN', 'PageDown'), ('pageup', 'PageUp'),
                 ('arrowdown', 'ArrowDown'), ('ARROWUP', 'ArrowUp'),
                 ('ESCAPE', 'Escape'), ('backspace', 'Backspace')]
        for written, arrived in NAMED:
            before = self.main_world('window.pressed.length')
            self.ok('press', element=rows, key=written, ctrl=False, shift=False, alt=False, meta=False)
            self.assertEqual(self.main_world(f'window.pressed[{before}]'), arrived,
                             f'{written} arrives as {arrived}')
        # A letter is still the letter it is written as, case and all.
        for written, arrived in [('a', 'a'), ('A', 'A')]:
            before = self.main_world('window.pressed.length')
            self.ok('press', element=rows, key=written, ctrl=False, shift=False, alt=False, meta=False)
            self.assertEqual(self.main_world(f'window.pressed[{before}]'), arrived, written)
        # A key the browser has never heard of is named back, with the keys it
        # could have meant, rather than sent as a key nothing answers.
        unknown = self.page('press', element=rows, key='ENDD',
                            ctrl=False, shift=False, alt=False, meta=False)
        self.assertEqual((unknown['ok'], unknown['code']), (False, 'invalid_key'), unknown)
        for near in ('ENDD', 'End', 'PageDown', 'Enter', 'any letter case'):
            self.assertIn(near, unknown['message'], unknown)
        # Nothing was sent for the key that does not exist.
        self.assertEqual(self.main_world('window.pressed.length'), len(NAMED) + 2, unknown)

    def test_a_long_page_says_how_much_it_left_out(self):
        self.h.navigate(self.url + '/long')
        read = self.ok('outline')
        self.assertIn('Condition 1.', read['outline'])
        # A read stops at its cap, and says so where the caller reads first: a
        # reader that never saw those lines cannot act on what is in them.
        self.assertIn('Condition 600.', read['outline'], 'the read carries its cap')
        self.assertNotIn('Condition 2000.', read['outline'])
        self.assertIn('stopped there', read['message'], read)
        self.assertIn('100000 characters of outline', read['message'], read)
        self.assertIn('the page goes on past them', read['message'], read)
        self.assertIn('read-page-element-at', read['message'], read)
        # The outline's own last line says the same, so a reader that only
        # looks at the outline is not left believing it saw the whole page.
        self.assertIn('the page goes on past them', read['outline'].splitlines()[-1], read['outline'][-400:])
        # A page that fits is read whole, and says nothing about a cut.
        self.h.navigate(self.url + '/native')
        short = self.ok('outline')
        self.assertNotIn('stopped there', short['message'], short)
        self.assertNotIn('the page goes on', short['outline'])

    def test_a_paragraph_is_read_whole(self):
        # A page's own words are what a reader came for. A paragraph cut after
        # a sentence or two cannot be read at all, however often it is read.
        self.h.navigate(self.url + '/essay')
        outline = self.outline()
        self.assertIn('Sentence 1 of this paragraph', outline)
        self.assertIn('The last sentence is the one that matters: read to the end.', outline)
        self.assertNotIn('…', outline)

    def test_running_text_is_read_to_its_last_word(self):
        # A page that writes an essay as one run of text, its paragraphs
        # parted by line breaks, is read paragraph by paragraph, and a
        # paragraph longer than a line carries goes on in the next line:
        # no word of it is left out.
        self.h.navigate(self.url + '/run')
        lines = [json.loads(line.split('text ', 1)[1]) for line in self.outline().splitlines() if 'text "' in line]
        self.assertEqual(lines[0], 'First paragraph, a short one.')
        self.assertEqual(lines[-1], 'Last paragraph: the point of it all.')
        middle = lines[1:-1]
        self.assertGreater(len(middle), 1, 'the long paragraph is carried by several lines')
        self.assertEqual(' '.join(middle), ' '.join(RUN_SENTENCES))
        # Each line ends where a sentence does, and none says it was cut.
        for line in middle:
            self.assertTrue(line.endswith('stops.'), line[-60:])
            self.assertLessEqual(len(line), 2000)
        self.assertEqual(self.ok('search', text='Sentence 80 of the long paragraph')['data']['total'], 1)

    def test_a_long_page_is_read_on_from_where_a_read_stopped(self):
        # A read carries as much as one read may, and says the line to read
        # on from; reading on carries the next part, to the end of the page.
        self.h.navigate(self.url + '/long')
        first = self.ok('outline', actor='ada')
        last = max(int(number) for number in re.findall(r'Condition (\d+)\.', first['outline']))
        self.assertLess(last, 2000)
        onward = re.search(r'read on from line (\d+) with from', first['message'])
        self.assertIsNotNone(onward, first['message'])
        rest = self.ok('outline', actor='ada', **{'from': int(onward.group(1))})
        # Nothing is skipped and nothing is read twice.
        self.assertIn(f'Condition {last + 1}.', rest['outline'].splitlines()[0])
        self.assertNotIn(f'Condition {last}.', rest['outline'])
        self.assertTrue(rest['message'].startswith(f'Read lines {onward.group(1)} to '), rest['message'])
        reads = 1
        while 'the page goes on past them' in rest['outline']:
            onward = re.search(r'read on from line (\d+) with from', rest['message'])
            rest = self.ok('outline', actor='ada', **{'from': int(onward.group(1))})
            reads += 1
            self.assertLess(reads, 10)
        self.assertIn('Condition 2000.', rest['outline'])
        self.assertNotIn('stopped there', rest['message'])
        # Reading on is not a new look at the page: what changed is still
        # told against the read from the top.
        self.assertEqual(self.ok('changes', actor='ada')['message'], 'Nothing changed')
        # A line the page does not have is not one to read from.
        beyond = self.page('outline', **{'from': 5000})
        self.assertFalse(beyond['ok'])
        self.assertIn('there is no line 5000', beyond['message'])

    def test_words_on_a_page_are_found_by_searching_for_them(self):
        # A reader looking for a sentence goes to it: a search answers with
        # the lines that hold the words, and a read starts from one of them.
        self.h.navigate(self.url + '/long')
        found = self.ok('search', text='Section 1500 of the ACT')
        self.assertEqual(found['data']['total'], 1)
        hit = found['data']['matches'][0]
        self.assertIn('Condition 1500.', hit['text'])
        self.assertEqual(found['outline'], f"{hit['line']}: {hit['text']}")
        around = self.ok('outline', **{'from': hit['line']})
        self.assertIn('Condition 1500.', around['outline'].splitlines()[0])
        # Many lines are counted, and as many listed as one answer carries.
        many = self.ok('search', text='within 30 days')
        self.assertEqual(many['data']['total'], 2000)
        self.assertEqual(len(many['data']['matches']), 30)
        self.assertIn('the first 30 listed', many['message'])
        nothing = self.ok('search', text='no such words here')
        self.assertEqual(nothing['data'], {'matches': [], 'total': 0})
        self.assertIn('search for fewer of them', nothing['message'])
        # A find that was asked for words says where words are found.
        missed = self.page('find', role='paragraph')
        self.assertFalse(missed['ok'])
        self.assertIn('found by searching for them', missed['message'])
        # A search reads what the outline reads: a secret is in neither.
        self.h.navigate(self.url + '/form')
        password = self.ref(self.outline(), 'textbox "Password"')
        self.ok('fill', element=password, text='hunter2-secret', secret=True)
        self.assertEqual(self.ok('search', text='hunter2')['data']['total'], 0)
        self.assertEqual(self.ok('search', text='place order')['data']['total'], 1)
        # Searching is a read: it took no place in the tab's record.
        self.assertEqual([entry['action'] for entry in self.ok('ledger', since=0)['data']['entries']], ['fill'])

    def find(self, **fields):
        # A find is a read of the page: it answers with a ref, or with why it
        # could not say which element was meant.
        return self.page('find', **fields)


    def test_a_find_names_an_element_by_what_the_page_says_it_is(self):
        self.h.navigate(self.url + '/finds')
        outline = self.outline()
        # What the page holds, read once: every find below answers the ref
        # the same page's own outline prints.
        # A label names its control, and the same words find it.
        by_name = self.find(name='Email address')
        self.assertTrue(by_name['ok'], by_name)
        self.assertEqual(by_name['data']['element'], self.ref(outline, 'textbox "Email address"'), by_name)
        self.assertEqual(by_name['data']['role'], 'textbox')
        self.assertEqual(by_name['data']['name'], 'Email address')
        # The line is the outline's own line, with the words of the box it is
        # printed under, so what is answered is what a reader can find in a
        # read of the page.
        self.assertIn('textbox "Email address"', by_name['data']['line'], by_name)
        self.assertIn(by_name['data']['line'].replace('group "Contact": ', ''), outline,
                      'the answer is the outline\'s own line, under the box it is printed in')
        self.assertIn('Email address', by_name['message'])
        # The ref a find answers is a ref like any other: a step fills with
        # it, and what was typed is on the page where the caller left it.
        self.ok('fill', element=by_name['data']['element'], text='ada@example.test')
        self.assertIn('value="ada@example.test"', self.outline())
        # A role and a name together pick one element out of the page.
        button = self.find(role='button', name='Place booking')
        self.assertTrue(button['ok'], button)
        self.assertEqual(button['data']['element'], self.ref(self.outline(), 'button "Place booking"'))
        # What sits under a bar is found by the bar's words too, as its own
        # line is printed under them.
        nested = self.find(name='Telephone')
        self.assertTrue(nested['ok'], nested)
        self.assertEqual(nested['data']['element'], self.ref(self.outline(), 'textbox "Telephone"'))
        # A role alone names every element of that kind, so it is numbered
        # like any other description with more than one match.
        by_role = self.find(role='button')
        self.assertEqual((by_role['ok'], by_role['code']), (False, 'ambiguous'), by_role)
        first = self.find(role='button', nth=1)
        self.assertTrue(first['ok'], first)
        self.assertEqual(first['data']['element'], self.ref(self.outline(), 'button "Remove"'))
        self.assertEqual(first['data']['nth'], 1)

    def test_a_find_matches_a_name_case_and_part_ways(self):
        self.h.navigate(self.url + '/finds')
        outline = self.outline()
        email = self.ref(outline, 'textbox "Email address"')
        # Exactly as printed first, then in any letter case, then by a part.
        for written in ('Email address', 'email ADDRESS', 'mail addr'):
            found = self.find(name=written)
            self.assertTrue(found['ok'], (written, found))
            self.assertEqual(found['data']['element'], email, written)
        # An exact match wins over a looser one that would have matched too.
        self.assertEqual(self.find(name='Newsletter')['data']['role'], 'checkbox')

    def test_a_find_numbers_equal_matches_and_refuses_to_guess(self):
        self.h.navigate(self.url + '/finds')
        outline = self.outline()
        removes = re.findall(r'button "Remove" \[ref=(d\d+e\d+)\]', outline)
        self.assertEqual(len(removes), 2, outline)
        # Several equal matches are refused, and the error lists each with the
        # number that names it.
        refused = self.find(role='button', name='Remove')
        self.assertEqual((refused['ok'], refused['code']), (False, 'ambiguous'), refused)
        for number, ref in enumerate(removes, start=1):
            self.assertIn(f'({number}) group "Guests": button "Remove"', refused['message'], refused)
            self.assertIn(f'[ref={ref}]', refused['message'], refused)
        self.assertIn('nth', refused['message'], refused)
        # The number picks the one meant, in the order the outline prints them.
        for number, ref in enumerate(removes, start=1):
            found = self.find(role='button', name='Remove', nth=number)
            self.assertTrue(found['ok'], found)
            self.assertEqual(found['data']['element'], ref, number)
            self.assertEqual(found['data']['nth'], number)
        # A number past the last names how many there are.
        past = self.find(role='button', name='Remove', nth=3)
        self.assertEqual((past['ok'], past['code']), (False, 'not_found'), past)
        self.assertIn('from 1 to 2', past['message'], past)
        # A description the page does not hold names what it nearly said
        # instead of any element at all.
        missing = self.find(role='button', name='Delete')
        self.assertEqual((missing['ok'], missing['code']), (False, 'not_found'), missing)
        self.assertIn('Remove', missing['message'], missing)
        self.assertIn('button "Place booking"', missing['message'], missing)
        self.assertNotIn('element', missing, 'a find that found nothing answers no ref')
        # A description of nothing names no element, and says what a
        # description needs rather than guessing at one.
        empty = self.page('find')
        self.assertEqual((empty['ok'], empty['code']), (False, 'invalid_request'), empty)
        self.assertIn('role', empty['message'], empty)
        self.assertIn('name', empty['message'], empty)

    def test_a_find_understands_a_description_after_the_page_has_reloaded(self):
        self.h.navigate(self.url + '/finds')
        first = self.find(role='textbox', name='Telephone')['data']['element']
        self.ok('fill', element=first, text='01234')
        self.h.navigate(self.url + '/finds')
        # A reloaded document has refs of its own; the description finds the
        # element again, and the ref from before it is no longer the page's.
        again = self.find(role='textbox', name='Telephone')
        self.assertTrue(again['ok'], again)
        self.assertNotEqual(again['data']['element'], first, 'a reloaded page has refs of its own')
        self.assertEqual(again['data']['element'], self.ref(self.outline(), 'textbox "Telephone"'))
        # The whole of what a replayed step does: find, then act on the ref.
        self.ok('fill', element=again['data']['element'], text='01234')
        self.assertIn('value="01234"', self.outline())
        # The old ref is refused, never resolved against whatever is there now.
        stale = self.page('fill', element=first, text='999')
        self.assertEqual((stale['ok'], stale['code']), (False, 'stale_element'), stale)

    def test_a_find_reaches_inside_an_open_shadow_root(self):
        self.h.navigate(self.url + '/form')
        # The outline prints what a shadow root holds; a find reaches it the
        # same way a read does, without the caller knowing there is a root.
        found = self.find(role='button', name='Shadow button')
        self.assertTrue(found['ok'], found)
        self.assertEqual(found['data']['element'], self.ref(self.outline(), 'button "Shadow button"'))
        self.ok('click', element=found['data']['element'])
        self.assertIn('text "shadow clicked"', self.outline())

    def test_a_read_follows_a_slot_to_what_it_was_given(self):
        # A component hands its own children to a slot of its shadow tree,
        # and a person reads them where the slot stands. A read does too.
        self.h.navigate(self.url + '/shadow')
        outline = self.outline()
        self.assertIn('heading "Simple template" level=1', outline)
        self.assertIn('text "Let\'s have some different text!"', outline)
        self.assertIn('listitem: "Slotted first item"', outline)
        self.assertIn('listitem: "In a list!"', outline)
        # A slot given nothing shows its own words, one given something
        # never does, and a child no slot takes is not on the page.
        self.assertEqual(outline.count('text "My default text"'), 2, outline)
        self.assertNotIn('Never given a slot', outline)
        # What a read prints, a search finds.
        found = self.ok('search', text='in a LIST')
        self.assertEqual(found['data']['total'], 1, found)
        self.assertIn('In a list!', found['data']['matches'][0]['text'])
        self.assertEqual(self.ok('search', text='never given')['data']['total'], 0)

    def test_a_read_follows_one_shadow_tree_into_another(self):
        # A component built of components: the card's shadow tree holds a box
        # with a shadow tree of its own, and hands on what the page handed it.
        self.h.navigate(self.url + '/shadow')
        outline = self.outline()
        self.assertIn('heading "Card title" level=2', outline)
        self.assertRegex(outline, r'link "Read more" \[ref=d\d+e\d+\] -> http://127\.0\.0\.1:\d+/next')
        self.assertRegex(outline, r'textbox "Member name" \[ref=d\d+e\d+\]')
        self.assertIn('text "The card\'s own footer"', outline)
        self.assertIn('text "Inner fallback"', outline)
        self.assertRegex(outline, r'link "Inner link" \[ref=d\d+e\d+\]')
        for unseen in ('Untitled', 'No link given', 'Nothing here', 'No footer'):
            self.assertNotIn(unseen, outline)
        # In the order a person reads them.
        places = [outline.index(words) for words in
                  ('Card title', 'Read more', 'Member name', 'own footer', 'Inner fallback', 'Inner link')]
        self.assertEqual(places, sorted(places), outline)

    def test_a_field_and_a_link_inside_a_shadow_tree_are_operated_by_ref(self):
        self.h.navigate(self.url + '/shadow')
        outline = self.outline()
        member = self.ref(outline, 'textbox "Member name"')
        # A find reaches what a read prints, and answers the same ref.
        found = self.find(role='textbox', name='Member name')
        self.assertTrue(found['ok'], found)
        self.assertEqual(found['data']['element'], member)
        self.ok('fill', element=member, text='ada', actor='ada')
        after = self.outline()
        self.assertIn('text "member ada"', after)
        self.assertIn(f'textbox "Member name" [ref={member}] value="ada"', after)
        review = {field['name']: field for field in self.ok('fields')['data']['fields']}
        self.assertEqual((review['Member name']['value'], review['Member name']['set']['by']), ('ada', 'ada'))
        entry = self.ok('ledger', since=0)['data']['entries'][-1]
        self.assertEqual((entry['action'], entry['target'], entry['text']),
                         ('fill', {'role': 'textbox', 'name': 'Member name'}, 'ada'))
        # A link handed down through two slots is clicked where it is drawn.
        link = self.find(role='link', name='Read more')
        self.assertTrue(link['ok'], link)
        self.assertEqual(link['data']['element'], self.ref(outline, 'link "Read more"'))
        clicked = self.ok('click', element=link['data']['element'])
        self.assertEqual((clicked['navigation'], clicked['navigated']), ('started', True), clicked)
        self.assertTrue(clicked['url'].endswith('/next'), clicked)

    def test_a_button_whose_words_are_handed_to_its_slot_is_named_and_clicked(self):
        # A component library's button is a button in a shadow tree around a
        # slot, and its words are the component's own children. It is named
        # by the words it shows, and a click lands on it: over those words a
        # point is the component's, and the button is what is drawn there.
        self.h.navigate(self.url + '/shadow')
        outline = self.outline()
        save, send = self.ref(outline, 'button "Save draft"'), self.ref(outline, 'button "Send now"')
        self.assertEqual(self.find(role='button', name='Save draft')['data']['element'], save)
        self.ok('click', element=save)
        self.assertIn('text "pressed Save draft"', self.outline())
        self.ok('click', element=send)
        self.assertIn('text "pressed Send now"', self.outline())
        # A point over the words names the button a click there presses.
        box = json.loads(self.main_world('JSON.stringify(document.querySelector("slot-button").getBoundingClientRect())'))
        hit = self.ok('at', x=box['x'] + box['width'] / 2, y=box['y'] + box['height'] / 2)
        self.assertEqual((hit['data']['ref'], hit['data']['target']), (save, 'button "Save draft"'), hit)

    def test_a_secret_inside_a_shadow_tree_is_kept_as_one_outside_it_is(self):
        self.h.navigate(self.url + '/shadow')
        said = []

        def heard(answer):
            said.append(answer)
            return answer
        fields = 'document.querySelector("outer-card").shadowRoot'
        outline = heard(self.ok('outline', actor='ada'))['outline']
        self.assertRegex(outline, r'textbox "Passphrase" \[ref=d\d+e\d+\] type=password')
        field, pin = self.ref(outline, 'textbox "Passphrase"'), self.ref(outline, 'textbox "Pin"')
        refused = heard(self.page('fill', element=field, text='correct-horse', actor='ada'))
        self.assertEqual((refused['ok'], refused['code']), (False, 'secret_field'), refused)
        heard(self.ok('fill', element=field, text='correct-horse', secret=True, actor='ada'))
        keys = dict(ctrl=False, shift=False, alt=False, meta=False)
        for char in '-battery':
            pressed = heard(self.ok('press', element=field, key=char, actor='ben', **keys))
            self.assertEqual(pressed['message'], 'Pressed a key in a secret field', pressed)
        # A page that shows a password on request turns its field into plain
        # text. In a shadow tree too, what it holds is no less a secret, even
        # when nothing had been written into it before.
        self.main_world(f'({fields}.querySelector("[name=pin]").type = "text", 1)')
        refused = heard(self.page('fill', element=pin, text='pin-4711', actor='ada'))
        self.assertEqual((refused['ok'], refused['code']), (False, 'secret_field'), refused)
        heard(self.ok('fill', element=pin, text='pin-4711', secret=True, actor='ada'))
        # A secret a person types is set down as set, and no more.
        self.h.settle(1.6)
        mark = len(self.h.seen)
        self.main_world(f'({fields}.querySelector("[name=pin]").focus(), 1)')
        self.typed('-x9')
        typed = self.told('page_record', mark)['entry']
        self.assertEqual((typed['how'], typed['secret'], typed['target']),
                         ('input', True, {'role': 'textbox', 'name': 'Pin'}), typed)
        self.assertNotIn('text', typed)
        said.append(typed)
        held = json.loads(self.main_world(f'JSON.stringify([...{fields}.querySelectorAll("input")].map(i => i.value))'))
        self.assertEqual(held, ['', 'correct-horse-battery', 'pin-4711-x9'])
        again = heard(self.ok('outline', actor='rev'))['outline']
        self.assertIn(f'textbox "Passphrase" [ref={field}] type=password value=<21 characters hidden>', again)
        self.assertIn(f'textbox "Pin" [ref={pin}] value=<11 characters hidden>', again)
        # A search says the words it looked for, so it looks for parts of them.
        for words in ('orrect-hor', 'atter', '471', '-x'):
            self.assertEqual(heard(self.ok('search', text=words))['data']['total'], 0)
        review = {listed['name']: listed for listed in heard(self.ok('fields', actor='rev'))['data']['fields']}
        self.assertEqual((review['Passphrase']['value'], review['Pin']['value']),
                         ('<21 characters hidden>', '<11 characters hidden>'))
        entries = heard(self.ok('ledger', since=0, actor='cy'))['data']['entries']
        fills = [entry for entry in entries if entry.get('action') == 'fill' and entry['ok']]
        self.assertEqual([(entry['target']['name'], entry.get('text'), entry['secret']) for entry in fills],
                         [('Passphrase', None, True), ('Pin', None, True)])
        self.assertEqual({entry['message'] for entry in entries if entry.get('action') == 'press'},
                         {'Pressed a key in a secret field'})
        everything = json.dumps(said)
        for text in ('correct-horse', 'battery', 'pin-4711', '4711', 'x9'):
            self.assertNotIn(text, everything)

    def test_a_read_says_what_a_closed_shadow_tree_keeps_from_it(self):
        # A closed shadow tree is out of a page script's reach, so its words
        # are not read. The read says so in a line, and names the element: a
        # picture of it shows what the read could not.
        self.h.navigate(self.url + '/shadow')
        outline = self.outline()
        self.assertNotIn('Sealed words', outline)
        told = [line for line in outline.splitlines() if 'sealed-note' in line]
        self.assertEqual(len(told), 1, outline)
        self.assertIn('closed shadow tree', told[0])
        box = self.ok('capture', element=re.search(r'\[ref=(d\d+e\d+)\]', told[0]).group(1))
        self.assertEqual(box['data']['format'], 'png')
        self.assertGreater(box['data']['height'], 0)
        # The elements whose shadow trees are open are read, and said nothing of.
        self.assertEqual(outline.count('closed shadow tree'), 1, outline)

    def test_a_code_block_is_read_as_code_line_for_line(self):
        self.h.navigate(self.url + '/code')
        outline = self.outline()
        # A word of code in a sentence is a word of the sentence.
        self.assertIn('text "Match a control by its type or its pattern."', outline)
        # A block of code keeps its lines and its indentation.
        self.assertIn('- code ' + json.dumps(CODE_BLOCK), outline)
        # Lines a highlighter sets in boxes of their own are lines, and a
        # gutter the page hides is not read.
        self.assertIn('- code ' + json.dumps('first();\n  second();'), outline)
        # A link in code is read with it and still acted on by its ref.
        self.assertIn('- code "linked() + 1"', outline)
        self.assertRegex(outline, r'link "linked\(\)" \[ref=d\d+e\d+\]')
        # A long block is read whole, a part to a line, broken between two
        # of its own lines.
        self.assertEqual(re.findall(r'step\((\d+)\);', outline), [str(number) for number in range(1, 401)])
        parts = [line for line in outline.splitlines() if 'step(' in line]
        self.assertGreater(len(parts), 1, parts)
        for part in parts:
            self.assertTrue(part.startswith('- code "step('), part)
            self.assertTrue(part.endswith(');"'), part)
            self.assertLess(len(part), 2200, part)
        # A search finds code by what it says, and a read goes on from it.
        found = self.ok('search', text='input[pattern]')
        self.assertEqual(found['data']['total'], 1, found)
        around = self.ok('outline', **{'from': found['data']['matches'][0]['line']})
        self.assertTrue(around['outline'].splitlines()[0].startswith('- code "/* matches a password input */'), around)

    def test_the_spaces_between_a_page_s_elements_do_not_end_its_read(self):
        # A read looks at so many elements and no more, so that it ends
        # whatever the page holds. The text nodes between them are not
        # elements: a long article's mark-up leaves more of them than it has
        # elements, and its read goes to its end all the same.
        self.h.navigate(self.url + '/spaced')
        self.assertEqual(self.main_world('document.getElementById("gaps").childNodes.length'), 45000)
        read = self.ok('outline')
        self.assertEqual(read['outline'], '- heading "Spaced" level=1\n- text "The end of the page."', read)
        self.assertEqual(read['message'], 'Read 2 lines')
        self.assertEqual(self.ok('search', text='the end of')['data']['total'], 1)

    def test_a_read_goes_through_an_element_that_has_no_box_of_its_own(self):
        # A documentation site sets its article in a grid: the article's
        # element is `display: contents`, so it has no box and what it holds
        # is laid out in its place. It is on the page all the same.
        self.h.navigate(self.url + '/layout')
        outline = self.outline()
        self.assertIn('- main', outline)
        self.assertIn('heading "The input element" level=1', outline)
        self.assertIn('text "Used to create interactive controls."', outline)
        self.assertIn('heading "Attributes" level=2', outline)
        self.assertIn('text "The pattern attribute is a regular expression."', outline)
        self.assertIn('columnheader: "Description"', outline)
        self.assertIn('cell: "A single-line text field"', outline)
        self.assertIn('text "Words in a box of no box."', outline)
        self.assertIn('- code ' + json.dumps('input[pattern] {\n}'), outline)
        self.assertIn('text "About this site"', outline)
        # What is hidden stays out, whatever stands between it and what
        # hides it.
        for hidden in ('Folded', 'Never shown', 'nor this', 'Hidden by attribute', 'Hidden from readers',
                       'Invisible words', 'Skipped', 'skipped'):
            self.assertNotIn(hidden, outline)
        self.assertEqual(self.ok('search', text='the PATTERN attribute')['data']['total'], 1)
        self.assertEqual(self.ok('search', text='folded')['data']['total'], 0)
        link = self.find(role='link', name='Attributes')
        self.assertTrue(link['ok'], link)
        self.assertEqual(link['data']['element'], self.ref(outline, 'link "Attributes"'))
        # What a person opens is then read.
        self.ok('click', element=self.ref(outline, 'button "More on patterns"'))
        opened = self.outline()
        for words in ('Folded words', 'Folded paragraph', 'Folded run'):
            self.assertIn(words, opened)

    def test_a_find_reads_a_frame_s_own_document(self):
        self.h.navigate(self.url + '/embed')
        outline = self.outline()
        frame = re.search(r'iframe "Card form" \[frame=([\d.]+)\]', outline).group(1)
        # Neither the page nor the frame was read first: the id the page
        # printed and the description are all the caller needs.
        found = self.find(role='textbox', name='Security code', frame=frame)
        self.assertTrue(found['ok'], found)
        self.assertEqual(found['data']['element'], self.ref(self.ok('outline', frame=frame)['outline'],
                                                           'textbox "Security code"'))
        self.ok('fill', element=found['data']['element'], text='123', frame=frame)
        self.assertIn('value="123"', self.ok('outline', frame=frame)['outline'])
        # The ref says which frame it was found in: it needs no frame named.
        self.ok('fill', element=found['data']['element'], text='456')
        self.assertIn('value="456"', self.ok('outline', frame=frame)['outline'])
        # The page itself holds none of the frame's words: a find that names
        # no frame looks on in the frame the page shows, and names it.
        there = self.find(name='Security code')
        self.assertTrue(there['ok'], there)
        self.assertEqual((there['data']['element'], there['data']['frame']), (found['data']['element'], frame), there)
        self.assertIn(f'in frame {frame} ("Card form")', there['message'], there)
        # What no document of the page holds is not found, and the caller is
        # still told where a frame's own document is read.
        absent = self.find(name='Sort code')
        self.assertEqual((absent['ok'], absent['code']), (False, 'not_found'), absent)
        self.assertIn(f'frame={frame}', absent['message'], absent)
        self.assertIn('read it with read-page', absent['message'], absent)

    def test_a_find_and_a_search_look_on_in_the_frames_the_page_shows(self):
        self.h.navigate(self.url + '/embedder')
        outline = self.reads('textbox "Email"', 'the frame was never read with the page')['outline']
        frame = re.search(r'iframe "Newsletter sign-up" \[frame=(\d+)\]', outline).group(1)
        # Words the page's own document does not say are looked for in the
        # frame it shows. The line names its frame, and is read on from there.
        found = self.ok('search', text='promo CODE')
        hit = found['data']['matches'][0]
        self.assertEqual((found['data']['total'], found['data']['page_total'], hit['frame']), (1, 0, frame), found)
        self.assertEqual(found['outline'], f"frame {frame}, {hit['line']}: {hit['text']}")
        self.assertIn(f'none in the page\'s own document, 1 in frame {frame} ("Newsletter sign-up")', found['message'])
        self.assertNotIn('frames', found['data'])
        self.assertIn('Promo code', self.ok('outline', frame=frame, **{'from': hit['line']})['outline'].splitlines()[0])
        # Words the page's own document says are found there and no further.
        own = self.ok('search', text='letters')
        self.assertEqual((own['data']['total'], [match.get('frame') for match in own['data']['matches']]), (1, [None]), own)
        self.assertNotIn('page_total', own['data'])
        # Asked to, a search looks in the frames as well.
        both = self.ok('search', text='letters', frames=True)
        self.assertEqual((both['data']['total'], both['data']['page_total'], [match.get('frame') for match in both['data']['matches']]),
                         (2, 1, [None, frame]), both)
        self.assertTrue(both['outline'].splitlines()[1].startswith(f'frame {frame}, '), both)
        self.assertIn('Search the letters', both['data']['matches'][1]['text'])
        # A frame named is the one document searched.
        alone = self.ok('search', text='letters', frame=frame)
        self.assertEqual((alone['data']['total'], alone['data']['matches'][0].get('frame')), (1, None), alone)
        # A find that the page's own document cannot answer is answered by
        # the frame, whose element's ref acts there.
        email = self.find(name='Email')
        self.assertTrue(email['ok'], email)
        self.assertEqual((email['data']['element'], email['data']['frame'], email['data']['matches']),
                         (self.ref(outline, 'textbox "Email"'), frame, 1), email)
        self.ok('fill', element=email['data']['element'], text='ada@example.org')
        self.assertIn('value="ada@example.org"', self.ok('outline', frame=frame)['outline'])
        # One the page's own document answers is answered there.
        note = self.find(role='textbox')
        self.assertEqual((note['data']['name'], 'frame' in note['data']), ('Note', False), note)
        # Asked to, a find counts the page's matches and the frame's
        # together, the page's first: several are listed with their frames
        # and none is guessed.
        several = self.find(role='textbox', frames=True)
        self.assertEqual((several['ok'], several['code']), (False, 'ambiguous'), several)
        self.assertIn('4 elements are role "textbox" on the page and in its frames: (1) textbox "Note"', several['message'])
        self.assertIn(f'(2) frame {frame}: textbox "Email"', several['message'])
        third = self.find(role='textbox', nth=3, frames=True)
        self.assertEqual((third['data']['name'], third['data']['frame'], third['data']['nth'], third['data']['matches']),
                         ('Promo code', frame, 3, 4), third)
        self.assertIn(f'in frame {frame} ("Newsletter sign-up") of 4', third['message'])
        first = self.find(role='textbox', nth=1, frames=True)
        self.assertEqual((first['data']['name'], 'frame' in first['data']), ('Note', False), first)
        beyond = self.find(role='textbox', nth=5, frames=True)
        self.assertEqual((beyond['ok'], beyond['code']), (False, 'not_found'), beyond)
        self.assertIn('there is no (5)', beyond['message'])
        # The closest way any document matches decides: a name in another
        # letter case is found in the frame when nothing matches it better.
        self.assertEqual(self.find(name='promo code', frames=True)['data']['frame'], frame)
        # Looking is reading: nothing but the fill took a place in the record.
        self.assertEqual([entry['action'] for entry in self.ok('ledger', since=0)['data']['entries']], ['fill'])

    def test_gestures_work_on_the_elements_of_another_site_s_frame(self):
        self.h.navigate(self.url + '/gestures-holder')
        outline = self.reads('canvas "Signature pad"', 'the frame was never read with the page')['outline']
        frame = re.search(r'iframe "Gestures" \[frame=(\d+)\]', outline).group(1)
        inside = lambda: self.ok('outline', frame=frame)['outline']
        self.assertNotIn('Fishing licence', inside(), 'the menu starts closed')
        # A hover on an element of the frame opens what the frame's own
        # style opens, where the frame sits in the page.
        trigger = self.ref(outline, '(?:hint|hoverable) "Licences menu"')
        hovered = self.ok('hover', element=trigger, actor='ada')
        self.assertTrue(hovered['data']['changed'], hovered)
        self.assertIn('+ - list', hovered['outline'], hovered)
        self.ok('click', element=self.ref(inside(), 'link "Fishing licence"'), actor='ada')
        self.assertIn('text "opened fishing"', inside())
        # A drag between two elements of the frame moves what a hand would.
        first = self.ref(inside(), 'draggable "Riverton office"')
        third = self.ref(inside(), 'draggable "Central office"')
        dragged = self.ok('drag', element=first, to=third, steps=6, actor='ada')
        self.assertIn('Dragged', dragged['message'], dragged)
        self.ok('click', element=self.ref(inside(), 'button "Save order"'), actor='ada')
        self.assertIn('text "saved Porthaven office > Central office > Riverton office"', inside(), dragged)
        # A path is drawn on the frame's canvas, in the canvas's own box.
        pad = self.ref(inside(), 'canvas "Signature pad"')
        points = [[20, 110], [40, 60], [60, 100], [80, 50], [100, 105], [120, 70], [140, 95]]
        drawn = self.ok('path', element=pad, points=points, actor='ada')
        self.assertEqual(drawn['data']['drew'], len(points), drawn)
        self.ok('click', element=self.ref(inside(), 'button "Submit signature"'), actor='ada')
        self.assertIn('text "Signature received"', inside(), 'the frame\'s canvas took the line')
        # Each is set down with the frame it was done in.
        entries = self.ok('ledger', since=0)['data']['entries']
        self.assertEqual([(entry['action'], entry['ok'], entry.get('frame')) for entry in entries],
                         [('hover', True, frame), ('click', True, frame), ('drag', True, frame), ('click', True, frame),
                          ('path', True, frame), ('click', True, frame)])
        self.assertEqual({entry['frame_url'] for entry in entries}, {self.server.other + '/gestures'})
        # A drag does not leave its frame: an end outside it is refused.
        refused = self.page('drag', element=first, to_x=10, to_y=10, steps=2)
        self.assertEqual((refused['ok'], refused['code']), (False, 'invalid_request'), refused)
        # The page around the frame stayed where it was.
        self.assertIn('heading "Holder" level=1', self.outline())

    def test_a_point_over_a_frame_names_the_frame_s_own_element(self):
        self.h.navigate(self.url + '/nested')
        outline = self.reads('iframe "Outer frame"', 'the page never listed its frame')['outline']
        outer = re.search(r'iframe "Outer frame" \[frame=(\d+)\]', outline).group(1)
        within = self.reads('iframe "Payment form"', 'the outer frame never listed its own', frame=outer)['outline']
        inner = re.search(r'iframe "Payment form" \[frame=(\d+)\]', within).group(1)
        save = self.ref(self.reads('button "Save bank details"', 'the inner frame was never read', frame=inner)['outline'],
                        'button "Save bank details"')
        # Where the inner frame's button is in the page: each frame's place
        # in the document that holds it, and the button's in its own.
        x, y = self.main_world(
            '((outer, inner, box) => [outer.x + inner.x + box.x + box.width / 2, outer.y + inner.y + box.y + box.height / 2])('
            'document.querySelector("iframe").getBoundingClientRect(), '
            'document.querySelector("iframe").contentDocument.querySelector("iframe").getBoundingClientRect(), '
            'document.querySelector("iframe").contentDocument.querySelector("iframe").contentDocument'
            '.querySelector("button").getBoundingClientRect())')
        hit = self.ok('at', x=x, y=y)
        # The element is the frame's own, named with its frame, by a ref
        # that acts there, at the point the caller gave.
        self.assertEqual((hit['data']['ref'], hit['data']['frame'], hit['data']['interactive']), (save, inner, True), hit)
        self.assertEqual((hit['data']['x'], hit['data']['y']), (x, y), hit)
        self.assertIn(f'button "Save bank details" [ref={save}] is at', hit['message'], hit)
        self.assertIn(f'in frame {inner}', hit['message'], hit)
        self.assertEqual([key for key in hit['data'] if key in ('frame_element', 'inside')], [])
        # A point of the page's own document is the page's, as it was.
        own = self.ok('at', x=20, y=20)
        self.assertNotIn('frame', own['data'], own)
        self.assertNotIn('in frame', own['message'], own)

    def test_files_are_chosen_for_a_file_input_in_another_site_s_frame(self):
        self.h.navigate(self.url + '/upload-holder')
        outline = self.reads('file "Passport scan"', 'the frame was never read with the page')['outline']
        frame = re.search(r'iframe "Upload" \[frame=(\d+)\]', outline).group(1)
        scan = self.ref(outline, 'file "Passport scan"')
        with tempfile.TemporaryDirectory() as folder:
            passport, photo, second = (os.path.join(folder, name) for name in ('passport.pdf', 'photo.png', 'second.pdf'))
            for path, body in ((passport, b'%PDF-1.4'), (photo, b'\x89PNG one'), (second, b'%PDF-1.5!')):
                with open(path, 'wb') as file:
                    file.write(body)
            # The frame's own element says what it accepts.
            refused = self.page('fill_files', element=scan, paths=[photo])
            self.assertEqual((refused['ok'], refused['code']), (False, 'not_accepted'), refused)
            chosen = self.ok('fill_files', element=scan, paths=[passport], actor='ada')
            self.assertEqual(chosen['data']['files'], ['passport.pdf'], chosen)
            # The frame's page heard the picker's own events, and holds the file.
            held = self.reads('text "files passport.pdf:8"', 'the frame\'s page never heard of the file', frame=frame)
            self.assertRegex(held['outline'], r'file "Passport scan" .*files="passport.pdf"')
            # A click on it holds the page's chooser as in the page itself,
            # and the files then chosen reach the same field.
            clicked = self.ok('click', element=scan, actor='ada')
            self.assertIn('file chooser', clicked['message'], clicked)
            again = self.ok('fill_files', element=scan, paths=[second], actor='ada')
            self.assertEqual(again['data']['files'], ['second.pdf'], again)
            self.reads('text "files second.pdf:9"', 'the frame\'s page never heard of the second file', frame=frame)
        entries = self.ok('ledger', since=0)['data']['entries']
        self.assertEqual([(entry['action'], entry['ok'], entry.get('frame')) for entry in entries],
                         [('fill_files', False, frame), ('fill_files', True, frame), ('click', True, frame), ('fill_files', True, frame)])


if __name__ == '__main__':
    unittest.main(verbosity=2)
