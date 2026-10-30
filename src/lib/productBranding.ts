/** Product-owned UI storage; protocol identifiers and upstream formats stay unchanged. */
export const OSIR_THEME_STORAGE_KEY = "osir_client-theme";
export const UPSTREAM_THEME_STORAGE_KEY = "cc-switch-theme";

export function productThemeStorageKey(osir: boolean): string {
  if (!osir) return UPSTREAM_THEME_STORAGE_KEY;
  try {
    if (localStorage.getItem(OSIR_THEME_STORAGE_KEY) === null) {
      const previous = localStorage.getItem(UPSTREAM_THEME_STORAGE_KEY);
      if (["light", "dark", "system"].includes(previous ?? "")) {
        localStorage.setItem(OSIR_THEME_STORAGE_KEY, previous!);
      }
    }
  } catch {
    // A blocked preference cache must not prevent opening the application.
  }
  return OSIR_THEME_STORAGE_KEY;
}

export function productConfigFallback(osir: boolean): string {
  return osir ? "~/.osir_client/config.json" : "~/.cc-switch/config.json";
}
