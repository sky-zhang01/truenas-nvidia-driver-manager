// Sign-in diagnostics omit external command output. Account help uses a generic credentials-file path.

import { postLogin, requestSshChallenge, postSshLogin, ApiError, type LoginInfo, type SetupProblem } from "../api.js";
import {
  el,
  clear,
  expand,
  expandText,
  appendInline,
  renderNotice,
  type Params,
} from "../dom.js";
import {
  CHALLENGE_PLACEHOLDER,
  ChallengeSequencer,
  agentPickSignCommand,
  agentSignCommand,
  checkSignatureText,
  copyText,
  fileSignCommand,
} from "../ssh.js";
import { renderSetup } from "./setup.js";
import type { App } from "../main.js";
import type { Lang } from "../messages.js";

// Startup items on the sign-in page omit external command output.
export function startupItemText(p: SetupProblem, lang: Lang): string {
  const params = { ...(p.params ?? {}) } as Params;
  const mr = params["mapping_reason"];
  if (mr && typeof mr === "object" && (mr as { key?: string }).key === "startup.mapping.inspect_failed") {
    params["mapping_reason"] = { key: "startup.mapping.inspect_failed.login", params: {} };
  }
  return expandText(p.key, params, lang);
}

// Startup problem items retain their own keys and params under the startup.failed outer key.
export function startupProblemItems(problem: SetupProblem): SetupProblem[] {
  if (problem.key !== "startup.failed") return [];
  const items = (problem.params ?? {})["items"];
  if (!Array.isArray(items)) return [];
  const out: SetupProblem[] = [];
  for (const entry of items) {
    if (entry === null || typeof entry !== "object") continue;
    if (typeof (entry as { key?: unknown }).key !== "string") continue;
    out.push(entry as SetupProblem);
  }
  return out;
}

export function writeReasonField(reason: unknown): unknown {
  return reason === "file" || reason === "directory"
    ? { key: `credentials.unsaved_reason.${reason}`, params: {} }
    : "UNKNOWN";
}

// Only recognized credentials-state shapes produce a notice; unknown shapes return null.
export function credentialsProblemNode(state: unknown, lang: Lang): HTMLElement | null {
  if (state === "username_invalid") {

    return renderNotice(
      "setting.invalid",
      {
        variable: "NVM_INITIAL_ADMIN_USERNAME",
        requirement: { key: "setting.rule.username", params: {} },
      },
      lang,
    );
  }
  if (typeof state === "object" && state !== null && "unreadable" in state) {
    const reason = String((state as { unreadable: unknown }).unreadable);
    if (reason === "file_unreadable") {
      return renderNotice("credentials.unreadable.file_unreadable", {}, lang);
    }
    return renderNotice(
      "credentials.unreadable",
      { reason: { key: `credentials.reason.${reason}`, params: {} } },
      lang,
    );
  }
  if (typeof state === "object" && state !== null && "unsaved" in state) {
    return renderNotice(
      "credentials.unsaved",
      { write_reason: writeReasonField((state as { unsaved: unknown }).unsaved) },
      lang,
    );
  }
  return null;
}

export function needsResetHelp(state: unknown): boolean {
  return (
    typeof state === "object" &&
    state !== null &&
    "unreadable" in state &&
    (state as { unreadable: unknown }).unreadable !== "directory_unreadable"
  );
}

// Choose the UTF-8 catalog variant from invalid_utf8 while retaining the closed outer problem-key set.
export function settingProblemNoticeKey(problem: SetupProblem): string {
  if (problem.key === "setting.invalid" && (problem.params ?? {})["invalid_utf8"] === true) {
    return "setting.invalid_utf8";
  }
  return problem.key;
}

// Suppress a credentials username notice already present in setup.problems.
export function setupHasUsernameProblem(problems: readonly SetupProblem[]): boolean {
  for (const p of problems) {
    // Both username error keys denote the same setting and must not produce duplicate lines.
    if (p.key !== "setting.invalid" && p.key !== "setting.invalid_utf8") continue;
    if ((p.params ?? {})["variable"] === "NVM_INITIAL_ADMIN_USERNAME") return true;
  }
  return false;
}

