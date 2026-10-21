// Reject recognizable private-key material in either public-key or comment inputs before sending a request.

import {
  postPassword,
  getSshKeys,
  postSshKey,
  deleteSshKey,
  ApiError,
  type SshKeyItem,
} from "../api.js";
import { el, clear, expandText, renderNotice, appendInline, expand } from "../dom.js";
import {
  CHALLENGE_PLACEHOLDER,
  checkPublicKeyText,
  looksLikePrivateKey,
  copyText,
  registeredAgentCommand,
} from "../ssh.js";
import { closeDialog, makeDialog } from "../dialog.js";
import type { App } from "../main.js";
import type { Lang } from "../messages.js";
import { writeReasonField } from "./login.js";

export function renderPassword(app: App, container: HTMLElement): void {
  const lang = app.lang;
  container.append(el("h2", { text: expandText("password.title_voluntary", {}, lang) }));
  container.append(el("h3", { text: expandText("password.section", {}, lang) }));

  const form = el("form", { class: "password-form" });
  const newLabel = el("label", { for: "pass-new", text: expandText("password.new", {}, lang) });
  const newInput = el("input", {
    id: "pass-new",
    name: "new_password",
    type: "password",
    autocomplete: "new-password",
  });
  const repeatLabel = el("label", {
    for: "pass-repeat",
    text: expandText("password.repeat", {}, lang),
  });
  const repeatInput = el("input", {
    id: "pass-repeat",
    name: "repeat_password",
    type: "password",
    autocomplete: "new-password",
  });
  const submit = el("button", { type: "submit", text: expandText("password.submit", {}, lang) });
  const errorBox = el("div", { class: "form-error" });
  const doneBox = el("div", { class: "form-done", role: "status" });

  form.append(newLabel, newInput, repeatLabel, repeatInput, submit, errorBox, doneBox);
  container.append(form);

  let passwordBusy = false;
  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    if (passwordBusy) return;
    void (async () => {
      clear(errorBox);
      clear(doneBox);
      if (newInput.value !== repeatInput.value) {
        errorBox.append(renderNotice("password.mismatch", {}, lang));
        return;
      }
      passwordBusy = true;
      submit.disabled = true;
      try {
        await postPassword(newInput.value);
        newInput.value = "";
        repeatInput.value = "";
        doneBox.append(renderNotice("password.changed", {}, lang));

        app.setPasswordChanged();
        await app.refreshLogin();
        app.go("#/home");
      } catch (err) {
        passwordBusy = false;
        submit.disabled = false;
        if (err instanceof ApiError && err.isSessionEnded) {
          app.sessionEnded();
          return;
        }
        if (err instanceof ApiError) {
          if (err.key === "password.unsaved") {
            // The catalog expects write_reason; the API field is reason.
            errorBox.append(
              renderNotice(
                "credentials.unsaved",
                { write_reason: writeReasonField(err.params["reason"]) },
                lang,
              ),
            );
          } else {
            errorBox.append(renderNotice(err.key, { ...err.params }, lang));
          }
        } else {
          errorBox.append(
            renderNotice("internal", { reason: { key: "internal.request_failed", params: {} } }, lang),
          );
        }
      }
    })();
  });

  renderSshKeys(app, container);
}

