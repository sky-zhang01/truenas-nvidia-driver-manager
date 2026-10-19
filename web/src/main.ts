import { ApiError, clearToken, getHome, getLogin, postLogout, type HomeGpuDevice, type HomeResponse, type LoginInfo, type SetupProblem } from "./api.js";
import { brandLockup } from "./brand.js";
import { HomeRead } from "./home-read.js";
import { el, clear, expandText, renderNotice, apiErrorNotice, type Params } from "./dom.js";
import { currentLang, htmlLang, setLang } from "./i18n.js";
import {
  applyTheme,
  currentTheme,
  isTheme,
  setTheme,
  themeMessageKey,
  THEMES,
  type Theme,
} from "./theme.js";
import type { Lang } from "./messages.js";
import { renderLogin, startupItemText, startupProblemItems, settingProblemNoticeKey } from "./views/login.js";
import { renderPassword } from "./views/password.js";
import { renderHome } from "./views/home.js";
import { renderUpdate } from "./views/update.js";
import { renderDrivers } from "./views/drivers.js";
import { renderTask, renderTasks } from "./views/tasks.js";
import { closeAllDialogs, setPageTitle } from "./dialog.js";

export interface App {
  readonly route: string;
  readonly lang: Lang;
  readonly login: LoginInfo;
  refreshLogin(owner?: () => unknown): Promise<LoginInfo>;
  // Only a successful key-list read updates the SSH sign-in flag; it never changes session ownership.
  syncSshEnabled(present: boolean): void;
  render(): void;
  go(route: string): void;

  sessionEnded(): void;
  readonly logoutDone: boolean;
  clearLogoutDone(): void;
  readonly signedIn: boolean;

  readonly sessionExpired: boolean;
  clearSessionExpired(): void;

  readonly passwordChanged: boolean;
  setPasswordChanged(): void;
  clearPasswordChanged(): void;

  readonly logoutError: { key: string; params: Params } | null;
  setLogoutError(key: string, params: Params): void;
  clearLogoutError(): void;

  readonly loginError: unknown;
  setLoginError(err: unknown): void;
  clearLoginError(): void;

  addCleanup(fn: () => void): void;
  // Global reports describe the active host task; task reports may clear only their own task claim.
  // Reporting patches the dot without redrawing.
  reportTaskState(active: TaskStateKind, scope: TaskStateScope, taskId?: string): void;

  readonly navTaskActive: TaskStateKind;

  readonly homeSnapshot: HomeResponse | null;
  // A session epoch prevents a late home response from affecting a subsequent login.
  readonly homeEpoch: number;

  ensureHome(): void;
  // An explicit home refresh joins any request already in flight.
  loadHome(): Promise<HomeResponse>;
}

const DEFAULT_ROUTE = "#/home";

export class Shell implements App {
  route = DEFAULT_ROUTE;
  lang: Lang = currentLang();
  login: LoginInfo = {
    logged_in: false,
    setup: { problems: [] },
    credentials: "ok",
    show_reset_help: false,
  };
  logoutDone = false;
  signedIn = false;
  session_expired = false;
  password_changed = false;
  logout_error: { key: string; params: Params } | null = null;
  login_error: unknown = null;
  nav_task_active: TaskStateKind = null;

  nav_task_id: string | null = null;
  private readonly homeReads = new HomeRead<HomeResponse>();
  private cleanups: (() => void)[] = [];

  get homeSnapshot(): HomeResponse | null {
    return this.homeReads.snapshot;
  }

  get homeEpoch(): number {
    return this.homeReads.currentEpoch;
  }

  private startHome(): Promise<HomeResponse> {
    return this.homeReads.start(
      () => this.signedIn,
      () => getHome(),
      (home) => paintHeaderGpu(home, this.lang),
    );
  }

  ensureHome(): void {
    if (!this.signedIn || this.homeSnapshot || this.homeReads.inFlight()) return;
    const epoch = this.homeEpoch;
    void this.startHome().catch(() => {
      if (epoch !== this.homeEpoch || !this.signedIn) return;
      const slot = document.getElementById("header-gpu");
      if (!slot || slot.childElementCount > 0) return;
      slot.append(el("p", { class: "header-gpu-line", text: "UNKNOWN" }));
    });
  }

