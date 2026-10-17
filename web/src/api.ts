// Uploads use XMLHttpRequest because it reports upload progress. API failures preserve error, params
// and optional raw output.

export interface ReasonField {
  key: string;
  params?: Record<string, unknown>;
}

export type Modules = "closed_only" | "open_only" | "either";

export type Reading<T> =
  | { value: T }
  | { not_loaded: true }
  | { unknown: ReasonField };

export interface UpdateBlocked {
  key: string;
  reason?: ReasonField;
}

export interface SetupProblem {
  key: string;
  params?: Record<string, unknown>;
  raw?: string;
}

export type CredentialsState =
  | "ok"
  | "username_invalid"
  | "initial_password_unusable"
  | { unreadable: string }
  | { unsaved: string };

export interface LoginInfo {
  logged_in: boolean;
  must_change_password?: boolean;
  update_blocked?: UpdateBlocked[];
  setup: { problems: SetupProblem[] };
  credentials: CredentialsState;
  show_reset_help: boolean;
  // Show SSH sign-in only when the server explicitly reports ssh_enabled === true.
  ssh_enabled?: boolean;
}

export interface GpuReading {
  address: string;
  device: string;
  name?: string;
  modules: Modules;
  last_branch?: number;
  first_branch?: number;
  too_new: string[];
  too_old: string[];
}

export interface Unreadable {
  object: string;
  reason: ReasonField;
  raw?: string;
}

export interface VersionsResponse {
  items: { version: string }[];
  open_unbuildable: string[];
  unreadable: Unreadable[];
  gpus: GpuReading[];
}

export interface HomeLink {
  exists: boolean;
  target?: string;
  points_to_canonical?: boolean;
  unknown?: ReasonField;
}

export interface OriginalSaved {
  saved: boolean;
  version?: string;
  saved_at?: number;
  unknown?: ReasonField;
}

export interface ActiveTask {
  task_id: string;
  status: string;
  state_read_error?: string;
}

export interface HomeGpuDevice {
  address: string;
  device: string;
  passthrough: boolean;
  name?: string;
}

export type HomeGpus =
  | { value: { devices: HomeGpuDevice[] }; unknown?: undefined }
  | { unknown: ReasonField; value?: undefined };

export interface HomeTaskRecord {
  task_id: string;
  status: string;
  phase: string | null;
  created_at: number | null;
  finished_at: number | null;
  interrupt_discovered_at: number | null;
  source: PlanDialogSource | null;
  version: string | null;
}

export interface HomeResponse {
  loaded_version: Reading<string>;
  canonical: { version: Reading<string>; sha256: Reading<string> };
  truenas_version: Reading<string>;
  kernel: Reading<string>;
  link: HomeLink;
  original_saved: OriginalSaved;
  gpus: HomeGpus;
  latest_task: HomeTaskRecord | { unknown: ReasonField } | null;
  interrupted: HomeTaskRecord | null;
  active_task: ActiveTask | { unknown: ReasonField } | null;
  update_blocked: UpdateBlocked[];
}

export interface GroupProblem {
  key: string;
  params?: Record<string, unknown>;
}

export interface GroupSummary {
  group_id: string;
  run: { sha256: string; fingerprint: string; candidates: string[] } | null;
  debs: Record<string, { version: string }>;
  missing: string[];
  toolkit_version: string | null;
  below_security_floor: boolean;
  problems: GroupProblem[];
}

export interface UploadFileInfo {
  kind: "run" | "deb";
  client_name: string;
  sha256: string;
  size: number;
  package?: string;
  version?: string;
  candidates?: string[];
}

export interface UploadFileResponse {
  file: UploadFileInfo;
  group: GroupSummary;
}

export type PlanSource =
  | { kind: "official"; version: string; gpus: string[] }
  | { kind: "upload"; group_id: string; version: string; gpus: string[] }
  | { kind: "local"; row_id: string };

export interface PlanDialogSource {
  kind: "official" | "upload" | "local";
  fingerprint?: string;
  sha256?: string;
  row_kind?: string;
}

export interface PlanDialog {
  source: PlanDialogSource;
  version: string;
  kernel: string;
  truenas_version: string;
  original: { saved: boolean; version?: string; saved_at?: number } | { unknown: ReasonField };
  save_available: boolean;
  confirmation: {
    original: "saved" | "save_available" | "unavailable";
    buttons: { button: "save" | "skip" | "start"; enabled: boolean }[];
    how: ReasonField;
    warnings: ReasonField[];
  };
}

