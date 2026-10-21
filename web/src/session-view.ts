import { ApiError } from "./api.js";
import { apiErrorNotice, el } from "./dom.js";
import type { App } from "./main.js";

/** A successful page read remains visible if the subsequent login-status read fails. */
export async function guardSignedIn(app: App, container: HTMLElement): Promise<boolean> {
  if (!container.isConnected) return false;
  try {
    await app.refreshLogin(() => container.isConnected);
    if (!container.isConnected) return false;
    if (!app.signedIn) {
      app.render();
      return false;
    }
  } catch (error) {
    if (!container.isConnected) return false;
    if (error instanceof ApiError && error.isSessionEnded) {
      app.sessionEnded();
      return false;
    }
    const readError = el("div", { id: "login-read-error" });
    readError.append(apiErrorNotice(error, app.lang));
    container.append(readError);
  }
  return true;
}