async function refreshSetupView(app: App, owner: () => boolean): Promise<void> {
  try {
    await app.refreshLogin(owner);
  } catch {
    // refreshLogin keeps the failed status read visible in the shell.
  }
  if (owner()) app.render();
}

function resetLocationNode(lang: Lang): HTMLElement {
  const location = el("p");
  location.append(
    el("code", { text: expandText("login.help.reset_path", {}, lang) }),
    el("span", { text: ` ${expandText("login.help.reset_location_unknown", {}, lang)}` }),
  );
  return location;
}

function resetHelpNode(lang: Lang, alert = true): HTMLElement {
  const node = renderNotice("help.reset_account", {}, lang, { role: alert ? "alert" : "none" });
  node.append(resetLocationNode(lang));
  return node;
}

// Account guidance uses the YAML data-directory placeholder for every visitor.
function renderAccountHelp(lang: Lang): HTMLElement {
  const details = el("details", { class: "login-help" });
  details.append(el("summary", { text: expandText("login.help.summary", {}, lang) }));
  const list = el("ul", { class: "login-help-list" });
  for (const key of [
    "login.help.no_account",
    "login.help.after_reinstall",
    "login.help.forgot_password",
  ] as const) {
    const item = el("li");
    appendInline(item, expand(key, {}, lang).segments.flatMap((s) => s.nodes));
    list.append(item);
  }
  details.append(list, resetLocationNode(lang));
  return details;
}

function commandRow(
  labelKey: string,
  command: string,
  lang: Lang,
  onCopy: (value: string, button: HTMLElement) => void,
): { row: HTMLElement; code: HTMLElement } {
  const row = el("div", { class: "ssh-command" });
  const code = el("code", { class: "ssh-command-code", text: command });
  const copy = el("button", {
    type: "button",
    class: "ssh-copy",
    text: expandText("ssh.login.copy", {}, lang),
  });
  copy.addEventListener("click", () => onCopy(code.textContent ?? "", copy));
  row.append(
    el("span", { class: "ssh-command-label", text: expandText(labelKey, {}, lang) }),
    code,
    copy,
  );
  return { row, code };
}