  loadHome(): Promise<HomeResponse> {
    return this.startHome();
  }

  get navTaskActive(): TaskStateKind {
    return this.nav_task_active;
  }

  reportTaskState(active: TaskStateKind, scope: TaskStateScope, taskId?: string): void {
    if (scope === "global") {

      this.nav_task_active = active;
      this.nav_task_id = active === "running" ? (taskId ?? null) : null;
    } else if (active === "running") {

      this.nav_task_active = "running";
      this.nav_task_id = taskId ?? null;
    } else if (taskId !== undefined && this.nav_task_id !== null && this.nav_task_id === taskId) {
      // Uncertainty about another task cannot erase the currently confirmed running task.
      this.nav_task_active = null;
      this.nav_task_id = null;
    }
    applyNavTaskState(this.nav_task_active);
  }

  get loginError(): unknown {
    return this.login_error;
  }

  setLoginError(err: unknown): void {
    this.login_error = err;
  }

  clearLoginError(): void {
    this.login_error = null;
  }

  get logoutError(): { key: string; params: Params } | null {
    return this.logout_error;
  }

  setLogoutError(key: string, params: Params): void {
    this.logout_error = { key, params };
  }

  clearLogoutError(): void {
    this.logout_error = null;
  }

  get sessionExpired(): boolean {
    return this.session_expired;
  }

  clearSessionExpired(): void {
    this.session_expired = false;
  }

  get passwordChanged(): boolean {
    return this.password_changed;
  }

  setPasswordChanged(): void {
    this.password_changed = true;
  }

  clearPasswordChanged(): void {
    this.password_changed = false;
  }

  addCleanup(fn: () => void): void {
    this.cleanups.push(fn);
  }

  private runCleanups(): void {
    const cleanups = this.cleanups;
    this.cleanups = [];
    for (const fn of cleanups) fn();
  }

  clearLogoutDone(): void {
    this.logoutDone = false;
  }

  sessionEnded(): void {
    // Keep the signed-out result when a late 401 arrives. Remove open modals so the sign-in page is not left inert.
    closeAllDialogs();
    this.session_expired = true;
    this.signedIn = false;
    this.nav_task_active = null;
    this.nav_task_id = null;
    this.homeReads.invalidate();
    // A 401 ends the session immediately; the login-status follow-up only refreshes notices.
    this.render();
    void this.refreshLogin()
      .then(() => {
        // The 401 is authoritative even if login status still echoes the stale cookie.
        this.signedIn = false;
        this.render();
      })
      .catch((err: unknown) => {

        this.setLoginError(err);
        this.render();
      });
  }

  signOutLocally(): void {
    clearToken();
    closeAllDialogs();
    this.signedIn = false;
    this.nav_task_active = null;
    this.nav_task_id = null;
    this.homeReads.invalidate();
  }

  syncSshEnabled(present: boolean): void {
    this.login = { ...this.login, ssh_enabled: present };
  }

  // A response may affect global state only while its calling page remains live. Calls without a page
  // owner are application-scoped.
  refreshLogin(owner?: () => unknown): Promise<LoginInfo> {
    return getLogin()
      .then((info) => {
        if (owner && !owner()) return info;
        this.login = info;
        this.signedIn = info.logged_in;
        this.clearLoginError();
        return info;
      })
      .catch((err: unknown) => {
        if (owner && !owner()) throw err;
        this.setLoginError(err);
        throw err;
      });
  }

  go(route: string): void {
    if (window.location.hash === route) {
      this.route = route;
      this.render();
    } else {
      window.location.hash = route;
    }
  }

  render(): void {

    this.runCleanups();
    renderShell(this);
  }
}

// Apply the stored theme before the first render to prevent a flash of the default theme.
applyTheme(currentTheme());

const shell = new Shell();

export type TaskStateKind = "running" | "interrupted" | null;

export type TaskStateScope = "global" | "task";

function taskStateText(kind: Exclude<TaskStateKind, null>, lang: Lang): string {
  return expandText(kind === "running" ? "nav.tasks.active" : "nav.tasks.interrupted", {}, lang);
}
const root = (): HTMLElement => {
  const node = document.getElementById("app");
  if (!node) throw new Error("missing #app");
  return node;
};

let headerTitle: HTMLElement | null = null;

