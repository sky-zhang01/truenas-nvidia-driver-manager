#!/usr/bin/env bash
# Export an explicitly classified local Git commit without carrying its history.
set -Eeuo pipefail
exec node - "$@" <<'NODE'
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');

function refuse(message) { throw new Error(message); }
function git(args) {
  try {
    return execFileSync('git', args, {
      env: { ...process.env, GIT_NO_REPLACE_OBJECTS: '1' },
      maxBuffer: 128 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'],
    });
  } catch { refuse('local Git input could not be read'); }
}
function inside(parent, child) {
  const relative = path.relative(parent, child);
  return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
}
function destination(value) {
  const resolved = path.resolve(value);
  if (fs.existsSync(resolved) && fs.lstatSync(resolved).isSymbolicLink()) refuse('destination cannot be a symbolic link');
  const parent = fs.realpathSync(path.dirname(resolved));
  return path.join(parent, path.basename(resolved));
}
function safePath(value, directory = false) {
  const leaf = directory && value.endsWith('/') ? value.slice(0, -1) : value;
  if (!leaf || /[\x00-\x20\x7f\\:*?\[\]{}]/.test(leaf) || leaf.startsWith('/') ||
      leaf.split('/').some(part => !part || part === '.' || part === '..' || part.toLowerCase() === '.git')) {
    refuse('invalid path or glob in export input');
  }
  return value;
}
function matches(spec, leaf) { return spec.endsWith('/') ? leaf.startsWith(spec) : leaf === spec; }

