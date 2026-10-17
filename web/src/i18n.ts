// A valid stored language wins; otherwise only a first browser language starting with zh selects Chinese.

import type { Lang } from "./messages.js";

export type { Lang };

const STORAGE_KEY = "nvm.lang";

export const LANG_STORAGE_KEY: string = STORAGE_KEY;

export function isLang(v: unknown): v is Lang {
  return v === "zh" || v === "en";
}

export function detectLang(stored: string | null, languages: readonly string[]): Lang {
  if (isLang(stored)) return stored;
  const first = languages.length > 0 ? languages[0] : "";
  return first.toLowerCase().startsWith("zh") ? "zh" : "en";
}

function readStored(): string | null {
  try {
    return window.localStorage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
}

let lang: Lang | null = null;

export function currentLang(): Lang {
  if (lang === null) {
    const nav = typeof navigator === "undefined" ? { languages: [] } : navigator;
    lang = detectLang(readStored(), nav.languages ?? []);
  }
  return lang;
}

export function setLang(next: Lang): void {
  lang = next;
  try {
    window.localStorage.setItem(STORAGE_KEY, next);
  } catch {

  }
}

export function htmlLang(l: Lang): string {
  return l === "zh" ? "zh-CN" : "en";
}
