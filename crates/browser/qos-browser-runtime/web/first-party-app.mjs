// Persisted first-party ids keep the spelling they were written under. The
// Rust runtime resolves them through `collab_types::first_party_app_ids`; the
// JS host only needs the same-family check for records it looks up by id.
// Every renamed family shares its suffix across `dev.qos.*`, `dev.tilo.*` and
// the current `so.tilo.*` id.
const CURRENT = 'so.tilo.';
const RETIRED = ['dev.tilo.', 'dev.qos.'];

/** Whether a persisted app id names the current first-party package `current`. */
export function firstPartyApp(id, current) {
  if (id === current) return true;
  if (typeof id !== 'string' || !current.startsWith(CURRENT)) return false;
  const suffix = current.slice(CURRENT.length);
  return RETIRED.some(prefix => id === prefix + suffix);
}