let stage;
try {
  const args = process.argv.slice(2);
  if (args.length === 1 && args[0] === '--help') {
    console.log('Usage: export-public.sh --source COMMIT --output DIR --manifest FILE [--rules FILE] [--worktree] [--allow-unmatched-rule PATH ...]');
    console.log('Default rules are read from the commit. Worktree mode includes nonignored untracked files and is only for local rehearsal.');
    process.exit(0);
  }
  const options = new Map();
  const exceptions = new Set();
  for (let index = 0; index < args.length; index++) {
    const key = args[index];
    if (key === '--worktree') {
      if (options.has(key)) refuse('duplicate option');
      options.set(key, true);
    } else if (['--source', '--output', '--manifest', '--rules', '--allow-unmatched-rule'].includes(key)) {
      const value = args[++index];
      if (!value || value.startsWith('-')) refuse('option requires a value');
      if (key === '--allow-unmatched-rule') {
        safePath(value, true);
        if (exceptions.has(value)) refuse('duplicate rule exception');
        exceptions.add(value);
      } else {
        if (options.has(key)) refuse('duplicate option');
        options.set(key, value);
      }
    } else refuse('unknown export option');
  }
  for (const key of ['--source', '--output', '--manifest']) if (!options.has(key)) refuse(`required option: ${key}`);
  const rehearsal = options.has('--worktree');
  if (exceptions.size && !rehearsal) refuse('unmatched rule exceptions require worktree rehearsal mode');
  const root = fs.realpathSync(git(['rev-parse', '--show-toplevel']).toString('utf8').trim());
  process.chdir(root);
  const sourceCommit = git(['rev-parse', '--verify', '--end-of-options', `${options.get('--source')}^{commit}`]).toString('utf8').trim();
  if (!/^[a-f0-9]{40,64}$/.test(sourceCommit)) refuse('invalid source commit');
  const output = destination(options.get('--output'));
  const manifest = destination(options.get('--manifest'));
  if (inside(root, output) || inside(output, root)) refuse('export directory must be outside the source repository');
  if (inside(output, manifest)) refuse('manifest must be outside the export directory');
  if (fs.existsSync(manifest)) refuse('manifest already exists');
  if (fs.existsSync(output) && (!fs.lstatSync(output).isDirectory() || fs.readdirSync(output).length)) refuse('export directory must be empty');
  const rulesBytes = options.has('--rules') ? fs.readFileSync(options.get('--rules')) : git(['show', `${sourceCommit}:export/rules`]);
  const rules = [];
  for (const line of rulesBytes.toString('utf8').split(/\r?\n/)) {
    if (!line.trim() || line.trim().startsWith('#')) continue;
    const match = /^(include|exclude) ([^\s]+)$/.exec(line.trim());
    if (!match) refuse('invalid export rule');
    const spec = safePath(match[2], true);
    if (rules.some(rule => rule.action === match[1] && rule.spec === spec)) refuse('duplicate export rule');
    rules.push({ action: match[1], spec, hits: 0 });
  }
  if (!rules.length) refuse('export rules cannot be empty');
  const entries = new Map();
  const treeBytes = git(['ls-tree', '-r', '-z', '--full-tree', sourceCommit]);
  if (!Buffer.from(treeBytes.toString('utf8')).equals(treeBytes)) refuse('source paths must use valid UTF-8');
  for (const raw of treeBytes.toString('utf8').split('\0')) {
    if (!raw) continue;
    const match = /^(\d{6}) (blob|commit) ([a-f0-9]{40,64})\t(.+)$/.exec(raw);
    if (!match) refuse('invalid Git tree entry');
    const [, mode, type, object, name] = match;
    safePath(name);
    if (type !== 'blob' || !['100644', '100755', '120000'].includes(mode)) refuse('gitlinks and unsupported file modes are forbidden');
    entries.set(name, { path: name, mode, object });
  }
  if (rehearsal) {
    entries.clear();
    const nameBytes = git(['ls-files', '-z', '--cached', '--others', '--exclude-standard']);
    if (!Buffer.from(nameBytes.toString('utf8')).equals(nameBytes)) refuse('worktree paths must use valid UTF-8');
    const names = new Set(nameBytes.toString('utf8').split('\0').filter(Boolean));
    for (const name of names) {
      safePath(name);
      const full = path.join(root, name);
      const stat = fs.lstatSync(full, { throwIfNoEntry: false });
      // Cached paths deleted from the current checkout are absent rehearsal inputs.
      if (!stat) continue;
      // Resolve the parent before inspecting the leaf to prevent directory-link traversal.
      if (!inside(root, fs.realpathSync(path.dirname(full)))) refuse('worktree parent leaves the repository');
      if (!stat.isFile() && !stat.isSymbolicLink()) refuse('worktree accepts regular files and symbolic links only');
      entries.set(name, { path: name, mode: stat.isSymbolicLink() ? '120000' : stat.mode & 0o111 ? '100755' : '100644' });
    }
  }
  const included = new Map();
  const excludedPaths = [];
  for (const [name, entry] of [...entries].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)) {
    const matching = rules.filter(rule => matches(rule.spec, name));
    if (!matching.length) refuse('source contains an unclassified path');
    for (const rule of matching) rule.hits++;
    const bytes = rehearsal
      ? entry.mode === '120000' ? Buffer.from(fs.readlinkSync(path.join(root, name))) : fs.readFileSync(path.join(root, name))
      : git(['cat-file', 'blob', entry.object]);
    entry.bytes = bytes;
    if (matching.some(rule => rule.action === 'exclude')) excludedPaths.push(name);
    else included.set(name, entry);
  }
  const unusedExceptions = new Set(exceptions);
  for (const rule of rules) {
    if (!rule.hits) {
      if (!exceptions.has(rule.spec)) refuse('rule matches no source paths');
      unusedExceptions.delete(rule.spec);
    }
  }
  if (unusedExceptions.size) refuse('rule exception does not name an unmatched rule');
  if (!included.size) refuse('export must include at least one file');
  // Resolve links through the selected leaves, including links in directory positions.
  function resolveLink(name, trail = new Set()) {
    const parts = name.split('/');
    for (let index = 1; index <= parts.length; index++) {
      const prefix = parts.slice(0, index).join('/');
      const entry = included.get(prefix);
      if (!entry || entry.mode !== '120000') continue;
      if (trail.has(prefix)) refuse('symbolic link cycle');
      const target = entry.bytes.toString('utf8');
      if (!target || target.includes('\0') || path.posix.isAbsolute(target) || target.includes('\\')) refuse('invalid symbolic link target');
      const resolved = path.posix.normalize(path.posix.join(path.posix.dirname(prefix), target, ...parts.slice(index)));
      if (resolved === '..' || resolved.startsWith('../') || path.posix.isAbsolute(resolved)) refuse('symbolic link leaves the export');
      return resolveLink(resolved, new Set([...trail, prefix]));
    }
    if (!included.has(name) && ![...included.keys()].some(leaf => leaf.startsWith(`${name}/`))) refuse('symbolic link target is absent from export');
    return name;
  }
  for (const [name, entry] of entries) {
    if (entry.mode !== '120000') continue;
    if (!Buffer.from(entry.bytes.toString('utf8')).equals(entry.bytes)) refuse('symbolic link targets must use valid UTF-8');
    const target = entry.bytes.toString('utf8');
    if (!target || target.includes('\0') || target.includes('\\')) refuse('invalid symbolic link target');
    if (!included.has(name)) {
      const resolved = path.posix.normalize(path.posix.join(path.posix.dirname(name), target));
      if (path.posix.isAbsolute(target) || resolved === '..' || resolved.startsWith('../')) refuse('symbolic link leaves source tree');
    } else resolveLink(name);
  }
  const files = [...included.values()].map(entry => ({
    path: entry.path, mode: entry.mode,
    sha256: crypto.createHash('sha256').update(entry.bytes).digest('hex'),
  }));
  stage = fs.mkdtempSync(path.join(path.dirname(output), '.public-export-'));
  for (const entry of included.values()) {
    const file = path.join(stage, entry.path);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    if (entry.mode === '120000') fs.symlinkSync(entry.bytes.toString('utf8'), file);
    else fs.writeFileSync(file, entry.bytes, { flag: 'wx', mode: entry.mode === '100755' ? 0o755 : 0o644 });
  }
  // Publish the external receipt first; a failure cannot leave a successful-looking tree.
  const receipt = { sourceCommit, mode: rehearsal ? 'worktree' : 'commit', rulesSha256: crypto.createHash('sha256').update(rulesBytes).digest('hex'), files, excludedPaths, unmatchedRuleExceptions: [...exceptions].sort() };
  fs.writeFileSync(manifest, `${JSON.stringify(receipt, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  try {
    if (fs.existsSync(output)) fs.rmdirSync(output);
    fs.renameSync(stage, output);
    stage = undefined;
  } catch (error) {
    fs.unlinkSync(manifest);
    throw error;
  }
  console.log(`Exported ${files.length} files; excluded ${excludedPaths.length}; mode=${receipt.mode}; source=${sourceCommit}`);
  if (exceptions.size) console.log(`Explicit unmatched rule exceptions: ${exceptions.size}; see external manifest`);
} catch (error) {
  if (stage) fs.rmSync(stage, { recursive: true, force: true });
  // Filesystem and Git errors may contain private host paths; expose only controlled diagnostics.
  console.error(`export-public: ${error.code ? 'local input or destination could not be read or written' : error.message}`);
  process.exitCode = 1;
}
NODE
