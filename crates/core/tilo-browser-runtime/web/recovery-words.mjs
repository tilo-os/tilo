/**
 * The person's 24 recovery words: shown once, right after this browser's
 * person is first let in, drawn over the entry in the entry's own card, until
 * the person confirms they kept them.
 *
 * The words are the root's seed in the identity store's one recovery format
 * (`identity.mjs`'s `recoveryWords`): the only way back into Tilo if the
 * person loses every device, and never sent anywhere — they are derived in
 * this browser and shown here.
 */
import {localeMessagesSoon, localized} from './arrival.mjs';

/** How many words a recovery key has; as many as `tilo_identity::store`
 * (`RECOVERY_WORD_COUNT`). */
export const RECOVERY_WORD_COUNT = 24;

/** The card's words, in English, and what a locale file may replace. */
export const RECOVERY_WORDS = Object.freeze({
  heading: 'Your 24 recovery words',
  body: 'These 24 words are the only way back into Tilo if you lose every device. Write them down in order and keep them safe. Anyone who has them can be you.',
  copy: 'Copy',
  copied: 'Copied',
  confirm: 'I’ve written them down',
});

/** The card's words in `messages`' language (`arrival.mjs`'s locale files),
 * the English where the locale has none. */
export function recoveryCardCopy(messages = null) {
  const say = (key, english) => localized(messages, `recovery-words-${key}`, english);
  return Object.fromEntries(Object.entries(RECOVERY_WORDS).map(([key, english]) => [key, say(key, english)]));
}

/** `words` as the 24 recovery words, in order; a phrase of any other length
 * is a fault of the page, not the person. */
function wordsInOrder(words) {
  const list = String(words ?? '').trim().split(/\s+/).filter(Boolean);
  if (list.length !== RECOVERY_WORD_COUNT) {
    throw new RangeError(`a recovery key is ${RECOVERY_WORD_COUNT} words, not ${list.length}`);
  }
  return list;
}

/**
 * Show `words` (the person's 24, in order) over the page and resolve once the
 * person confirms they wrote them down. The card carries each word with its
 * position, a way to copy them, and the button that says they kept them.
 *
 * `document` and `clipboard` are the page's; `messages` its locale's, read
 * here when none is given.
 */
export async function showRecoveryWords(words, {document = globalThis.document, clipboard = globalThis.navigator?.clipboard, messages = null} = {}) {
  const list = wordsInOrder(words);
  const copy = recoveryCardCopy(messages ?? await localeMessagesSoon());

  return new Promise(resolve => {
    const form = document.createElement('form');
    form.className = 'tilo-entry-card tilo-recovery';
    form.dataset.card = 'recovery-words';
    form.setAttribute('role', 'dialog');
    form.setAttribute('aria-labelledby', 'tilo-recovery-heading');

    const heading = document.createElement('h2');
    heading.id = 'tilo-recovery-heading';
    heading.textContent = copy.heading;

    const body = document.createElement('p');
    body.className = 'tilo-recovery-lead';
    body.textContent = copy.body;

    // Every word with the position it is written in, so a copy that skipped
    // one is noticed before it is needed.
    const shown = document.createElement('ol');
    shown.className = 'tilo-recovery-words';
    list.forEach((word, at) => {
      const item = document.createElement('li');
      item.className = 'tilo-recovery-word';
      item.dataset.position = String(at + 1);
      const number = document.createElement('span');
      number.className = 'tilo-recovery-number';
      number.textContent = String(at + 1);
      const text = document.createElement('span');
      text.className = 'tilo-recovery-text';
      text.textContent = word;
      item.append(number, text);
      shown.append(item);
    });

    const actions = document.createElement('div');
    actions.className = 'tilo-entry-actions';
    const go = document.createElement('button');
    go.type = 'submit';
    go.className = 'tilo-entry-primary';
    go.textContent = copy.confirm;
    const hand = document.createElement('button');
    hand.type = 'button';
    hand.className = 'tilo-recovery-copy';
    hand.textContent = copy.copy;
    hand.addEventListener('click', () => {
      Promise.resolve(clipboard?.writeText?.(list.join(' '))).then(() => { hand.textContent = copy.copied; }, () => {});
    });
    // Row-reversed like every entry card: the action that goes on is rightmost.
    actions.append(go, hand);

    form.addEventListener('submit', event => {
      event.preventDefault();
      form.remove();
      resolve();
    });

    form.append(heading, body, shown, actions);
    document.body.append(form);
    go.focus?.();
  });
}