function topNotices(app: App, loginPage: boolean): HTMLElement {
  const lang = app.lang;
  const box = settingsNotices(app);
  const startup: SetupProblem[] = [];
  for (const p of app.login.setup.problems) {
    if (p.key === "startup.failed") startup.push(p);
  }
  if (startup.length > 0) {
    const items: string[] = [];
    const raws: string[] = [];
    for (const problem of startup) {
      for (const item of startupProblemItems(problem)) {
        // The sign-in page hides external command output; signed-in diagnostics may include it.
        if (loginPage) {
          items.push(startupItemText(item, lang));
          continue;
        }
        items.push(expandText(item.key, (item.params ?? {}) as Params, lang));
        if (typeof item.raw === "string" && item.raw !== "") raws.push(item.raw);
      }
    }
    box.append(renderNotice("startup.failed", {}, lang, { items }));
    // External command output stays outside alert regions.
    for (const raw of raws) box.append(el("pre", { class: "raw-output", text: raw }));
  }
  return box;
}

function settingsNotices(app: App): HTMLElement {
  const box = el("div", { class: "top-notices" });
  for (const p of app.login.setup.problems) {
    if (p.key.startsWith("startup.")) continue;
    // Select the UTF-8 catalog variant from the flag without inventing another outer problem key.
    box.append(renderNotice(settingProblemNoticeKey(p), (p.params ?? {}) as Params, app.lang));
  }
  return box;
}

// Navigation icons are decorative; the visible text remains the accessible link name.
const ICON_STROKE = "http://www.w3.org/2000/svg";
const NAV_ICON_PATHS: Record<string, string[]> = {
  "#/home": ["M4 5h16v6H4z", "M4 13h16v6H4z", "M7 8h0.01", "M7 16h0.01"],
  "#/update": ["M12 3v11", "M7 9l5 5 5-5", "M4 21h16"],
  "#/drivers": ["M4 4h16v5H4z", "M6 9v9a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2V9", "M10 13h4"],
  "#/tasks": ["M9 4h6v3H9z", "M9 5H7a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-2", "M9 14l2 2 4-4"],
};

