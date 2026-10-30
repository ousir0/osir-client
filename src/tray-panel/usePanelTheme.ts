import { useEffect, useState } from "react";
import { UPSTREAM_THEME_STORAGE_KEY } from "@/lib/productBranding";

type ThemeChoice = "light" | "dark" | "system";

function readChoice(storageKey: string): ThemeChoice {
  try {
    const stored = window.localStorage.getItem(storageKey);
    if (stored === "light" || stored === "dark") return stored;
  } catch {
    // 读不到按跟随系统
  }
  return "system";
}

function systemDark(): boolean {
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false;
}

export interface PanelTheme {
  dark: boolean;
  /** 用户选定的深浅色；跟随系统时为 null（窗口外观交回系统） */
  explicit: boolean | null;
}

function resolve(storageKey: string): PanelTheme {
  const choice = readChoice(storageKey);
  if (choice === "system") return { dark: systemDark(), explicit: null };
  return { dark: choice === "dark", explicit: choice === "dark" };
}

/**
 * 面板是常驻的独立 WebView：不用主界面的 ThemeProvider（它会改主窗口的原生主题），
 * 只读主界面存在 localStorage 的选择，另一个窗口改了或系统深浅色变了都跟着变。
 * `refreshKey` 变化（每次弹出）时也重新读一次。
 */
export function usePanelTheme(
  refreshKey: unknown,
  storageKey = UPSTREAM_THEME_STORAGE_KEY,
): PanelTheme {
  const [theme, setTheme] = useState<PanelTheme>(() => resolve(storageKey));

  useEffect(() => {
    setTheme(resolve(storageKey));
  }, [refreshKey, storageKey]);

  useEffect(() => {
    const update = () => setTheme(resolve(storageKey));
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    media?.addEventListener("change", update);
    const onStorage = (event: StorageEvent) => {
      if (event.key === storageKey) update();
    };
    window.addEventListener("storage", onStorage);
    return () => {
      media?.removeEventListener("change", update);
      window.removeEventListener("storage", onStorage);
    };
  }, [storageKey]);

  useEffect(() => {
    document.documentElement.classList.toggle("dark", theme.dark);
    document.documentElement.classList.toggle("light", !theme.dark);
  }, [theme.dark]);

  return theme;
}