export interface PrecheckFailure {
  item: string;
  object?: string;
  reason?: ReasonField;
  target?: string;
  row_kind?: string;
  target_kernel?: string;
  current_kernel?: string;
}

export interface GpuBlock {
  state: "busy" | "unreadable";

  read_reason?: ReasonField;
  users: ObjectEntry[];
  errors: CategoryError[];
}

export interface ObjectEntry {
  name: string;
  type: "docker" | "incus";
}

export interface CategoryError {
  type: "docker" | "incus";
  reason: ReasonField;
  raw?: string;
}

export interface UncomputableItem {
  object: string;
  reason: ReasonField;
}

export interface PlanReady {
  result: "ready";
  plan_id: string;
  dialog: PlanDialog;
}

export interface PlanBlocked {
  result: "blocked";
  failures: PrecheckFailure[];
  gpu: GpuBlock | null;
}

export interface PlanUncomputable {
  result: "uncomputable";
  items: UncomputableItem[];
}

export type PlanResponse = PlanReady | PlanBlocked | PlanUncomputable;

export interface DriverRow {
  id: string;
  kind: "official" | "upload" | "original" | null;
  version: string | null;
  fingerprint: string | null;
  created_at: number | null;
  size: number | null;
  target_kernel: string | null;
  status: "in_use" | "available" | "kernel_mismatch" | "UNKNOWN";
  keep: boolean | null;
  can_update: boolean;
  delete_blocked: "in_use" | "kept" | "unknown" | null;
}

export interface DeleteUnavailable {
  key: string;
  reason?: ReasonField;
}

export interface DriversResponse {
  rows: DriverRow[];
  kernel: string;
  delete_unavailable: DeleteUnavailable | null;
}

export interface TaskListItem {
  task_id: string;
  status: string;
  phase: string | null;
  source: PlanDialogSource | null;
  version: string | null;
  created_at: number | null;
  finished_at: number | null;
  interrupt_discovered_at: number | null;
  state_read_error?: string;
}

export interface UnitQuery {
  reason: ReasonField;
}

export interface TasksResponse {
  tasks: TaskListItem[];
  active_task: ActiveTask | { unknown: ReasonField } | null;
  unit_query: UnitQuery | null;
  state_dir: UnitQuery | null;
}

export interface LogLine {
  t: number;
  key: string;
  p: Record<string, unknown>;
}

export interface LogTail {
  lines: LogLine[];
  next: number;
  reset: boolean;
}

export interface Notice {
  key: string;
  variant?: string;
  params?: Record<string, unknown>;
}

export interface TaskError {
  code: string;
  params: Record<string, unknown>;
  command: string[] | null;
  exit: unknown;
  raw: string | null;
}

export interface TaskDetail {
  recovery_material_kept: boolean;
  released_by?: string;
  released_at?: number;
  task_id: string;
  status: string;
  state_read_error?: string | null;
  state_note: { state_read_error: string; kept_as: string | null } | null;
  phase: string | null;
  created_at: number | null;
  finished_at: number | null;
  interrupt_discovered_at: number | null;
  plan: Record<string, unknown> | null;
  loaded_version_start: Reading<string> | null;
  loaded_version_end: Reading<string> | null;
  download: { done_bytes?: number; total_bytes?: number } | null;
  replace: ReplaceRecord[];
  checks: CheckRecord[];
  restore: RestoreRecord | null;
  error: TaskError | null;
  gpu_users: { users: ObjectEntry[]; errors: CategoryError[] } | null;
  version_change: boolean;
  notice: Notice | null;
  log: LogTail;
  unit_query: UnitQuery | null;
}

export interface ReplaceRecord {
  pass: "update" | "restore";
  unloaded: string[];
  reloaded: {
    module: string;
    ok: boolean;
    reason?: ReasonField | { stderr: string } | null;
  }[];
}

export interface CheckRecord {
  pass: "update" | "restore";
  since_before: Reading<number>;
  since_after: Reading<number>;
  refresh_ok: boolean;
  nvidia_smi: Reading<string>;
  proc_version: Reading<string>;
  expected: string;
  version_ok: boolean;
  passed: boolean;
}

export interface RestoreRecord {
  decision: "restore" | "none";
  reason: string | null;
  read_reason: string | null;
  original: { row_id: string; version: string; saved_at: number } | null;
}

