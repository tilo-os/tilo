// Test-only inert forms inserted into an authenticated document, without
// replacing its document, changing its origin, or submitting any values.
(() => {
  if (location.protocol !== 'https:') throw new Error('Fixture needs HTTPS');
  const prior = document.getElementById('qos-credential-fixture');
  if (prior) prior.remove();
  const root = document.createElement('section');
  root.id = 'qos-credential-fixture';
  root.style.cssText = 'position:relative;z-index:2147483647;background:white;color:black;padding:24px;font:16px system-ui;display:block';
  const heading = document.createElement('h1');
  heading.textContent = 'Synthetic credential and autofill fixture';
  root.append(heading);
  function form(id, fields) {
    const node = document.createElement('form');
    node.id = id;
    node.action = location.origin + '/';
    node.method = 'post';
    node.addEventListener('submit', event => {
      event.preventDefault();
      node.dataset.submitAttempted = 'true';
    });
    for (const [name, type, autocomplete] of fields) {
      const label = document.createElement('label');
      label.textContent = name + ' ';
      label.style.display = 'block';
      const input = document.createElement('input');
      Object.assign(input, {name, type, autocomplete});
      input.id = id + '-' + name;
      label.append(input);
      node.append(label);
    }
    root.append(node);
    return node;
  }
  form('qos-login', [['username','text','username'],['password','password','current-password']]);
  form('qos-address', [['given','text','given-name'],['family','text','family-name'],['street','text','address-line1'],['city','text','address-level2'],['region','text','address-level1'],['postal','text','postal-code'],['country','text','country-name']]);
  const hidden = form('qos-hidden', [['password','password','current-password']]);
  hidden.hidden = true;
  document.body.prepend(root);
  return {installed:true, origin:location.origin, forms:3};
})();