function addedAtText(addedAt: number): string {
  const date = new Date(addedAt);
  if (Number.isNaN(date.getTime())) return "UNKNOWN";
  const pad = (n: number): string => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

function keyInvalidReason(params: Record<string, unknown>): unknown {
  const reason = params["reason"];
  if (reason !== null && typeof reason === "object" && "key" in (reason as object)) {
    return reason;
  }
  return { key: "ssh.key_invalid.bad_format", params: {} };
}

function renderSshKeys(app: App, container: HTMLElement): void {
  const lang = app.lang;
  const section = el("section", { class: "ssh-keys" });
  section.append(el("h3", { text: expandText("ssh.keys.title", {}, lang) }));
  const intro = el("p", { class: "ssh-keys-intro" });
  appendInline(intro, expand("ssh.keys.intro", {}, lang).segments.flatMap((s) => s.nodes));
  section.append(intro);

  const listBox = el("div", { class: "ssh-keys-list" });
  // Copy-feedback timers belong to this page and cannot patch a redrawn section.
  const pendingTimers = new Set<number>();
  app.addCleanup(() => {
    for (const timer of pendingTimers) window.clearTimeout(timer);
    pendingTimers.clear();
  });
  const later = (fn: () => void, ms: number): void => {
    const timer = window.setTimeout(() => {
      pendingTimers.delete(timer);
      if (section.isConnected) fn();
    }, ms);
    pendingTimers.add(timer);
  };
  const listError = el("div", { class: "form-error" });
  const noticeBox = el("div", { class: "form-done", role: "status" });
  section.append(listBox, listError, noticeBox);

  const owner = (): boolean => section.isConnected;

  const renderList = (items: SshKeyItem[]): void => {
    clear(listBox);
    if (items.length === 0) {
      listBox.append(el("p", { class: "ssh-keys-empty", text: expandText("ssh.keys.empty", {}, lang) }));
      return;
    }
    for (const item of items) {
      const row = el("div", { class: "ssh-key-row" });
      const head = el("div", { class: "ssh-key-head" });
      // Key comments enter the DOM as plain text and never enter shell commands.
      head.append(
        el("span", { class: "ssh-key-algorithm", text: item.algorithm }),
        el("code", { class: "ssh-key-fingerprint", text: item.fingerprint }),
        el("span", { class: "ssh-key-comment", text: item.comment }),
        el("span", { class: "ssh-key-added-at", text: addedAtText(item.added_at) }),
      );
      row.append(head);
      const commandBox = el("div", { class: "ssh-key-command" });
      commandBox.append(
        el("span", {
          class: "ssh-command-label",
          text: expandText(
            "ssh.keys.sign_command",
            { challenge_placeholder: CHALLENGE_PLACEHOLDER },
            lang,
          ),
        }),
      );
      // Export the socket on its own line; the sign command uses validated canonical key bytes.
      const command = registeredAgentCommand(
        CHALLENGE_PLACEHOLDER,
        item.public_key,
        "~/.1password/agent.sock",
      );
      if (command !== null) {
        const code = el("code", { class: "ssh-command-code", text: command });
        const copy = el("button", {
          type: "button",
          class: "ssh-copy",
          text: expandText("ssh.login.copy", {}, lang),
        });
        copy.addEventListener("click", () => {
          void (async () => {
            if (await copyText(command)) {
              copy.textContent = expandText("ssh.login.copied", {}, lang);
              later(() => {
                copy.textContent = expandText("ssh.login.copy", {}, lang);
              }, 2000);
            }
          })();
        });
        commandBox.append(code, copy);
      }
      row.append(commandBox);
      const remove = el("button", {
        type: "button",
        class: "ssh-key-delete",
        text: expandText("ssh.keys.delete", {}, lang),
      });
      row.append(remove);
      remove.addEventListener("click", () => {
        confirmRemoveKey(app, item, lang, items.length === 1, () => reload());
      });
      listBox.append(row);
    }
  };

  const reload = (): Promise<void> =>
    getSshKeys()
      .then((response) => {
        if (!owner()) return;
        app.syncSshEnabled(response.items.length > 0);
        renderList(response.items);
      })
      .catch((err: unknown) => {
        if (!owner()) return;
        if (err instanceof ApiError && err.isSessionEnded) {
          app.sessionEnded();
          return;
        }
        clear(listError);
        listError.append(renderNotice("ssh.keys.load_failed", {}, lang));
      });

  void reload();

  const addForm = el("form", { class: "ssh-keys-add" });
  const addTitle = el("h4", { text: expandText("ssh.keys.add_title", {}, lang) });
  const keyLabel = el("label", { for: "ssh-add-key", text: expandText("ssh.keys.public_key", {}, lang) });
  const keyInput = el("textarea", { id: "ssh-add-key", name: "public_key" }) as HTMLTextAreaElement;
  keyInput.setAttribute("rows", "2");
  keyInput.setAttribute("spellcheck", "false");
  const commentLabel = el("label", { for: "ssh-add-comment", text: expandText("ssh.keys.comment", {}, lang) });
  const commentInput = el("input", { id: "ssh-add-comment", name: "comment", type: "text" });
  const addSubmit = el("button", { type: "submit", text: expandText("ssh.keys.add_submit", {}, lang) });
  const addError = el("div", { class: "form-error", role: "alert" });
  addForm.append(addTitle, keyLabel, keyInput, commentLabel, commentInput, addSubmit, addError);
  section.append(addForm);

  addForm.addEventListener("submit", (ev) => {
    ev.preventDefault();
    void (async () => {
      clear(addError);
      // The private-key boundary includes the comment input; clear offending material without echoing it.
      if (looksLikePrivateKey(commentInput.value)) {
        commentInput.value = "";
        addError.append(renderNotice("ssh.private_refused", {}, lang, { role: "none" }));
        return;
      }
      const check = checkPublicKeyText(keyInput.value);
      if (!check.ok) {
        if (check.reason === "private_key") {
          keyInput.value = "";
          addError.append(renderNotice("ssh.private_refused", {}, lang, { role: "none" }));
        } else {
          addError.append(renderNotice("ssh.public_key_bad_format", {}, lang, { role: "none" }));
        }
        return;
      }
      try {
        const item = await postSshKey(check.line, commentInput.value.trim());
        keyInput.value = "";
        commentInput.value = "";
        clear(noticeBox);
        noticeBox.append(
          renderNotice("ssh.keys.added", { fingerprint: item.fingerprint }, lang, { role: "none" }),
        );
        await reload();
      } catch (err) {
        if (err instanceof ApiError && err.isSessionEnded) {
          app.sessionEnded();
          return;
        }
        if (err instanceof ApiError) {
          if (err.key === "ssh.key_invalid") {
            addError.append(
              renderNotice("ssh.key_invalid", { reason: keyInvalidReason(err.params) }, lang, {
                role: "none",
              }),
            );
          } else if (err.key === "ssh.keys_unsaved") {

            addError.append(
              renderNotice(
                "ssh.keys_unsaved",
                { write_reason: writeReasonField(err.params["reason"]) },
                lang,
                { role: "none" },
              ),
            );
          } else {
            addError.append(renderNotice(err.key, { ...err.params }, lang, { role: "none" }));
          }
        } else {
          if (section.isConnected) {
            addError.append(
              renderNotice("internal", { reason: { key: "internal.request_failed", params: {} } }, lang, {
                role: "none",
              }),
            );
          }
        }
      }
    })();
  });

  container.append(section);

  // Revoking a key invalidates every session; the replacement cookie keeps this browser signed in.
  function confirmRemoveKey(
    app2: App,
    item: SshKeyItem,
    lang2: Lang,
    last: boolean,
    onDone: () => Promise<void> | void,
  ): void {
    const { dialog, show } = makeDialog({ title: expandText("ssh.keys.delete", {}, lang2) });
    const lead = el("p");
    if (last) {
      appendInline(lead, expand("ssh.keys.delete_last", {}, lang2).segments.flatMap((s) => s.nodes));
    }
    const body = el("p");
    appendInline(body, expand("ssh.keys.delete_confirm", {}, lang2).segments.flatMap((s) => s.nodes));
    const fingerprint = el("p");
    fingerprint.append(el("code", { text: item.fingerprint }));
    const cancel = el("button", { type: "button", text: expandText("dialog.close", {}, lang2) });
    const confirm = el("button", {
      type: "button",
      class: "btn-primary",
      text: expandText("ssh.keys.delete", {}, lang2),
    });
    const error = el("div", { class: "form-error" });
    const actions = el("div", { class: "dialog-actions" });
    actions.append(cancel, confirm);
    if (last) dialog.append(lead);
    dialog.append(body, fingerprint, error, actions);
    show();
    app2.addCleanup(() => { if (dialog.open) dialog.close(); dialog.remove(); });
    cancel.addEventListener("click", () => {
      closeDialog(dialog);
      dialog.remove();
    });
    dialog.addEventListener("cancel", () => {
      closeDialog(dialog);
      dialog.remove();
    });
    confirm.addEventListener("click", () => {
      void (async () => {
        clear(error);
        try {
          // The browser applies the HttpOnly replacement cookie; refresh the list without signing out locally.
          await deleteSshKey(item.id);
          closeDialog(dialog);
          dialog.remove();
          clear(noticeBox);
          noticeBox.append(renderNotice("ssh.keys.deleted", {}, lang2, { role: "none" }));
          await onDone();
          if (!section.isConnected) return;
          const remaining = section.querySelector(".ssh-key-delete");
          if (remaining instanceof HTMLElement) {
            remaining.focus();
          } else {
            document.getElementById("ssh-add-key")?.focus();
          }
        } catch (err) {
          if (err instanceof ApiError && err.isSessionEnded) {
            closeDialog(dialog);
            dialog.remove();
            app2.sessionEnded();
            return;
          }
          if (err instanceof ApiError) {
            if (err.key === "ssh.keys_unsaved") {
              error.append(
                renderNotice(
                  "ssh.keys_unsaved",
                  { write_reason: writeReasonField(err.params["reason"]) },
                  lang2,
                ),
              );
            } else {
              error.append(renderNotice(err.key, { ...err.params }, lang2));
            }
          } else {
            error.append(
              renderNotice("internal", { reason: { key: "internal.request_failed", params: {} } }, lang2),
            );
          }
        }
      })();
    });
  }
}