export class ApiError extends Error {
  readonly status: number;
  readonly key: string;
  readonly params: Record<string, unknown>;
  readonly raw: string | undefined;
  readonly body: unknown;

  constructor(
    status: number,
    key: string,
    params: Record<string, unknown>,
    raw: string | undefined,
    body: unknown,
  ) {
    super(`${status} ${key}`);
    this.name = "ApiError";
    this.status = status;
    this.key = key;
    this.params = params;
    this.raw = raw;
    this.body = body;
  }

  get isSessionEnded(): boolean {
    return this.status === 401 && this.key === "auth.required";
  }
}

function parseError(status: number, text: string): ApiError {
  let body: unknown = null;
  try {
    body = JSON.parse(text);
  } catch {
    body = text;
  }
  const obj = (body ?? {}) as { error?: unknown; params?: unknown; raw?: unknown };
  const key = typeof obj.error === "string" ? obj.error : "internal";
  const params =
    typeof obj.params === "object" && obj.params !== null
      ? (obj.params as Record<string, unknown>)
      : {};
  const raw = typeof obj.raw === "string" ? obj.raw : undefined;
  return new ApiError(status, key, params, raw, body);
}

const TOKEN_STORAGE_KEY = "nvm.token";

function readToken(): string | null {
  try {
    return sessionStorage.getItem(TOKEN_STORAGE_KEY);
  } catch {
    return null;
  }
}

function rememberToken(token: unknown): void {
  if (typeof token !== "string" || token.length === 0) return;
  try {
    sessionStorage.setItem(TOKEN_STORAGE_KEY, token);
  } catch {
    // A failed local token write leaves the cookie; the next request must confirm the session.
  }
}

export function clearToken(): void {
  try {
    sessionStorage.removeItem(TOKEN_STORAGE_KEY);
  } catch {
    // Storage may be unavailable; there is no further local state to clear.
  }
}

function withToken(path: string, init?: RequestInit): RequestInit {
  const headers = new Headers(init?.headers);
  if (path.startsWith("/api") && !headers.has("X-NVM-Token")) {
    const token = readToken();
    if (token) headers.set("X-NVM-Token", token);
  }
  return { credentials: "same-origin", ...init, headers };
}

async function request(path: string, init?: RequestInit): Promise<Response> {
  const res = await fetch(path, withToken(path, init));
  if (!res.ok) {
    throw parseError(res.status, await res.text());
  }
  return res;
}

export async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await request(path, init);
  return (await res.json()) as T;
}

function jsonInit(method: string, body: unknown): RequestInit {
  return {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  };
}

export function getLogin(): Promise<LoginInfo> {
  return apiFetch<LoginInfo>("/api/login");
}

export async function postLogin(
  username: string,
  password: string,
): Promise<{ must_change_password: boolean }> {
  const next = await apiFetch<{ must_change_password: boolean; token?: string }>(
    "/api/login",
    jsonInit("POST", { username, password }),
  );
  rememberToken(next.token);
  return next;
}

export async function postPassword(newPassword: string): Promise<{ token?: string }> {
  const next = await apiFetch<{ token?: string }>(
    "/api/password",
    jsonInit("POST", { new_password: newPassword }),
  );
  rememberToken(next.token);
  return next;
}

export function postLogout(): Promise<Record<string, never>> {
  return apiFetch<Record<string, never>>("/api/logout", { method: "POST" });
}

export interface SshChallenge {
  challenge_id: string;
  challenge: string;
  namespace: string;
  expires_in_s: number;
}

export interface SshKeyItem {
  id: string;
  algorithm: string;
  fingerprint: string;
  comment: string;
  public_key: string;
  added_at: number;
}

export function requestSshChallenge(): Promise<SshChallenge> {
  return apiFetch<SshChallenge>("/api/ssh/challenge", jsonInit("POST", {}));
}

export async function postSshLogin(
  challengeId: string,
  signature: string,
): Promise<{ must_change_password: boolean }> {
  const next = await apiFetch<{ must_change_password: boolean; token?: string }>(
    "/api/ssh/login",
    jsonInit("POST", { challenge_id: challengeId, signature }),
  );
  rememberToken(next.token);
  return next;
}

export function getSshKeys(): Promise<{ items: SshKeyItem[] }> {
  return apiFetch<{ items: SshKeyItem[] }>("/api/ssh/keys");
}