// Render SSH sign-in only when explicitly enabled. Reject recognizable private-key paste before
// sending a request.
function renderSshSection(app: App, stack: HTMLElement): void {
  const lang = app.lang;
  const details = el("details", { class: "login-ssh" });
  details.append(el("summary", { text: expandText("ssh.login.summary", {}, lang) }));
  const body = el("div", { class: "login-ssh-body" });
  details.append(body);

  const sequencer = new ChallengeSequencer();
  // Every timer belongs to this page; a redrawn sign-in page must not be patched by its predecessor.
  const pendingTimers = new Set<number>();
  app.addCleanup(() => {
    sequencer.reset();
    for (const timer of pendingTimers) window.clearTimeout(timer);
    pendingTimers.clear();
  });
  const later = (fn: () => void, ms: number): void => {
    const timer = window.setTimeout(() => {
      pendingTimers.delete(timer);
      if (details.isConnected) fn();
    }, ms);
    pendingTimers.add(timer);
  };
  let current: { id: string; challenge: string } | null = null;
  let fetched = false;
  const challengeCode = el("code", { id: "ssh-challenge", text: CHALLENGE_PLACEHOLDER });
  const challengeLine = el("p", { class: "ssh-challenge-line" });
  challengeLine.setAttribute("aria-live", "polite");
  challengeLine.append(challengeCode);
  const refresh = el("button", {
    type: "button",
    id: "ssh-refresh",
    text: expandText("ssh.login.refresh", {}, lang),
  });
  const REFRESH_ATTENTION = "ssh-refresh-attention";
  // Invalid-challenge emphasis and focus stay on Refresh until the newest challenge fetch succeeds.
  const emphasizeRefresh = (): void => {
    refresh.classList.add(REFRESH_ATTENTION);
    refresh.focus();
  };
  const errorBox = el("div", { class: "form-error", role: "alert" });

  const templates: [(challenge: string) => string, string][] = [
    [(challenge) => fileSignCommand(challenge, "~/.ssh/id_ed25519"), "ssh.login.command_file"],
    [(challenge) => agentSignCommand(challenge, "~/.1password/agent.sock"), "ssh.login.command_agent"],
    [
      (challenge) => agentPickSignCommand(challenge, "~/.ssh/registered-key.pub"),
      "ssh.login.command_agent_multi",
    ],
  ];
  const commandRows: { code: HTMLElement; build: (challenge: string) => string }[] = [];
  const commands = el("div", { class: "ssh-commands" });
  const onCopy = (value: string, button: HTMLElement): void => {
    void (async () => {
      // Copy feedback reflects the actual outcome; leave text selectable when both copy paths fail.
      if (await copyText(value)) {
        button.textContent = expandText("ssh.login.copied", {}, lang);
        later(() => {
          button.textContent = expandText("ssh.login.copy", {}, lang);
        }, 2000);
      }
    })();
  };
  // Only the newest successful challenge value is copyable.
  const challengeCopy = el("button", {
    type: "button",
    id: "ssh-challenge-copy",
    class: "ssh-copy ssh-challenge-copy",
    text: expandText("ssh.login.copy", {}, lang),
  });
  challengeCopy.disabled = true;
  challengeCopy.addEventListener("click", () => {
    // The placeholder is not a real challenge and cannot be copied.
    if (current === null) return;
    onCopy(current.challenge, challengeCopy);
  });
  for (const [build, labelKey] of templates) {
    const { row, code } = commandRow(labelKey, build(CHALLENGE_PLACEHOLDER), lang, onCopy);
    commandRows.push({ code, build });
    commands.append(row);
  }

  const setChallengeDisplay = (challenge: string): void => {
    challengeCode.textContent = challenge;
    for (const row of commandRows) row.code.textContent = row.build(challenge);
  };

  const fetchChallenge = (): void => {
    const seq = sequencer.next();
    void requestSshChallenge()
      .then((got) => {
        // An earlier request or departed page cannot overwrite the current challenge.
        if (!sequencer.isCurrent(seq) || !details.isConnected) return;
        current = { id: got.challenge_id, challenge: got.challenge };
        setChallengeDisplay(got.challenge);

        refresh.classList.remove(REFRESH_ATTENTION);
        challengeCopy.disabled = false;
      })
      .catch(async (err: unknown) => {
        if (!sequencer.isCurrent(seq) || !details.isConnected) return;
        clear(errorBox);
        if (err instanceof ApiError) {
          if (err.key === "login.setup_required") {
            await refreshSetupView(app, () => details.isConnected);
            return;
          }
          if (err.key === "ssh.challenge_invalid") emphasizeRefresh();
          errorBox.append(renderNotice(err.key, { ...err.params }, lang, { role: "none" }));
        } else {
          errorBox.append(
            renderNotice(
              "internal",
              { reason: { key: "internal.request_failed", params: {} } },
              lang,
              { role: "none" },
            ),
          );
        }
      });
  };

  refresh.addEventListener("click", () => {
    clear(errorBox);
    fetchChallenge();
  });
  details.addEventListener("toggle", () => {

    if (details.open && !fetched) {
      fetched = true;
      fetchChallenge();
    }
  });

  const steps = el("ol", { class: "ssh-steps" });
  const step1 = el("li");
  appendInline(step1, expand("ssh.login.step_challenge", {}, lang).segments.flatMap((s) => s.nodes));
  const challengeLabel = el("span", {
    class: "ssh-challenge-label",
    text: expandText("ssh.login.challenge_label", {}, lang),
  });
  const challengeActions = el("div", { class: "ssh-challenge-actions" });
  challengeActions.append(challengeCopy, refresh);
  step1.append(
    challengeLabel,
    challengeLine,
    challengeActions,
    el("span", { class: "ssh-expires", text: expandText("ssh.login.expires", {}, lang) }),
  );
  const step2 = el("li");
  appendInline(step2, expand("ssh.login.step_sign", {}, lang).segments.flatMap((s) => s.nodes));
  step2.append(commands);
  const pick = el("p", { class: "ssh-command-pick" });
  appendInline(pick, expand("ssh.login.command_pick", {}, lang).segments.flatMap((s) => s.nodes));
  step2.append(pick);
  const step3 = el("li");
  appendInline(step3, expand("ssh.login.step_paste", {}, lang).segments.flatMap((s) => s.nodes));
  steps.append(step1, step2, step3);

  const form = el("form", { class: "login-form ssh-login-form" });
  const signatureLabel = el("label", {
    for: "ssh-signature",
    text: expandText("ssh.login.signature_label", {}, lang),
  });
  const signature = el("textarea", { id: "ssh-signature", name: "signature" }) as HTMLTextAreaElement;
  signature.setAttribute("rows", "6");
  signature.setAttribute("spellcheck", "false");
  const fileLabel = el("label", {
    for: "ssh-sigfile",
    text: expandText("ssh.login.pick_file", {}, lang),
  });
  const file = el("input", { id: "ssh-sigfile", type: "file", accept: ".sig" });
  file.addEventListener("change", () => {
    const chosen = (file as HTMLInputElement).files?.[0];
    if (!chosen) return;
    const reader = new FileReader();
    reader.addEventListener("load", () => {
      // Apply a completed file read only while its owning block remains live.
      if (details.isConnected) signature.value = String(reader.result ?? "");
    });
    reader.readAsText(chosen);
  });
  const submit = el("button", {
    type: "submit",
    class: "btn-primary",
    text: expandText("ssh.login.submit", {}, lang),
  });
  form.append(signatureLabel, signature, fileLabel, file, submit, errorBox);

  let sshBusy = false;
  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    if (sshBusy) return;
    void (async () => {
      clear(errorBox);
      // Reject private-key material without echoing it, clear the input and send no request.
      const check = checkSignatureText(signature.value);
      if (!check.ok) {
        if (check.reason === "private_key") {
          signature.value = "";
          errorBox.append(renderNotice("ssh.private_refused", {}, lang, { role: "none" }));
        } else {
          errorBox.append(renderNotice("ssh.signature_bad_format", {}, lang, { role: "none" }));
        }
        return;
      }
      if (current === null) {
        errorBox.append(renderNotice("ssh.challenge_invalid", {}, lang, { role: "none" }));
        return;
      }
      const challengeId = current.id;
      sshBusy = true;
      submit.disabled = true;
      try {
        await postSshLogin(challengeId, signature.value);
        if (!details.isConnected) return;
        await app.refreshLogin(() => details.isConnected);
        if (!details.isConnected) return;
        app.clearLogoutDone();
        app.clearSessionExpired();
        app.clearLogoutError();
        app.go("#/home");
      } catch (err) {
        sshBusy = false;
        if (details.isConnected) submit.disabled = false;
        // A departed page no longer owns the in-flight response.
        if (!details.isConnected) return;
        if (err instanceof ApiError) {
          if (err.key === "login.setup_required") {
            await refreshSetupView(app, () => details.isConnected);
            return;
          }
          const params: Params = { ...err.params };
          if (err.key === "login.rate_limited" && params["seconds"] === undefined) {
            params["seconds"] = params["retry_after_s"] ?? "UNKNOWN";
          }
          // A consumed one-shot challenge returns to the placeholder to prevent signing stale bytes.
          if (
            err.key === "ssh.challenge_invalid" ||
            err.key === "ssh.login_failed" ||
            err.key === "ssh.key_not_registered"
          ) {
            current = null;
            setChallengeDisplay(CHALLENGE_PLACEHOLDER);

            challengeCopy.disabled = true;
            if (err.key === "ssh.challenge_invalid") emphasizeRefresh();
          }
          if (err.key === "login.credentials_unusable") {

            const problem = credentialsProblemNode(params["state"], lang);
            if (problem) {
              problem.removeAttribute("role");
              errorBox.append(problem);
              if (needsResetHelp(params["state"])) {
                errorBox.append(resetHelpNode(lang, false));
              }
            } else {
              errorBox.append(renderNotice(err.key, params, lang, { role: "none" }));
            }
          } else {
            errorBox.append(renderNotice(err.key, params, lang, { role: "none" }));
            if (err.params["show_reset_help"] === true) {
              errorBox.append(resetHelpNode(lang, false));
            }
          }
        } else {
          errorBox.append(
            renderNotice(
              "internal",
              { reason: { key: "internal.request_failed", params: {} } },
              lang,
              { role: "none" },
            ),
          );
        }
      }
    })();
  });

  body.append(steps, form);
  stack.append(details);
}

