import { test } from "node:test";
import assert from "node:assert/strict";

import { installDom, domShim, FakeElement, FakeText } from "./dom-shim.js";

const PUBLIC_LINE =
  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti user@example.com";
const PUBLIC_LINE_CANONICAL =
  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti";
const PRIVATE_PASTE = `-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAAB
-----END OPENSSH PRIVATE KEY-----`;
const SIGNATURE = `-----BEGIN SSH SIGNATURE-----
U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAggGT2Lsf+qDXR2PKgZyg21E9JMm
-----END SSH SIGNATURE-----`;

function elements(node: FakeElement): FakeElement[] {
  return node.children.filter((n): n is FakeElement => n instanceof FakeElement);
}

function findByClass(node: FakeElement, className: string): FakeElement | undefined {
  const direct = elements(node).find((n) => n.className === className);
  if (direct) return direct;
  for (const child of elements(node)) {
    const found = findByClass(child, className);
    if (found) return found;
  }
  return undefined;
}

/** Every text node below `node`. */
function allText(node: FakeElement | FakeText): string {
  if (node instanceof FakeText) return node.textContent;
  return node.children.map(allText).join("");
}

/** Every element below `node`, in document order. */
function walk(node: FakeElement): FakeElement[] {
  const out: FakeElement[] = [];
  for (const child of node.children) {
    if (child instanceof FakeElement) {
      out.push(child, ...walk(child));
    }
  }
  return out;
}

test("private key armor is recognized in every common container form", async () => {
  const { looksLikePrivateKey } = await import("../src/ssh.js");
  for (const [text, expected] of [
    [PRIVATE_PASTE, true],
    ["-----BEGIN RSA PRIVATE KEY-----", true],
    ["-----BEGIN ENCRYPTED PRIVATE KEY-----", true],
    ["-----BEGIN PRIVATE KEY-----", true],
    [SIGNATURE, false],
    [PUBLIC_LINE, false],
  ] as const) assert.equal(looksLikePrivateKey(text), expected, text);
});

test("the public key check accepts one openssh line and refuses everything else", async () => {
  const { checkPublicKeyText } = await import("../src/ssh.js");
  const ecdsa = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBB81 1password";
  for (const [raw, expected] of [
    [PUBLIC_LINE, { ok: true, line: PUBLIC_LINE }],
    [`${PUBLIC_LINE}\r\n`, { ok: true, line: PUBLIC_LINE }],
    [ecdsa, { ok: true, line: ecdsa }],
    [PRIVATE_PASTE, { ok: false, reason: "private_key" }],
    ["not a key", { ok: false, reason: "bad_format" }],
    ["ssh-rsa AAAAB3NzaC1yc2E x", { ok: false, reason: "bad_format" }],
    [`${PUBLIC_LINE}\n${PUBLIC_LINE}`, { ok: false, reason: "bad_format" }],
  ] as const) assert.deepEqual(checkPublicKeyText(raw), expected, raw);
});

test("the signature check accepts the armored block and refuses private keys", async () => {
  const { checkSignatureText } = await import("../src/ssh.js");
  for (const [raw, expected] of [
    [`\n${SIGNATURE}\n`, { ok: true, value: SIGNATURE }],
    [PRIVATE_PASTE, { ok: false, reason: "private_key" }],
    ["x", { ok: false, reason: "bad_format" }],
    ["-----BEGIN SSH SIGNATURE-----\nonly half", { ok: false, reason: "bad_format" }],
  ] as const) assert.deepEqual(checkSignatureText(raw), expected, raw);
});

test("the command templates embed the challenge and the namespace", async () => {
  const ssh = await import("../src/ssh.js");
  const challenge = "a".repeat(64);
  const file = ssh.fileSignCommand(challenge, "~/.ssh/id_ed25519");
  assert.equal(
    file,
    "printf '%s' 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' | ssh-keygen -Y sign -f ~/.ssh/id_ed25519 -n nvm-login",
  );
  const agent = ssh.agentSignCommand(challenge, "~/.1password/agent.sock");
  assert.ok(agent.startsWith("export SSH_AUTH_SOCK=~/.1password/agent.sock\n"));
  assert.ok(agent.includes("| ssh-keygen -Y sign -f <(ssh-add -L) -n nvm-login"));
  assert.ok(agent.includes(challenge));
  const pick = ssh.agentPickSignCommand(challenge, "~/.ssh/key.pub");
  assert.ok(pick.endsWith("-f ~/.ssh/key.pub -n nvm-login"));
});

