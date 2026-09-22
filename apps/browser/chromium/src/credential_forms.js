// Evaluated once by the native renderer. The returned closure is retained in
// C++, never assigned to the page global. Secret values are function arguments,
// never interpolated into evaluated source or retained in this handle map.
(() => {
  'use strict';
  const handles = new Map();
  let sequence = 0;
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
  const getter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').get;
  const addressTokens = Object.freeze({
    name: 'full_name', 'given-name': 'given_name', 'family-name': 'family_name',
    'street-address': 'street', 'address-line1': 'street', 'address-level2': 'city',
    'address-level1': 'region', 'postal-code': 'postal_code', country: 'country',
    'country-name': 'country'
  });
  const textTypes = new Set(['text', 'email', 'tel', 'search', 'url']);
  const token = field => String(field.autocomplete || '').trim().split(/\s+/).at(-1);
  function visible(field) {
    if (!(field instanceof HTMLInputElement) || field.ownerDocument !== document ||
        !field.isConnected || field.getRootNode() !== document || field.disabled || field.readOnly ||
        !field.getClientRects().length) return false;
    const style = getComputedStyle(field);
    return style.display !== 'none' && style.visibility !== 'hidden' &&
           style.visibility !== 'collapse' && Number(style.opacity) !== 0;
  }
  function safeForm(form) {
    if (window !== top || location.protocol !== 'https:' || !form.isConnected ||
        form.ownerDocument !== document) return false;
    try {
      const action = new URL(form.action || location.href, location.href);
      return action.protocol === 'https:' && action.origin === location.origin &&
             !action.username && !action.password;
    } catch { return false; }
  }
  function fieldValid(field, form, password) {
    return visible(field) && field.form === form &&
           (password ? field.type === 'password' : textTypes.has(field.type));
  }
  function valid(entry) {
    return entry && safeForm(entry.form) && fieldValid(entry.password, entry.form, true) &&
           (!entry.username || fieldValid(entry.username, entry.form, false));
  }
  function add(entry) {
    if (handles.size >= 32 || sequence >= Number.MAX_SAFE_INTEGER) return null;
    const id = String(++sequence); handles.set(id, entry); return id;
  }
  function discover() {
    handles.clear(); const result = [];
    for (const form of Array.from(document.forms).slice(0, 32)) {
      if (!safeForm(form)) continue;
      const fields = Array.from(form.elements).slice(0, 256);
      const usernames = fields.filter(f => fieldValid(f, form, false) && token(f) === 'username');
      const fallback = fields.filter(f => fieldValid(f, form, false) && ['text', 'email'].includes(f.type));
      // Ambiguous usernames require a supported form instead of guessing.
      const candidates = usernames.length ? usernames : fallback;
      if (candidates.length > 1) continue;
      const username = candidates[0] || null;
      for (const password of fields.filter(f => fieldValid(f, form, true)).slice(0, 8)) {
        const id = add({form, username, password}); if (!id) break;
        result.push({id, username: username ? getter.call(username).slice(0, 1024) : '',
                     kind: token(password) === 'new-password' ? 'new-password' : 'current-password'});
      }
    }
    return result;
  }
  function validate(id) { return !!valid(handles.get(id)); }
  function capture(id) {
    const entry = handles.get(id); if (!valid(entry)) return null;
    const password = getter.call(entry.password);
    const username = entry.username ? getter.call(entry.username) : '';
    if (!password || password.length > 4096 || username.length > 1024) return null;
    return {username, password};
  }
  function write(field, value) {
    setter.call(field, value);
    field.dispatchEvent(new Event('input', {bubbles: true}));
    field.dispatchEvent(new Event('change', {bubbles: true}));
  }
  function fill(id, username, password) {
    const entry = handles.get(id);
    if (typeof username !== 'string' || username.length > 1024 || typeof password !== 'string' ||
        !password || password.length > 4096 || !valid(entry)) return false;
    if (entry.username) write(entry.username, username);
    // Input handlers may replace nodes or change the form action synchronously.
    if (!valid(entry)) return false;
    write(entry.password, password); return true;
  }
  function discoverAddresses() {
    handles.clear(); const result = [];
    for (const form of Array.from(document.forms).slice(0, 32)) {
      if (!safeForm(form)) continue;
      const fields = Array.from(form.elements).slice(0, 256)
        .filter(f => fieldValid(f, form, false) && Object.hasOwn(addressTokens, token(f)))
        .map(field => ({field, token: token(field)}));
      if (!fields.length) continue;
      const id = add({form, fields}); if (id) result.push({id, fields: fields.map(f => f.token)});
    }
    return result;
  }
  function fillAddress(id, profile) {
    const entry = handles.get(id);
    const allowed = new Set(Object.values(addressTokens));
    if (!entry?.fields || !profile || typeof profile !== 'object' ||
        Object.keys(profile).some(key => !allowed.has(key) || typeof profile[key] !== 'string' || profile[key].length > 1024)) return false;
    const current = () => safeForm(entry.form) && entry.fields.every(({field, token: original}) =>
      fieldValid(field, entry.form, false) && token(field) === original);
    if (!current()) return false;
    for (const {field, token: original} of entry.fields) {
      if (!current()) return false;
      const value = profile[addressTokens[original]];
      if (value !== undefined) write(field, value);
    }
    return true;
  }
  return Object.freeze({discover, validate, capture, fill, discoverAddresses, fillAddress});
})()