function navIcon(route: string): HTMLElement {
  const span = el("span", { class: "nav-icon" });
  const svg = document.createElementNS(ICON_STROKE, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  for (const d of NAV_ICON_PATHS[route] ?? []) {
    const path = document.createElementNS(ICON_STROKE, "path");
    path.setAttribute("d", d);
    path.setAttribute("fill", "none");
    path.setAttribute("stroke", "currentColor");
    path.setAttribute("stroke-width", "2");
    path.setAttribute("stroke-linecap", "round");
    path.setAttribute("stroke-linejoin", "round");
    svg.append(path);
  }
  span.append(svg);
  return span;
}

function applyNavTaskState(active: TaskStateKind): void {
  for (const link of Array.from(
    document.querySelectorAll<HTMLAnchorElement>(".app-nav a[data-route='#/tasks']"),
  )) {
    const state = link.querySelector(".nav-task-state");
    if (state) {
      state.textContent = active === null ? "" : taskStateText(active, shell.lang);
    }
    if (active === null) link.removeAttribute("data-task-active");
    else link.setAttribute("data-task-active", active);
  }
}

function gpuLine(device: HomeGpuDevice, lang: App["lang"]): string {
  const name = device.name
    ? device.name
    : expandText("home.gpus.unlisted", { device: device.device }, lang);
  const meta = expandText("home.gpus.meta", { address: device.address, device: device.device }, lang);
  const pass = device.passthrough ? ` · ${expandText("home.gpus.passthrough", {}, lang)}` : "";
  return `${name} · ${meta}${pass}`;
}

function paintHeaderGpu(home: HomeResponse, lang: App["lang"], slot?: HTMLElement | null): void {
  const target = slot ?? document.getElementById("header-gpu");
  if (!target) return;
  clear(target);
  if ("unknown" in home.gpus && home.gpus.unknown) {
    target.append(el("p", { class: "header-gpu-line", text: expandText("home.gpus.unknown", {}, lang) }));
    return;
  }
  const devices = home.gpus.value?.devices ?? [];
  if (devices.length === 0) {
    target.append(el("p", { class: "header-gpu-line", text: expandText("home.gpus.none", {}, lang) }));
    return;
  }
  for (const device of devices) {
    target.append(el("p", { class: "header-gpu-line", text: gpuLine(device, lang) }));
  }
}

function header(app: App): HTMLElement {
  const lang = app.lang;
  const head = el("header", { class: "app-header" });
  const lockup = el("div", { class: "brand-lockup" });
  const title = el("h1", { class: "app-title" });
  title.append(
    brandLockup(),
    el("span", { class: "app-name sr-only", text: expandText("app.title", {}, lang) }),
  );
  headerTitle = title;
  lockup.append(
    title,
    el("p", { class: "app-purpose", text: expandText("app.purpose", {}, lang) }),
  );

  const nav = el("nav", { class: "app-nav" });
  const links: [string, string][] = [
    ["#/home", "nav.home"],
    ["#/update", "nav.update"],
    ["#/drivers", "nav.drivers"],
    ["#/tasks", "nav.tasks"],
  ];
  const current = routeParts(app.route).name;
  for (const [href, key] of links) {
    const link = el("a", { href });
    link.dataset["route"] = href;
    if (current === href.slice(2)) link.setAttribute("aria-current", "page");
    const icon = navIcon(href);
    if (href === "#/tasks") {

      const dot = el("span", { class: "nav-dot" });
      dot.setAttribute("aria-hidden", "true");
      icon.append(dot);
    }
    link.append(
      icon,
      el("span", { class: "nav-label", text: expandText(key, {}, lang) }),
      el("span", { class: "nav-task-state sr-only" }),
    );
    nav.append(link);
  }

  const actions = el("div", { class: "app-actions" });
  const themeLabel = el("label", { for: "theme-control", text: expandText("theme.control", {}, lang) });
  const themeSelect = el("select", { id: "theme-control" });
  for (const t of THEMES) {
    themeSelect.append(el("option", { value: t, text: expandText(themeMessageKey(t), {}, lang) }));
  }
  themeSelect.value = currentTheme();
  themeSelect.addEventListener("change", () => {
    // Theme switching preserves inputs, focus, scroll and pollers by changing only the root attribute.
    const next = themeSelect.value;
    if (isTheme(next)) setTheme(next as Theme);
  });
  actions.append(themeLabel, themeSelect);

  const langLabel = el("label", { for: "lang-control", text: expandText("lang.control", {}, lang) });
  const langSelect = el("select", { id: "lang-control" });
  langSelect.append(
    el("option", { value: "zh", text: expandText("lang.option.zh", {}, lang) }),
    el("option", { value: "en", text: expandText("lang.option.en", {}, lang) }),
  );
  langSelect.value = lang;
  langSelect.addEventListener("change", () => {
    const kept = captureInputs();
    setLang(langSelect.value === "zh" ? "zh" : "en");
    shell.lang = currentLang();
    document.documentElement.lang = htmlLang(shell.lang);
    shell.render();
    restoreInputs(kept);
    document.getElementById("lang-control")?.focus();
  });
  actions.append(langLabel, langSelect);

  if (app.signedIn) {
    const changePw = el("a", {
      href: "#/password",
      text: expandText("nav.change_password", {}, lang),
    });
    const logout = el("button", { type: "button", text: expandText("nav.logout", {}, lang) });
    logout.addEventListener("click", () => {
      void (async () => {
        shell.clearLogoutError();
        try {
          await postLogout();
        } catch (err) {
          // A failed revocation-file write still ends this browser session; other sign-out rejections retain it.
          if (err instanceof ApiError && err.key === "logout.unsaved") {
            shell.setLogoutError(err.key, { ...err.params });
          } else if (!(err instanceof ApiError && err.isSessionEnded)) {
            if (err instanceof ApiError) {
              shell.setLogoutError(err.key, { ...err.params });
            } else {
              shell.setLogoutError("internal", {
                reason: { key: "internal.request_failed", params: {} },
              });
            }
            shell.render();
            return;
          }
        }
        shell.logoutDone = true;
        shell.clearPasswordChanged();

        shell.signOutLocally();
        shell.route = DEFAULT_ROUTE;
        window.location.hash = "";
        shell.render();
        void shell
          .refreshLogin()
          .then(() => {
            // The sign-out result is authoritative; stale login status cannot sign the browser back in.
            shell.signOutLocally();
            shell.render();
          })
          .catch((err: unknown) => {

            shell.setLoginError(err);
            shell.render();
          });
      })();
    });
    actions.append(changePw, logout);
  }
  head.append(lockup);
  if (app.signedIn) {
    app.ensureHome();
    const gpu = el("div", { id: "header-gpu", class: "header-gpu" });
    head.append(gpu);
    // Paint the header element before insertion so cached state survives route and language redraws.
    if (app.homeSnapshot) paintHeaderGpu(app.homeSnapshot, lang, gpu);
  }

  if (app.signedIn) head.append(nav);
  head.append(actions);
  if (app.logoutError) {
    head.append(renderNotice(app.logoutError.key, app.logoutError.params, lang));
  }
  return head;
}

// Language switching preserves unsubmitted inputs by element id.
function captureInputs(): Map<string, string> {
  const kept = new Map<string, string>();
  for (const input of Array.from(document.querySelectorAll("input[id]"))) {
    const el2 = input as HTMLInputElement;
    kept.set(el2.id, el2.value);
  }
  return kept;
}

function restoreInputs(kept: Map<string, string>): void {
  for (const [id, value] of kept) {
    const node = document.getElementById(id);
    if (
      node instanceof HTMLInputElement &&
      node.type !== "checkbox" &&
      node.type !== "radio" &&
      node.type !== "file"
    ) {
      node.value = value;
    }
  }
}

function routeParts(route: string): { name: string; param?: string } {
  const body = route.replace(/^#\/?/, "");
  const parts = body.split("/");
  const name = parts[0] ?? "";
  return { name, param: parts[1] };
}

function renderShell(app: App): void {
  const lang = app.lang;
  document.documentElement.lang = htmlLang(lang);
  document.title = expandText("app.title", {}, lang);
  const description = expandText("app.purpose", {}, lang);
  let meta = document.querySelector('meta[name="description"]');
  if (!(meta instanceof HTMLMetaElement)) {
    meta = document.createElement("meta");
    meta.setAttribute("name", "description");
    document.head.append(meta);
  }
  meta.setAttribute("content", description);

  const container = root();
  clear(container);

  if (app.signedIn) container.removeAttribute("data-signed-out");
  else container.setAttribute("data-signed-out", "");
  container.append(header(app));
  applyNavTaskState(app.navTaskActive);
  if (headerTitle) setPageTitle(headerTitle);
  // The shell owns setup notices on every route to prevent duplicate notices.
  const notices = topNotices(app, !app.signedIn);
  if (notices.childElementCount > 0) container.append(notices);

  const main = el("main", { class: "app-main" });
  container.append(main);

  if (!app.signedIn && app.sessionExpired) {
    main.append(renderNotice("auth.required", {}, lang, { class: "update-error" }));
  }
  // A failed login-status follow-up is visible but cannot restore a session.
  if (!app.signedIn && app.loginError !== null) {
    main.append(apiErrorNotice(app.loginError, lang));
  }

  if (!app.signedIn) {
    renderLogin(app, main);
    return;
  }

  const { name, param } = routeParts(app.route);
  switch (name) {
    case "update":
      renderUpdate(app, main);
      break;
    case "drivers":
      renderDrivers(app, main);
      break;
    case "tasks":
      if (param !== undefined && param !== "") renderTask(app, main, param);
      else renderTasks(app, main);
      break;
    case "password":
      renderPassword(app, main);
      break;
    case "home":
    default:
      renderHome(app, main);
      break;
  }
}

function routeFromHash(): string {
  const hash = window.location.hash;
  if (hash === "" || hash === "#" || hash === "#/") return DEFAULT_ROUTE;
  return hash;
}

window.addEventListener("hashchange", () => {
  // Write pages own their login-status read; another request could settle later and overwrite their gate.
  shell.route = routeFromHash();
  shell.render();
});

export function handleApiError(err: unknown): void {
  if (err instanceof ApiError && err.isSessionEnded) {
    shell.sessionEnded();
  }
}

window.addEventListener("unhandledrejection", (ev) => {
  handleApiError(ev.reason);
});

void (async () => {
  shell.route = routeFromHash();
  try {
    await shell.refreshLogin();
  } catch (err) {

    shell.setLoginError(err);
  }
  shell.render();
})();