export function renderLogin(app: App, container: HTMLElement): void {
  const lang = app.lang;
  const info: LoginInfo = app.login;
  if (info.credentials === "setup_required") {
    renderSetup(app, container);
    return;
  }

  const stack = el("div", { class: "login-stack" });
  container.append(stack);

  stack.append(el("h2", { text: expandText("page.login", {}, lang) }));

  stack.append(el("p", { class: "login-intro", text: expandText("login.intro", {}, lang) }));

  if (app.logoutDone) {
    stack.append(renderNotice("logout.done", {}, lang));
  }

  const credProblem = credentialsProblemNode(info.credentials, lang);
  if (
    credProblem &&
    // The shell owns a duplicate username problem already present in setup.problems.
    !(info.credentials === "username_invalid" && setupHasUsernameProblem(info.setup.problems))
  ) {
    stack.append(credProblem);
  }
  if (info.show_reset_help) {
    stack.append(resetHelpNode(lang));
  }

  const form = el("form", { class: "login-form" });
  const userLabel = el("label", { for: "login-user", text: expandText("login.username", {}, lang) });
  const user = el("input", {
    id: "login-user",
    name: "username",
    type: "text",
    autocomplete: "username",
  });
  const passLabel = el("label", { for: "login-pass", text: expandText("login.password", {}, lang) });
  const pass = el("input", {
    id: "login-pass",
    name: "password",
    type: "password",
    autocomplete: "current-password",
  });
  const submit = el("button", { type: "submit", class: "btn-primary", text: expandText("login.submit", {}, lang) });
  const errorBox = el("div", { class: "form-error", role: "alert" });

  form.append(userLabel, user, passLabel, pass, submit, errorBox);
  stack.append(form);

  stack.append(
    el("p", { class: "login-help-hint", text: expandText("login.help.hint", {}, lang) }),
  );

  stack.append(renderAccountHelp(lang));

  if (info.ssh_enabled === true) {
    renderSshSection(app, stack);
  }

  let loginBusy = false;
  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    if (loginBusy) return;
    void (async () => {
      clear(errorBox);
      if (user.value === "" || pass.value === "") {
        errorBox.append(renderNotice("login.fields_required", {}, lang, { role: "none" }));
        return;
      }
      loginBusy = true;
      submit.disabled = true;
      try {
        await postLogin(user.value, pass.value);
        pass.value = "";
        await app.refreshLogin();
        app.clearLogoutDone();
        app.clearSessionExpired();
        app.clearLogoutError();

        app.go("#/home");
      } catch (err) {
        loginBusy = false;
        submit.disabled = false;
        if (err instanceof ApiError) {
          if (err.key === "login.setup_required") {
            await refreshSetupView(app, () => stack.isConnected);
            return;
          }
          // The catalog expects seconds; the API field is retry_after_s.
          const params: Params = { ...err.params };
          if (err.key === "login.rate_limited" && params["seconds"] === undefined) {
            params["seconds"] = params["retry_after_s"] ?? "UNKNOWN";
          }
          if (err.key === "login.credentials_unusable") {

            const problem = credentialsProblemNode(params["state"], lang);
            if (problem) {
              // The form error box owns the alert region; keep the nested notice out of it.
              problem.removeAttribute("role");
              errorBox.append(problem);
              if (needsResetHelp(params["state"])) {
                errorBox.append(
                  resetHelpNode(lang, false),
                );
              }
            } else {
              errorBox.append(renderNotice(err.key, params, lang, { role: "none" }));
            }
          } else if (err.key === "login.credentials_unsaved") {
            // The catalog expects write_reason; the API field is reason.
            errorBox.append(
              renderNotice(
                "credentials.unsaved",
                { write_reason: writeReasonField(err.params["reason"]) },
                lang,
                { role: "none" },
              ),
            );
          } else {
            errorBox.append(renderNotice(err.key, params, lang, { role: "none" }));
            if (err.params["show_reset_help"] === true) {
              errorBox.append(resetHelpNode(lang, false));
            }
          }
        } else {
          errorBox.append(
            renderNotice(
              "internal",
              { reason: { key: "internal.request_failed", params: {} } },
              lang,
              { role: "none" },
            ),
          );
        }
      }
    })();
  });
}
