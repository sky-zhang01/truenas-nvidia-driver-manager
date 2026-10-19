import { ApiError, postSetup } from "../api.js";
import { apiErrorNotice, clear, el, expandText, renderNotice } from "../dom.js";
import type { App } from "../main.js";
import { credentialsProblemNode, writeReasonField } from "./login.js";

const PASSWORD_RULE_KEYS: readonly unknown[] = [
  "password.rule.too_short",
  "password.rule.too_long",
  "password.rule.whitespace",
  "password.rule.placeholder",
];

export function renderSetup(app: App, container: HTMLElement): void {
  const lang = app.lang;
  const text = (key: string): string => expandText(key, {}, lang);
  const stack = el("section", { class: "login-stack setup-stack" });
  stack.setAttribute("aria-labelledby", "setup-title");
  stack.append(el("h2", { id: "setup-title", text: text("setup.title") }));
  stack.append(el("p", { class: "login-intro", text: text("setup.intro") }));
  const steps = el("ol", { class: "setup-steps" });
  for (const key of ["setup.step.app", "setup.step.logs", "setup.step.copy"]) {
    steps.append(el("li", { text: text(key) }));
  }
  stack.append(steps);
  const form = el("form", { class: "login-form setup-form" });
  const code = el("input", {
    id: "setup-code", name: "code", type: "text", autocomplete: "off",
    ariaDescribedBy: "setup-note setup-code-error",
  });
  const codeError = el("div", { id: "setup-code-error", class: "form-error", role: "alert" });
  const username = el("input", { id: "setup-user", name: "username", type: "text", autocomplete: "username", value: app.login.setup.username ?? "" });
  const password = el("input", { id: "setup-password", name: "new_password", type: "password", autocomplete: "new-password", ariaDescribedBy: "setup-password-hint setup-error" });
  const repeat = el("input", { id: "setup-repeat", name: "repeat_password", type: "password", autocomplete: "new-password", ariaDescribedBy: "setup-error" });
  code.setAttribute("inputmode", "text");
  code.setAttribute("autocapitalize", "characters");
  code.setAttribute("spellcheck", "false");
  code.setAttribute("placeholder", "XXXX-XXXX-XXXX-XXXX");
  username.setAttribute("readonly", "");
  for (const input of [code, password, repeat]) input.setAttribute("required", "");
  const submit = el("button", { type: "submit", class: "btn-primary", text: text("setup.submit") });
  const error = el("div", { id: "setup-error", class: "form-error", role: "alert" });
  form.append(
    el("label", { for: "setup-code", text: text("setup.code") }), code, codeError,
    el("label", { for: "setup-user", text: text("setup.administrator") }), username,
    el("label", { for: "setup-password", text: text("password.new") }), password,
    el("p", { id: "setup-password-hint", class: "hint", text: text("setup.password_hint") }),
    el("label", { for: "setup-repeat", text: text("setup.repeat") }), repeat, submit, error,
  );
  stack.append(form, el("p", { id: "setup-note", class: "login-help-hint", text: text("setup.note") }));
  const help = el("details", { class: "login-help" });
  help.append(el("summary", { text: text("setup.help.summary") }), el("p", { text: text("setup.help.body") }));
  stack.append(help);
  container.append(stack);

  let busy = false;
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (busy) return;
    clear(error);
    clear(codeError);
    code.removeAttribute("aria-invalid");
    repeat.removeAttribute("aria-invalid");
    password.removeAttribute("aria-invalid");
    if (password.value !== repeat.value) {
      error.append(renderNotice("password.mismatch", {}, lang, { role: "none" }));
      repeat.setAttribute("aria-invalid", "true");
      repeat.focus();
      return;
    }
    const submittedCode = code.value;
    code.value = "";
    busy = true;
    submit.disabled = true;
    form.setAttribute("aria-busy", "true");
    void (async () => {
      try {
        await postSetup(submittedCode, password.value);
        if (!stack.isConnected) return;
        code.value = "";
        password.value = "";
        repeat.value = "";
        await app.refreshLogin(() => stack.isConnected);
        if (!stack.isConnected) return;
        app.clearLogoutDone();
        app.clearSessionExpired();
        app.clearLogoutError();
        app.go("#/home");
      } catch (err) {
        if (!stack.isConnected) return;
        if (err instanceof ApiError) {
          if (err.key === "setup.code_invalid") {
            codeError.append(renderNotice(err.params["code_renewed"] === true ? "setup.code_renewed" : "setup.code_invalid", {}, lang, { role: "none" }));
            if (err.params["show_help"] === true) codeError.append(el("p", { text: text("setup.code_help") }));
            code.value = "";
            code.setAttribute("aria-invalid", "true");
            code.focus();
          } else if (err.key === "setup.not_available") {
            await app.refreshLogin(() => stack.isConnected);
            if (!stack.isConnected) return;
            app.setLoginError(err);
            app.render();
          } else if (err.key === "password.rule") {
            const rule = err.params["rule"];
            const key = typeof rule === "string" && PASSWORD_RULE_KEYS.includes(rule) ? rule : "request.invalid";
            error.append(renderNotice(key, {}, lang, { role: "none" }));
            password.setAttribute("aria-invalid", "true");
            password.focus();
          } else if (err.key === "login.rate_limited") {
            error.append(renderNotice(err.key, { seconds: err.params["retry_after_s"] }, lang, { role: "none" }));
          } else if (err.key === "login.credentials_unsaved") {
            error.append(renderNotice("credentials.unsaved", { write_reason: writeReasonField(err.params["reason"]) }, lang, { role: "none" }));
          } else if (err.key === "login.credentials_unusable") {
            const problem = credentialsProblemNode(err.params["state"], lang);
            if (problem) { problem.removeAttribute("role"); error.append(problem); }
            else error.append(apiErrorNotice(err, lang));
          } else {
            error.append(renderNotice(err.key, { ...err.params }, lang, { role: "none" }));
          }
        } else error.append(apiErrorNotice(err, lang));
      } finally {
        busy = false;
        if (stack.isConnected) { submit.disabled = false; form.removeAttribute("aria-busy"); }
      }
    })();
  });
}
