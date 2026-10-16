/**
 * The settings a browser profile starts with, read from the browser's own
 * locale: a person whose browser shows `12:31 AM` sees the desktop clock the
 * same way rather than `00:31`, which reads like a timer. They seed only a
 * profile with no stored settings; Settings changes them like any other.
 */

/** `locale.time_format` for this browser: `H12` where it uses a 12-hour clock. */
export function browserTimeFormat(intl = globalThis.Intl) {
  try {
    const options = new intl.DateTimeFormat(undefined, {hour: 'numeric'}).resolvedOptions();
    const twelve = options.hourCycle ? ['h11', 'h12'].includes(options.hourCycle) : options.hour12 === true;
    return twelve ? 'H12' : 'H24';
  } catch {
    return null;
  }
}

/** The settings document a new profile starts with in this browser. */
export function browserSettings(intl = globalThis.Intl) {
  const timeFormat = browserTimeFormat(intl);
  return timeFormat ? {locale: {time_format: timeFormat}} : {};
}
