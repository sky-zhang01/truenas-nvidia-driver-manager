// Unknown stored themes and unavailable storage fall back to focused-light. Theme changes preserve the
// current DOM, inputs and pollers.

export type Theme = "plate-light" | "plate-dark" | "focused-light" | "focused-dark";

export const THEMES: readonly Theme[] = [
  "focused-light",
  "focused-dark",
  "plate-light",
  "plate-dark",
];

export const DEFAULT_THEME: Theme = "focused-light";

const STORAGE_KEY = "nvm.theme";

export const THEME_STORAGE_KEY: string = STORAGE_KEY;

export function isTheme(v: unknown): v is Theme {
  return v === "plate-light" || v === "plate-dark" || v === "focused-light" || v === "focused-dark";
}

export function detectTheme(stored: string | null): Theme {
  return isTheme(stored) ? stored : DEFAULT_THEME;
}

function readStored(): string | null {
  try {
    return window.localStorage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
}

let theme: Theme | null = null;

export function currentTheme(): Theme {
  if (theme === null) {
    theme = detectTheme(readStored());
  }
  return theme;
}

export function applyTheme(next: Theme): void {
  theme = next;
  // Node-side imports have no document; keep pure state usable without a browser.
  if (typeof document !== "undefined") {
    document.documentElement.dataset["theme"] = next;
  }
}

// A failed storage write keeps the choice for this page load without blocking the switch.
export function setTheme(next: Theme): void {
  applyTheme(next);
  try {
    window.localStorage.setItem(STORAGE_KEY, next);
  } catch {

  }
}

export function themeMessageKey(t: Theme): string {
  return `theme.option.${t}`;
}
