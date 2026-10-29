/** Move presentation preferences once; never rename native tool/history paths. */
export function readSessionPreference(suffix: string): string | null {
  const key = `osir_client.${suffix}`;
  try {
    const value = window.localStorage.getItem(key);
    const legacyKey = `cc-switch.${suffix}`;
    const legacy = window.localStorage.getItem(legacyKey);
    if (value !== null) {
      if (legacy !== null) window.localStorage.removeItem(legacyKey);
      return value;
    }
    if (legacy !== null) {
      window.localStorage.setItem(key, legacy);
      window.localStorage.removeItem(legacyKey);
    }
    return legacy;
  } catch {
    return null;
  }
}

export function writeSessionPreference(suffix: string, value: string): void {
  try {
    window.localStorage.setItem(`osir_client.${suffix}`, value);
  } catch {
    // A blocked storage area must not prevent reading local conversations.
  }
}
