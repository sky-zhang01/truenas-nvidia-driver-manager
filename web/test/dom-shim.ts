// node --test has no DOM; this shim covers the rendering helpers without requiring a browser.
export class FakeText {
  readonly nodeType = 3;
  constructor(private value: string) {}
  get textContent(): string {
    return this.value;
  }
  set textContent(value: string) {
    this.value = value;
  }
}

export class FakeElement {
  readonly nodeType = 1;
  readonly tagName: string;
  /** The shim has no detach; a built element models a live page. */
  readonly isConnected = true;
  className = "";
  id = "";
  classList = {
    add: (name: string): void => {
      this.className = `${this.className} ${name}`.trim();
    },
  };
  private attrs = new Map<string, string>();
  private nodes: (FakeElement | FakeText)[] = [];
  parent: FakeElement | null = null;

  constructor(tagName: string) {
    this.tagName = tagName.toUpperCase();
  }

  get children(): (FakeElement | FakeText)[] {
    return this.nodes;
  }

  get firstChild(): FakeElement | FakeText | null {
    return this.nodes[0] ?? null;
  }

  get parentElement(): FakeElement | null {
    return this.parent;
  }

  get nextSibling(): FakeElement | FakeText | null {
    if (!this.parent) return null;
    const at = this.parent.nodes.indexOf(this);
    if (at < 0) return null;
    return this.parent.nodes[at + 1] ?? null;
  }

  get textContent(): string {
    return this.nodes.map((n) => n.textContent).join("");
  }

  set textContent(value: string) {
    this.nodes = value === "" ? [] : [new FakeText(value)];
  }

  append(...added: (FakeElement | FakeText | string)[]): void {
    for (const node of added) {
      const child = typeof node === "string" ? new FakeText(node) : node;
      if (child instanceof FakeElement) {
        child.detach();
        child.parent = this;
      }
      this.nodes.push(child);
    }
  }

  insertBefore(node: FakeElement, before: FakeElement | FakeText | null): void {
    node.detach();
    node.parent = this;
    if (before === null) {
      this.nodes.push(node);
      return;
    }
    const at = this.nodes.indexOf(before);
    if (at < 0) this.nodes.push(node);
    else this.nodes.splice(at, 0, node);
  }

  remove(): void {
    this.detach();
  }

  private detach(): void {
    if (!this.parent) return;
    const at = this.parent.nodes.indexOf(this);
    if (at >= 0) this.parent.nodes.splice(at, 1);
    this.parent = null;
  }

  appendChild(node: FakeElement | FakeText): FakeElement | FakeText {
    this.append(node);
    return node;
  }

  removeChild(node: FakeElement | FakeText): void {
    const at = this.nodes.indexOf(node);
    if (at >= 0) this.nodes.splice(at, 1);
    if (node instanceof FakeElement && node.parent === this) node.parent = null;
  }

  setAttribute(name: string, value: string): void {
    this.attrs.set(name, String(value));
  }

  getAttribute(name: string): string | null {
    return this.attrs.get(name) ?? null;
  }

  removeAttribute(name: string): void {
    this.attrs.delete(name);
  }

  addEventListener(): void {
    /* the unit tests never dispatch events */
  }

  querySelector(): null {
    return null;
  }

  focus(): void {
    /* no focus in the shim */
  }
}

export class FakeDocument {
  createElement(tagName: string): FakeElement {
    return new FakeElement(tagName);
  }
  createTextNode(text: string): FakeText {
    return new FakeText(text);
  }
  createDocumentFragment(): FakeElement {
    return new FakeElement("#fragment");
  }
  querySelectorAll(): FakeElement[] {
    return [];
  }
  getElementById(): null {
    return null;
  }
  /** The page visibility `poll.ts` reads; `setHidden` changes it. */
  hidden = false;
  private listeners = new Map<string, (() => void)[]>();
  addEventListener(type: string, listener: () => void): void {
    const list = this.listeners.get(type) ?? [];
    list.push(listener);
    this.listeners.set(type, list);
  }
  removeEventListener(type: string, listener: () => void): void {
    const list = this.listeners.get(type) ?? [];
    this.listeners.set(
      type,
      list.filter((l) => l !== listener),
    );
  }
  /** Move the page to the background or back, notifying the listeners. */
  setHidden(value: boolean): void {
    this.hidden = value;
    for (const listener of this.listeners.get("visibilitychange") ?? []) listener();
  }
}

let installed: FakeDocument | null = null;

/** Install the shim as the global `document`. Call before using `dom.ts`. */
export function installDom(): void {
  installed = new FakeDocument();
  (globalThis as unknown as { document: unknown }).document = installed;
}

/** The installed shim, for tests that need an element the real types reject. */
export function domShim(): FakeDocument {
  if (!installed) throw new Error("installDom() must run first");
  return installed;
}

/** The role of every alert/status region inside `element`, for placement tests. */
export function liveRegions(element: FakeElement): string[] {
  const out: string[] = [];
  const walk = (node: FakeElement | FakeText): void => {
    if (node instanceof FakeText) return;
    const role = node.getAttribute("role");
    if (role === "alert" || role === "status") out.push(node.textContent);
    for (const child of node.children) walk(child);
  };
  walk(element);
  return out;
}