test("the per-key command embeds only validated canonical key bytes", async () => {
  const ssh = await import("../src/ssh.js");
  const challenge = "b".repeat(64);
  const command = ssh.keySignCommand(challenge, PUBLIC_LINE_CANONICAL);
  assert.equal(
    command,
    "printf '%s' '" +
      "b".repeat(64) +
      "' | ssh-keygen -Y sign -f <(printf '%s' '" +
      PUBLIC_LINE_CANONICAL +
      "') -n nvm-login",
  );
  for (const [candidateChallenge, key] of [
    [challenge, PUBLIC_LINE],
    [challenge, "ssh-ed25519 AAAA$x"],
    ["c'$(){}", PUBLIC_LINE_CANONICAL],
  ]) assert.equal(ssh.keySignCommand(candidateChallenge, key), null);
  const agent = ssh.registeredAgentCommand(
    challenge,
    PUBLIC_LINE_CANONICAL,
    "~/.1password/agent.sock",
  );
  assert.equal(
    agent,
    "export SSH_AUTH_SOCK=~/.1password/agent.sock\n" + command,
  );
  assert.equal(ssh.registeredAgentCommand(challenge, PUBLIC_LINE_CANONICAL, "sock\nname"), null);
  assert.equal(ssh.registeredAgentCommand(challenge, PUBLIC_LINE, "~/.1password/agent.sock"), null);
  // A malicious comment can never influence the command: only the algorithm and base64 fields exist in
  // the stored line.
  const malicious = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA ' ; rm -rf /; echo '";
  assert.equal(ssh.canonicalKeyFields(malicious), null);
  assert.equal(ssh.keySignCommand(challenge, malicious), null);
});

test("the sequencer drops stale challenge responses", async () => {
  const ssh = await import("../src/ssh.js");
  const seq = new ssh.ChallengeSequencer();
  const first = seq.next();
  const second = seq.next();
  assert.equal(seq.isCurrent(first), false);
  assert.equal(seq.isCurrent(second), true);
  seq.reset();
  assert.equal(seq.isCurrent(second), false);
});

test("copyText reports the real outcome and falls back after a refusal", async () => {
  installDom();
  const ssh = await import("../src/ssh.js");
  // The minimal clipboard/textarea surface copyText touches; the shim's document has neither a body
  // nor execCommand.
  const created: { value: string; removed: boolean }[] = [];
  const doc = domShim() as unknown as {
    createElement: (tag: string) => unknown;
    body: unknown;
    execCommand: (cmd: string) => boolean;
  };
  (domShim() as unknown as { body: unknown }).body = { append: () => {} };
  (domShim() as unknown as { execCommand: unknown }).execCommand = () => true;
  const realCreate = doc.createElement.bind(domShim());
  (domShim() as unknown as { createElement: unknown }).createElement = (tag: string) => {
    const node = realCreate(tag) as unknown as {
      value: string;
      select: () => void;
      remove: () => void;
      style: Record<string, string>;
      setAttribute: (k: string, v: string) => void;
    };
    node.style = {};
    node.select = () => {};
    node.remove = () => {
      created[0].removed = true;
    };
    Object.defineProperty(node, "value", {
      get: () => created[0]?.value ?? "",
      set: (v: string) => {
        created[0] = { value: v, removed: false };
      },
    });
    return node;
  };
  const setNavigator = (value: unknown): void => {
    Object.defineProperty(globalThis, "navigator", {
      value,
      configurable: true,
    });
  };

  // No clipboard API at all (the plain-HTTP access path): the execCommand fallback decides.
  setNavigator({});
  assert.equal(await ssh.copyText("fallback bytes"), true);
  assert.equal(created[0]?.value, "fallback bytes");
  assert.equal(created[0]?.removed, true, "the helper textarea is cleaned up");

  // A clipboard that refuses is a failure, not a silent success: the fallback runs and its outcome is reported.
  let execResult = false;
  (domShim() as unknown as { execCommand: unknown }).execCommand = () => execResult;
  setNavigator({
    clipboard: {
      writeText: () => Promise.reject(new Error("denied")),
    },
  });
  assert.equal(
    await ssh.copyText("x"),
    false,
    "a refused clipboard with a dead fallback reports failure",
  );
  execResult = true;
  assert.equal(
    await ssh.copyText("x"),
    true,
    "a refused clipboard with a working fallback reports success",
  );

  // A working clipboard reports success and never touches the fallback.
  let fallbackRan = false;
  (domShim() as unknown as { execCommand: unknown }).execCommand = () => {
    fallbackRan = true;
    return true;
  };
  setNavigator({
    clipboard: {
      writeText: (t: string) => {
        assert.equal(t, "direct");
        return Promise.resolve();
      },
    },
  });
  assert.equal(await ssh.copyText("direct"), true);
  assert.equal(fallbackRan, false);
  Object.defineProperty(globalThis, "navigator", {
    value: undefined,
    configurable: true,
  });
});

