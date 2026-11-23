/**
 * A person as a page shows them: their name where anyone named them, and
 * otherwise one plain description and the person glyph on a neutral disc. A
 * key is never their name, and the words said in place of a name are not one
 * either, so two people nobody named are not told apart by a number.
 */

/** What a person nobody named is called, wherever a page names them: the same words the
 * catalogue's `person-unnamed` says, which the desktop paints them with. */
export const UNNAMED = 'Someone without a name yet';

/** The person glyph a face nobody's name is on wears (`Icon::User`). */
export const PERSON_GLYPH = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" '
  + 'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">'
  + '<path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/></svg>';

/** `name` where it is a name to show: trimmed, and neither a key's hex nor the words said in
 * place of a name. Empty where nobody named the person, or `name` is none of theirs. */
export function shownName(name) {
  const trimmed = typeof name === 'string' ? name.trim() : '';
  if (!trimmed || trimmed === UNNAMED) return '';
  return trimmed.length >= 32 && /^[0-9a-f]+$/i.test(trimmed) ? '' : trimmed;
}

/** Whether `name` is no name of a person's ([`shownName`]). */
export function isUnnamed(name) {
  return shownName(name) === '';
}

/** What a person goes by on a page, their name or the description of a person nobody named. */
export function personLabel(name) {
  return shownName(name) || UNNAMED;
}

/** A name's first letter, as a face shows it: '' for a person nobody named, whose face is the
 * person glyph. */
export function initial(name) {
  return ([...shownName(name)][0] ?? '').toUpperCase();
}
