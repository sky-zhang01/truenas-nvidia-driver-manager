// Recognizable private key material must never leave the browser. The server independently validates
// all submitted values.

// Recognize private-key armor across OpenSSH, PKCS#8, PEM and encrypted forms.
const PRIVATE_KEY_ARMOR = /-----BEGIN[ A-Z0-9]*PRIVATE KEY-----/;

export function looksLikePrivateKey(text: string): boolean {
  return PRIVATE_KEY_ARMOR.test(text);
}

export type PublicKeyCheck =
  | { ok: true; line: string }
  | { ok: false; reason: "private_key" | "bad_format" };

// Accept one supported public key line and an optional comment; trim surrounding CR/LF before submitting.
export function checkPublicKeyText(raw: string): PublicKeyCheck {
  if (looksLikePrivateKey(raw)) return { ok: false, reason: "private_key" };
  const line = raw.replace(/[\r\n]+$/, "").trim();
  const shape =
    /^(ssh-ed25519|ecdsa-sha2-nistp256|ecdsa-sha2-nistp384)\s+[A-Za-z0-9+/]+={0,2}(\s.*)?$/;
  if (!shape.test(line) || line.includes("\n")) {
    return { ok: false, reason: "bad_format" };
  }
  return { ok: true, line };
}

export type SignatureCheck =
  | { ok: true; value: string }
  | { ok: false; reason: "private_key" | "bad_format" };

export function checkSignatureText(raw: string): SignatureCheck {
  if (looksLikePrivateKey(raw)) return { ok: false, reason: "private_key" };
  const value = raw.trim();
  if (value === "") return { ok: false, reason: "bad_format" };
  if (
    !value.startsWith("-----BEGIN SSH SIGNATURE-----") ||
    !value.endsWith("-----END SSH SIGNATURE-----")
  ) {
    return { ok: false, reason: "bad_format" };
  }
  return { ok: true, value };
}

// Only validated literal values may enter single-quoted shell arguments; reject quotes, newlines,
// dollars and backticks.
export function isSafeLiteral(value: string): boolean {
  return value !== "" && !/['\n$`]/.test(value);
}

export function canonicalKeyFields(line: string): string | null {
  const match = /^(ssh-ed25519|ecdsa-sha2-nistp256|ecdsa-sha2-nistp384)\s+[A-Za-z0-9+/]+={0,2}$/.exec(
    line.trim(),
  );
  return match ? match[0] : null;
}

export function fileSignCommand(challenge: string, keyPath: string): string {
  return `printf '%s' '${challenge}' | ssh-keygen -Y sign -f ${keyPath} -n nvm-login`;
}

// The socket export must be its own line: process substitution does not inherit an inline environment
// assignment.
export function agentSignCommand(challenge: string, agentSocket: string): string {
  return [
    `export SSH_AUTH_SOCK=${agentSocket}`,
    `printf '%s' '${challenge}' | ssh-keygen -Y sign -f <(ssh-add -L) -n nvm-login`,
  ].join("\n");
}

// Use the registered public key file with -f; ssh-add -L alone silently selects the first key.
export function agentPickSignCommand(challenge: string, publicKeyPath: string): string {
  return `printf '%s' '${challenge}' | ssh-keygen -Y sign -f ${publicKeyPath} -n nvm-login`;
}

export function registeredAgentCommand(
  challenge: string,
  canonicalPublicKey: string,
  agentSocket: string,
): string | null {
  if (!isSafeLiteral(agentSocket)) return null;
  const body = keySignCommand(challenge, canonicalPublicKey);
  if (body === null) return null;
  return `export SSH_AUTH_SOCK=${agentSocket}\n${body}`;
}

// Build a per-key command only from a canonical public key and values accepted by isSafeLiteral.
export function keySignCommand(
  challenge: string,
  canonicalPublicKey: string,
): string | null {
  const key = canonicalKeyFields(canonicalPublicKey);
  if (key === null) return null;
  if (!isSafeLiteral(key)) return null;
  if (!isSafeLiteral(challenge)) return null;
  return `printf '%s' '${challenge}' | ssh-keygen -Y sign -f <(printf '%s' '${key}') -n nvm-login`;
}

export const CHALLENGE_PLACEHOLDER = "<challenge>";

// Only the newest challenge request may update the page.
export class ChallengeSequencer {
  private seq = 0;

  next(): number {
    this.seq += 1;
    return this.seq;
  }

  isCurrent(n: number): boolean {
    return n === this.seq;
  }

  reset(): void {
    this.seq += 1;
  }
}

// Clipboard rejection falls back to execCommand. Plain HTTP may have no clipboard API; report success
// only after a real copy.
export async function copyText(value: string): Promise<boolean> {
  const navigatorWithClipboard = navigator as
    | { clipboard?: { writeText?: (text: string) => Promise<void> } }
    | undefined;
  const writeText = navigatorWithClipboard?.clipboard?.writeText;
  if (writeText) {
    try {
      await writeText.call(navigatorWithClipboard?.clipboard, value);
      return true;
    } catch {
      // A refused clipboard write must reach the fallback rather than report success.
    }
  }
  try {
    const area = document.createElement("textarea");
    area.value = value;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.append(area);
    area.select();
    const ok = document.execCommand("copy");
    area.remove();
    return ok;
  } catch {
    return false;
  }
}
