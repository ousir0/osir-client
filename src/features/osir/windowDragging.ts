import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isLinux } from "@/lib/platform";

// Tauri's built-in region handler only checks the event target itself. Delegate
// within our region so brand text and images behave like its empty space.
export function useOsirWindowDragging() {
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window) || isLinux()) return;
    const drag = (event: MouseEvent) => {
      const target = event.target;
      if (
        event.button !== 0 ||
        event.detail > 1 ||
        !(target instanceof Element) ||
        !target.closest("[data-osir-drag-region]") ||
        target.closest(
          'button, a, input, select, textarea, [role="button"], [contenteditable="true"], [data-tauri-no-drag]',
        )
      )
        return;
      event.preventDefault();
      // Handle the native gesture once, before bubbling handlers can consume it.
      event.stopImmediatePropagation();
      void getCurrentWindow()
        .startDragging()
        .catch(() => {});
    };
    document.addEventListener("mousedown", drag, true);
    return () => document.removeEventListener("mousedown", drag, true);
  }, []);
}