test("the sign-in page renders the SSH block only when ssh_enabled is true", async () => {
  installDom();
  const { renderLogin } = await import("../src/views/login.js");
  const base = {
    logged_in: false,
    setup: { problems: [] },
    credentials: "ok",
    show_reset_help: false,
  };
  const container = domShim().createElement("main") as unknown as HTMLElement;
  renderLogin(
    { lang: "zh", logoutDone: false, login: { ...base }, addCleanup: () => {} } as never,
    container,
  );
  const without = container as unknown as FakeElement;
  assert.equal(findByClass(without, "login-ssh"), undefined, "no block without the flag");
  assert.notEqual(findByClass(without, "login-form"), undefined);

  installDom();
  const containerSsh = domShim().createElement("main") as unknown as HTMLElement;
  renderLogin(
    {
      lang: "zh",
      logoutDone: false,
      login: { ...base, ssh_enabled: true },
      addCleanup: () => {},
    } as never,
    containerSsh,
  );
  const withSsh = containerSsh as unknown as FakeElement;
  const block = findByClass(withSsh, "login-ssh");
  assert.ok(block, "the SSH group is rendered");
  assert.equal(block.tagName, "DETAILS");
  assert.equal(block.getAttribute("open"), null, "the group starts collapsed");
  const summary = elements(block).find((n) => n.tagName === "SUMMARY");
  assert.match(summary?.textContent ?? "", /使用 SSH 密钥登录/);
  // The three command templates and the namespace exist even before the first challenge arrives
  // (placeholder bytes).
  const codes = walk(block).filter((n) => n.className === "ssh-command-code");
  assert.equal(codes.length, 3);
  for (const code of codes) {
    assert.match(code.textContent, /-n nvm-login/);
    assert.match(code.textContent, /ssh-keygen -Y sign/);
    assert.ok(code.textContent.includes("<challenge>"));
  }

  const challengeLine = findByClass(block, "ssh-challenge-line");
  assert.equal(challengeLine?.getAttribute("aria-live"), "polite");
  // The challenge itself carries its label and a copy button, and the button stays disabled until a
  // real challenge arrived (only the current real challenge is copyable).
  const challengeLabel = findByClass(block, "ssh-challenge-label");
  assert.equal(challengeLabel?.textContent, "挑战");
  const challengeCopy = walk(block).find((n) => n.id === "ssh-challenge-copy");
  assert.ok(challengeCopy, "the challenge copy button exists");
  assert.equal(
    (challengeCopy as unknown as { disabled: boolean }).disabled,
    true,
    "no placeholder bytes are copyable",
  );
  const challengeActions = findByClass(block, "ssh-challenge-actions");
  assert.ok(challengeActions, "the copy and refresh buttons share one row");
  assert.equal(
    walk(challengeActions).some((n) => n.id === "ssh-refresh"),
    true,
    "the refresh button is the same existing control",
  );
  const signature = walk(block).find((n) => n.id === "ssh-signature");
  assert.ok(signature, "the signature box exists");
  assert.equal(signature.tagName, "TEXTAREA");

  installDom();
  const containerEn = domShim().createElement("main") as unknown as HTMLElement;
  renderLogin(
    {
      lang: "en",
      logoutDone: false,
      login: { ...base, ssh_enabled: true },
      addCleanup: () => {},
    } as never,
    containerEn,
  );
  const enBlock = findByClass(containerEn as unknown as FakeElement, "login-ssh");
  assert.match(allText(enBlock ?? domShim().createElement("div")), /SSH key/);
});

test("the account page renders the key section immediately after setup", async () => {
  installDom();
  (globalThis as unknown as { fetch: unknown }).fetch = async () =>
    new Promise<Response>(() => {});
  (globalThis as unknown as { window: unknown }).window = {
    setTimeout: (): number => 1,
  };
  const { renderPassword } = await import("../src/views/password.js");
  const voluntary = domShim().createElement("main") as unknown as HTMLElement;
  renderPassword(
    { lang: "zh", addCleanup: () => {} } as never,
    voluntary,
  );
  const section = findByClass(voluntary as unknown as FakeElement, "ssh-keys");
  assert.ok(section, "the voluntary account view carries the key section");
  const addForm = findByClass(section, "ssh-keys-add");
  assert.ok(addForm, "the registration form exists");
  assert.ok(walk(addForm).some((n) => n.id === "ssh-add-key"));
  assert.ok(walk(addForm).some((n) => n.id === "ssh-add-comment"));
});

