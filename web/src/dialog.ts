// Closing restores focus to the opener, or the page title if the opener no longer exists.

export interface DialogOrigin {

  trigger?: HTMLElement | null;
}

let pageTitle: HTMLElement | null = null;
let dialogTitleSeq = 0;

export function setPageTitle(element: HTMLElement): void {
  pageTitle = element;
}

function restoreDialogFocus(dialog: HTMLDialogElement): void {
  const originId = dialog.dataset["origin"] ?? "none";
  const trigger = originId === "none" ? null : document.getElementById(originId);
  if (trigger instanceof HTMLElement && trigger.isConnected) {
    trigger.focus();
  } else if (pageTitle) {
    pageTitle.tabIndex = -1;
    pageTitle.focus();
  }
}

export function openDialog(dialog: HTMLDialogElement, title: HTMLElement, origin?: DialogOrigin): void {
  // Associate the title with the native dialog to provide its accessible name.
  if (title.id === "") title.id = `nvm-dialog-title-${(dialogTitleSeq += 1)}`;
  dialog.setAttribute("aria-labelledby", title.id);
  dialog.dataset["origin"] = "none";
  if (origin?.trigger && origin.trigger.isConnected) {
    const id = `nvm-origin-${Math.floor(Math.random() * 1e9)}`;
    origin.trigger.id = id;
    dialog.dataset["origin"] = id;
  }
  // Native Escape must remove the node too, or closed dialogs leave duplicate ids.
  if (dialog.dataset["nvmCancelBound"] !== "1") {
    dialog.dataset["nvmCancelBound"] = "1";
    dialog.addEventListener("cancel", () => {
      if (dialog.open) dialog.close();
      restoreDialogFocus(dialog);
      dialog.remove();
    });
  }
  if (!dialog.open) dialog.showModal();
  title.tabIndex = -1;
  title.focus();
}

export function closeDialog(dialog: HTMLDialogElement): void {
  if (dialog.open) dialog.close();
  restoreDialogFocus(dialog);
}

// Remove modals when a session ends so the sign-in page does not remain inert.
export function closeAllDialogs(): void {
  for (const node of Array.from(document.querySelectorAll("dialog.nvm-dialog"))) {
    if (node instanceof HTMLDialogElement) {
      if (node.open) node.close();
      node.remove();
    }
  }
}