export function postSshKey(publicKey: string, comment: string): Promise<SshKeyItem> {
  return apiFetch<SshKeyItem>(
    "/api/ssh/keys",
    jsonInit("POST", { public_key: publicKey, comment }),
  );
}

export async function deleteSshKey(id: string): Promise<{ token?: string }> {
  const next = await apiFetch<{ token?: string }>(
    `/api/ssh/keys/${encodeURIComponent(id)}`,
    { method: "DELETE" },
  );
  rememberToken(next.token);
  return next;
}

export function getHome(): Promise<HomeResponse> {
  return apiFetch<HomeResponse>("/api/home");
}

export function getVersions(): Promise<VersionsResponse> {
  return apiFetch<VersionsResponse>("/api/versions");
}

export function openUploadGroup(): Promise<{ group_id: string }> {
  return apiFetch<{ group_id: string }>("/api/uploads", { method: "POST" });
}

export function getCurrentUpload(): Promise<GroupSummary | null> {
  return apiFetch<GroupSummary | null>("/api/uploads/current");
}

export function putUploadFile(
  groupId: string,
  clientName: string,
  file: File,
  onProgress?: (sent: number, total: number) => void,
  signal?: AbortSignal,
): Promise<UploadFileResponse> {
  return new Promise<UploadFileResponse>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const path = `/api/uploads/${encodeURIComponent(groupId)}/files?name=${encodeURIComponent(
      clientName,
    )}`;
    xhr.open("PUT", path);
    xhr.setRequestHeader("Content-Type", "application/octet-stream");
    const token = readToken();
    if (token) xhr.setRequestHeader("X-NVM-Token", token);
    xhr.upload.addEventListener("progress", (ev) => {
      if (onProgress && ev.lengthComputable) onProgress(ev.loaded, ev.total);
    });
    xhr.addEventListener("error", () => {
      reject(new ApiError(0, "upload.interrupted", { file_name: clientName }, undefined, null));
    });
    xhr.addEventListener("abort", () => {
      reject(new ApiError(0, "upload.interrupted", { file_name: clientName }, undefined, null));
    });
    xhr.addEventListener("load", () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        try {
          resolve(JSON.parse(xhr.responseText) as UploadFileResponse);
        } catch {
          reject(parseError(xhr.status, xhr.responseText));
        }
      } else {
        reject(parseError(xhr.status, xhr.responseText));
      }
    });
    // Abort rejects with the same upload.interrupted shape as a transport failure.
    signal?.addEventListener("abort", () => xhr.abort(), { once: true });
    xhr.send(file);
    // An already-aborted signal must prevent the upload too.
    if (signal?.aborted) xhr.abort();
  });
}

export function postPlan(source: PlanSource): Promise<PlanResponse> {
  return apiFetch<PlanResponse>("/api/plans", jsonInit("POST", { source }));
}

export function postTask(
  planId: string,
  button: "save" | "skip" | "start",
): Promise<{ task_id: string }> {
  return apiFetch<{ task_id: string }>("/api/tasks", jsonInit("POST", { plan_id: planId, button }));
}

export function getTasks(): Promise<TasksResponse> {
  return apiFetch<TasksResponse>("/api/tasks");
}

export function getTask(id: string, after?: number): Promise<TaskDetail> {
  const query = after === undefined ? "" : `?after=${after}`;
  return apiFetch<TaskDetail>(`/api/tasks/${encodeURIComponent(id)}${query}`);
}

export function releaseTask(id: string): Promise<TaskDetail> {
  return apiFetch<TaskDetail>(`/api/tasks/${encodeURIComponent(id)}/release`, { method: "POST" });
}

export function getDrivers(): Promise<DriversResponse> {
  return apiFetch<DriversResponse>("/api/drivers");
}

export function deleteDrivers(ids: string[]): Promise<{ deleted: string[] }> {
  return apiFetch<{ deleted: string[] }>("/api/drivers/delete", jsonInit("POST", { ids }));
}

export function setKeep(id: string, keep: boolean): Promise<DriverRow> {
  return apiFetch<DriverRow>(
    `/api/drivers/${encodeURIComponent(id)}/keep`,
    jsonInit("PUT", { keep }),
  );
}

// Only an explicit unknown reading matches; callers decide how to report absent or malformed values.
export function isUnknown(r: unknown): r is { unknown: ReasonField } {
  return (
    r !== null &&
    typeof r === "object" &&
    (r as { unknown?: unknown }).unknown !== undefined
  );
}