test("the key list renders rows with a per-key command and no raw comment in commands", async () => {
  installDom();
  (globalThis as unknown as { window: unknown }).window = {
    setTimeout: (): number => 1,
  };
  // Feed the list through the real fetch path: one item with a hostile comment; the sign command must
  // contain only the canonical key bytes.
  (globalThis as unknown as { fetch: unknown }).fetch = async () =>
    new Response(
      JSON.stringify({
        items: [
          {
            id: "0123456789abcdef",
            algorithm: "ssh-ed25519",
            fingerprint: "SHA256:abcdef",
            comment: "' ; rm -rf /; echo '",
            public_key: PUBLIC_LINE_CANONICAL,
            added_at: 1760000000000,
          },
        ],
      }),
      { status: 200 },
    );
  const noted: boolean[] = [];
  const { renderPassword } = await import("../src/views/password.js");
  const container = domShim().createElement("main") as unknown as HTMLElement;
  renderPassword(
    {
      lang: "zh",
      addCleanup: () => {},
      syncSshEnabled: (present: boolean) => noted.push(present),
    } as never,
    container,
  );
  // The fetch resolves in a microtask; let it run before asserting.
  await new Promise((resolve) => setTimeout(resolve, 0));
  const section = findByClass(container as unknown as FakeElement, "ssh-keys");
  assert.ok(section);
  const row = findByClass(section, "ssh-key-row");
  assert.ok(row, "the registered key renders as a row");
  const code = walk(row).find((n) => n.className === "ssh-command-code");
  assert.ok(code, "the per-key command exists");
  assert.equal(
    code.textContent,
    "export SSH_AUTH_SOCK=~/.1password/agent.sock\nprintf '%s' '<challenge>' | ssh-keygen -Y sign -f <(printf '%s' '" +
      PUBLIC_LINE_CANONICAL +
      "') -n nvm-login",
  );
  assert.equal(code.textContent.includes("rm -rf"), false);
  // The hostile comment renders as plain text, never inside the command.
  const comment = findByClass(row, "ssh-key-comment");
  assert.equal(comment?.textContent, "' ; rm -rf /; echo '");
  assert.deepEqual(noted, [true]);
});

test("a successful empty key list clears the sign-in flag, and a late list does not", async () => {
  installDom();
  (globalThis as unknown as { window: unknown }).window = {
    setTimeout: (): number => 1,
  };
  const noted: boolean[] = [];
  const app = {
    lang: "zh",
    addCleanup: () => {},
    syncSshEnabled: (present: boolean) => noted.push(present),
    sessionEnded: () => {
      throw new Error("a key-list read must not end the session");
    },
  };
  (globalThis as unknown as { fetch: unknown }).fetch = async () =>
    new Response(JSON.stringify({ items: [] }), { status: 200 });
  const { renderPassword } = await import("../src/views/password.js");
  const empty = domShim().createElement("main") as unknown as HTMLElement;
  renderPassword(app as never, empty);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(noted, [false]);
  assert.ok(findByClass(empty as unknown as FakeElement, "ssh-keys-empty"));

  let release: (response: Response) => void = () => {};
  let started: () => void = () => {};
  const fetchStarted = new Promise<void>((resolve) => {
    started = resolve;
  });
  (globalThis as unknown as { fetch: unknown }).fetch = () => {
    started();
    return new Promise<Response>((resolve) => {
      release = resolve;
    });
  };
  const late = domShim().createElement("main") as unknown as HTMLElement;
  renderPassword(app as never, late);
  await fetchStarted;
  const section = findByClass(late as unknown as FakeElement, "ssh-keys");
  assert.ok(section);
  let connected = true;
  Object.defineProperty(section, "isConnected", { get: () => connected });
  connected = false;
  release(new Response(JSON.stringify({ items: [] }), { status: 200 }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(noted, [false]);
});

test("the ssh error keys render notices in both languages", async () => {
  installDom();
  const { renderNotice } = await import("../src/dom.js");
  for (const key of [
    "ssh.challenge_invalid",
    "ssh.login_failed",
    "ssh.key_not_registered",
    "ssh.challenge_busy",
    "ssh.private_refused",
    "ssh.signature_bad_format",
    "ssh.public_key_bad_format",
    "ssh.keys_unsaved",
    "ssh.key_limit",
    "ssh.key_not_found",
  ]) {
    for (const lang of ["zh", "en"] as const) {
      const notice = renderNotice(key, {}, lang);
      const text = (notice as unknown as FakeElement).textContent;
      assert.notEqual(text, key, `${key}/${lang} must have catalog text`);
      assert.ok((text ?? "").length > 4, `${key}/${lang} renders real text`);
    }
  }

  const invalid = renderNotice(
    "ssh.key_invalid",
    { reason: { key: "ssh.key_invalid.unsupported_type", params: { algorithm: "ssh-rsa" } } },
    "zh",
  );
  assert.match((invalid as unknown as FakeElement).textContent ?? "", /ssh-rsa/);
  const unsaved = renderNotice(
    "ssh.keys_unsaved",
    { write_reason: { key: "credentials.unsaved_reason.file", params: {} } },
    "zh",
  );
  assert.match((unsaved as unknown as FakeElement).textContent ?? "", /文件未改动/);
});
