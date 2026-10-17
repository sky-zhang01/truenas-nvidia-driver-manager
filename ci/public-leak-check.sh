#!/usr/bin/env bash
# Scan a complete exported directory; diagnostics never include input text or paths.
set -Eeuo pipefail
set +x
node - "$@" <<'JS'
'use strict';
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const net = require('net');
const {spawnSync} = require('child_process');
function fail(message) { console.error('FAIL: ' + message); process.exit(1); }
function digest(data) { return crypto.createHash('sha256').update(data).digest('hex'); }
let root = '.', allowlist, contentOnly = false, genericOnly = false;
const args = process.argv.slice(2);
for (let i = 0; i < args.length; i++) {
  if (args[i] === '--root' && args[i + 1]) root = args[++i];
  else if (args[i] === '--allowlist' && args[i + 1]) allowlist = args[++i];
  else if (args[i] === '--content-only') contentOnly = true;
  else if (args[i] === '--generic-only') genericOnly = true;
  else fail('usage: public-leak-check.sh --root DIR [--allowlist PATH] [--generic-only] [--content-only]');
}
const hostValue = process.env.PUBLIC_RELEASE_FORBIDDEN_HOSTS || '';
const termValue = process.env.PUBLIC_RELEASE_FORBIDDEN_TERMS || '';
const splitPolicy = value => value.trim().split(/[,\s]+/).filter(Boolean);
const hostname = value => value.length <= 253 && value.split('.').every(label => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/i.test(label));
const hostBase = value => value.replace(/^\*\./, '').replace(/\.$/, '').toLowerCase();
const escape = value => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
if (genericOnly && (hostValue || termValue)) fail('generic-only mode cannot discard supplied private policy.');
if (!genericOnly && (!splitPolicy(hostValue).length || !splitPolicy(termValue).length)) fail('PUBLIC_RELEASE_FORBIDDEN_HOSTS and PUBLIC_RELEASE_FORBIDDEN_TERMS are both required.');
const privateHosts = splitPolicy(hostValue).map(hostBase);
const privateTerms = splitPolicy(termValue);
if (privateHosts.some(value => !hostname(value)) || privateTerms.some(value => !/^[\p{L}\p{N}]+(?:[-_][\p{L}\p{N}]+)*$/u.test(value))) fail('invalid private policy; no input details are disclosed.');
const hostRules = privateHosts.map(value => new RegExp('(?<![a-z0-9-])' + escape(value) + '(?![a-z0-9-])', 'i'));
const termRules = privateTerms.map(value => new RegExp('(?<![\\p{L}\\p{N}])' + value.split(/[-_]/).map(escape).join('[-_\\s]+') + '(?![\\p{L}\\p{N}])', 'iu'));
const flatHostRules = privateHosts.map(value => new RegExp(escape(value), 'gi'));
const flatTermRules = privateTerms.map(value => new RegExp(value.split(/[-_]/).map(escape).join('[-_]*'), 'giu'));
// Decode the documented host spellings without changing literal boundaries.
function decodedDots(value) {
  return value.replace(/%2e|\\+u002e|\\+x2e|\\+056/gi, '.')
    .replace(/\[\s*(?:\.|dot)\s*\]|\(\s*(?:\.|dot)\s*\)/gi, '.')
    .replace(/\s+dot\s+/gi, '.');
}
// Joining adjacent string literals also covers splits inside a DNS label.
function normalized(value) {
  return decodedDots(value).replace(/["'`]\s*\+\s*["'`]/g, '');
}
function decodedSplitDots(value) {
  return value.replace(/%\s*2\s*e|(?:\\\s*)+(?:u\s*0\s*0\s*2\s*e|x\s*2\s*e|0\s*5\s*6)/gi, '.')
    .replace(/\s+d\s*o\s*t\s+/gi, '.');
}
function sourceView(value) {
  const starts = new Uint32Array(value.length), ends = new Uint32Array(value.length);
  for (let index = 0; index < value.length; index++) { starts[index] = index; ends[index] = index + 1; }
  return {
    text: value, starts, ends,
    replace(pattern, replacement = '') {
      const edits = [...this.text.matchAll(pattern)];
      if (!edits.length) return this;
      const output = match => replacement === '$1' ? match[1] : replacement;
      const length = edits.reduce((size, match) => size - match[0].length + output(match).length, this.text.length);
      const starts = new Uint32Array(length), ends = new Uint32Array(length), parts = [];
      let cursor = 0, written = 0;
      for (const match of edits) {
        const index = match.index, end = index + match[0].length, kept = output(match);
        parts.push(this.text.slice(cursor, index), kept);
        starts.set(this.starts.subarray(cursor, index), written);
        ends.set(this.ends.subarray(cursor, index), written);
        written += index - cursor;
        for (let offset = 0; offset < kept.length; offset++) {
          starts[written + offset] = this.starts[index + (replacement === '$1' ? offset : 0)];
          ends[written + offset] = this.ends[replacement === '$1' ? index + offset : end - 1];
        }
        written += kept.length;
        cursor = end;
      }
      parts.push(this.text.slice(cursor));
      starts.set(this.starts.subarray(cursor), written);
      ends.set(this.ends.subarray(cursor), written);
      this.text = parts.join('');
      this.starts = starts;
      this.ends = ends;
      return this;
    },
  };
}
// Private-policy flattening covers ordinary writing and code: layout, line breaks,
// comments, literal joins across languages, interpolation, shell variable placeholders,
// and common dot encodings. Program-computed values (base64, printf/character-code
// assembly, runtime transforms, and similar operations) are outside this view's scope.
// This gate targets internal names accidentally included in the public tree, not
// authors deliberately evading it.
function flattened(value, stripComments = false) {
  // Match content after flattening, but check boundaries at original adjacent characters.
  if (stripComments) value = value.replace(/\/\*[\s\S]*?\*\/|\/\/[^\r\n]*|<!--[\s\S]*?-->/g, '');
  return normalized(normalized(decodedSplitDots(value).replace(/\\\r?\n/g, ''))
    .replace(/(^|:[\t ]*|-[\t ]+)[>|](?:[+-][1-9]?|[1-9][+-]?)?(?:[\t ]+#[^\r\n]*)?[\t ]*\r?$/gm, '$1')
    .replace(/^[\t ]*(?:\/\/|#|\/\*|\*\/?)[\t ]*/gm, '')
    .replace(/\/\*|\*\/|<!--|-->/g, '')
    .replace(/\b(?:br|rb|fr|rf|r|b|f|u8|u|L)#{0,255}(?=["'])/g, '')
    .replace(/"#+/g, '"')
    .replace(/["'`]\s*(?:\+|\.{1,2}|,)\s*["'`]/g, '')
    .replace(/\$\{[A-Za-z_][A-Za-z0-9_]*\}|\$[A-Za-z_][A-Za-z0-9_]*/g, '')
    .replace(/\b[A-Za-z_][A-Za-z0-9_]*\s*\+=/g, '')
    .replace(/\$\{|\}/g, '')
    .replace(/["'`+]/g, '')
    .replace(/[\s\p{White_Space}]+/gu, ''));
}
function flatMatches(value, flat, rules, leftLabel, rightLabel) {
  return rules.some(rule => {
    rule.lastIndex = 0;
    let match;
    while ((match = rule.exec(flat.text))) {
      const start = flat.starts[match.index], end = flat.ends[match.index + match[0].length - 1];
      if (!leftLabel.test(value.slice(Math.max(0, start - 2), start)) && !rightLabel.test(value.slice(end, end + 2))) return true;
      rule.lastIndex = match.index + (flat.text.codePointAt(match.index) > 0xffff ? 2 : 1);
    }
    return false;
  });
}
const forbidden = value => {
  const originals = [value], restored = decodedDots(decodedSplitDots(value));
  if (restored !== value) originals.push(restored);
  if (originals.some(text => hostRules.some(rule => rule.test(text)) || termRules.some(rule => rule.test(text)))) return true;
  // Literal-to-literal joins are transparent; all other removed characters separate boundaries.
  value = value.replace(/["'`]\s*(?:\+|\.{1,2}|,)?\s*["'`]/g, '');
  const decoded = normalized(value);
  if (hostRules.some(rule => rule.test(decoded)) || termRules.some(rule => rule.test(value) || rule.test(decoded))) return true;
  // Check comment bodies above as well as joins with intervening comments.
  return [false, true].some(stripComments => {
    const text = flattened(value, stripComments);
    const contains = rules => rules.some(rule => { rule.lastIndex = 0; return rule.test(text); });
    if (!contains(flatHostRules) && !contains(flatTermRules)) return false;
    const flat = flattened(sourceView(value), stripComments);
    return flatMatches(value, flat, flatHostRules, /[a-z0-9-]$/i, /^[a-z0-9-]/i)
      || flatMatches(value, flat, flatTermRules, /[\p{L}\p{N}]$/u, /^[\p{L}\p{N}]/u);
  });
};
if (contentOnly && ['CI', 'GITHUB_ACTIONS', 'GITEA_ACTIONS'].some(key => process.env[key] && process.env[key] !== 'false')) fail('content-only mode is unavailable in CI.');
try {
  root = fs.realpathSync(root);
  if (!fs.statSync(root).isDirectory()) fail('root must be a directory.');
  allowlist = allowlist || path.join(root, 'ci/public-leak-allowlist.json');
  const policy = JSON.parse(fs.readFileSync(allowlist, 'utf8'));
  const lineRules = ['ipv4', 'ipv6', 'internal-dns', 'chinese'];
  const fileRules = ['binary', 'mount', 'email', 'chinese', 'process'];
  const validRules = [...lineRules, ...fileRules];
  if (!policy || !Array.isArray(policy.entries) || !Array.isArray(policy.public_hosts) || policy.public_hosts.some(value => typeof value !== 'string' || !(hostname(hostBase(value)) || net.isIP(value)))) fail('invalid allowlist.');
  const entries = new Map();
  for (const entry of policy.entries) {
    if (!entry || typeof entry.path !== 'string' || entry.path === '' || entry.path.split('/').some(p => p === '..' || p === '.' || !p) || path.isAbsolute(entry.path) || entry.path.includes('\\') || !Array.isArray(entry.rules) || !entry.rules.length || entry.rules.some(rule => !validRules.includes(rule)) || typeof entry.reason !== 'string' || !entry.reason.trim()) fail('invalid allowlist entry.');
    const lineEntry = entry.line_sha256 !== undefined;
    if (!/^[a-f0-9]{64}$/.test(lineEntry ? entry.line_sha256 : entry.sha256) || (lineEntry && entry.sha256 !== undefined) || entry.rules.some(rule => lineEntry ? !lineRules.includes(rule) : !fileRules.includes(rule))) fail('invalid allowlist scope.');
    const list = entries.get(entry.path) || [];
    if (list.some(prior => prior.sha256 === entry.sha256 && prior.line_sha256 === entry.line_sha256)) fail('duplicate allowlist entry.');
    list.push(entry);
    entries.set(entry.path, list);
  }
  const ipv4 = /(?<![\w.])(?:\d{1,3}\.){3}\d{1,3}(?!\w|\.\d)/g;
  const mac = /\b(?:[a-f\d]{2}[:-]){5}[a-f\d]{2}\b|\b(?:[a-f\d]{4}\.){2}[a-f\d]{4}\b/gi;
  const ipv6 = /(?<![\w:])(?:[a-f\d]{0,4}:){2,}(?:[a-f\d.]{0,15})(?![\w:])/gi;
  const publicIpv6 = new net.BlockList();
  publicIpv6.addAddress('::', 'ipv6');
  publicIpv6.addAddress('::1', 'ipv6');
  publicIpv6.addSubnet('2001:db8::', 32, 'ipv6');
  const internalDns = /(?<![\w.-])(?:[a-z\d](?:[a-z\d-]*[a-z\d])?\.)+(?:local|lan|internal|home\.arpa|localdomain|intranet|corp|ts\.net)\b/i;
  // Consume the whole host token before deciding whether it is reserved.
  // A reserved prefix in a longer or malformed label must not grant an exception.
  const email = /[a-z\d.!#$%&'*+/=?^_`{|}~-]+@([a-z\d][a-z\d._-]*\.[a-z\d_-]+)/gi;
  const processWords = new RegExp('\\b(?:' + ['orches' + 'trator', 'author' + '\\s+seat', 'Authored' + '\\s+by'].join('|') + ')\\b', 'i');
  const findings = new Set();
  // Formats that may contain printable bytes still require exact registration.
  // SVG is intentionally absent: vector brand assets remain text-scanned.
  const binaryExtensions = new Set([
    '.7z', '.a', '.avif', '.bin', '.bmp', '.bz2', '.class', '.deb', '.dll',
    '.dylib', '.eot', '.exe', '.gif', '.gz', '.ico', '.img', '.iso', '.jar',
    '.jpeg', '.jpg', '.mp3', '.mp4', '.o', '.pdf', '.png', '.rar', '.rpm',
    '.so', '.tar', '.tif', '.tiff', '.ttf', '.wasm', '.webp', '.woff',
    '.woff2', '.xz', '.zip', '.zst',
  ]);
  const textMimes = new Set([
    'application/json', 'application/xml', 'application/javascript',
    'application/x-javascript', 'application/x-empty', 'inode/x-empty', 'image/svg+xml',
  ]);
  const toolEnv = {...process.env};
  delete toolEnv.PUBLIC_RELEASE_FORBIDDEN_HOSTS;
  delete toolEnv.PUBLIC_RELEASE_FORBIDDEN_TERMS;
  // RFC 2606 / RFC 6761 names are safe examples, with DNS label boundaries.
  // Private policy is checked separately and always takes precedence.
  function reservedHost(host) {
    return hostname(host) && ['example', 'invalid', 'test', 'localhost', 'example.com', 'example.net', 'example.org']
      .some(base => host === base || host.endsWith('.' + base));
  }
  function publicHost(host) {
    return reservedHost(host) || policy.public_hosts.some(value => {
      const base = hostBase(value);
      return host === base || value.startsWith('*.') && host.endsWith('.' + base);
    });
  }
  function urlDenied(text) {
    for (const match of text.matchAll(/\b(?:https?:\/\/|ssh:\/\/|git[@])([^\s"'`\\]*)/gi)) {
      let authority = match[1];
      // Empty prefixes and explicit template placeholders have no literal host.
      if (!authority || /^(?:<[^>]+>|\$\{[^}]+\}|\{[^}]+\}|\$[a-z_]\w*)/i.test(authority)) continue;
      authority = authority.split(/[/?#{}]/)[0];
      authority = authority.slice(authority.lastIndexOf('@') + 1);
      const host = (authority.startsWith('[') ? authority.slice(1, authority.indexOf(']')) : authority.split(':')[0]).replace(/\.$/, '').toLowerCase();
      if (!host || !publicHost(host)) return true;
    }
    return false;
  }
  function addressChecks(text) {
    const ipv6Addresses = [...text.matchAll(ipv6)].map(match => ({
      index: match.index, value: match[0].replace(/\.+$/, ''),
    })).filter(match => net.isIP(match.value) === 6);
    return {
      ipv4: [...text.matchAll(ipv4)].some(match => {
        const [a,b,c,d] = match[0].split('.').map(Number);
        return [a,b,c,d].every(n => n <= 255) && !(a === 0 && b === 0 && c === 0 && d === 0 || a === 127 || a === 255 && b === 255 && c === 255 && d === 255 || a === 192 && b === 0 && c === 2 || a === 198 && b === 51 && c === 100 || a === 203 && b === 0 && c === 113);
      }),
      ipv6: ipv6Addresses.some(match => !publicIpv6.check(match.value, 'ipv6')),
      // Six short groups inside a valid IPv6 address are not a MAC literal.
      mac: [...text.matchAll(mac)].some(match => !ipv6Addresses.some(address => match.index >= address.index && match.index + match[0].length <= address.index + address.value.length) && !/^00005e0053[\da-f]{2}$/i.test(match[0].replace(/[:.-]/g, ''))),
      'internal-dns': internalDns.test(text),
    };
  }
  function commonChecks(text) {
    return {
      'url-host': urlDenied(text),
      'home-path': /\/(?:Users|home)\/+(?!<)[^\s/<>"'`]+|\b[a-z]:[\\/]+Users[\\/]+(?!<)[^\\/\s<>"'`]+/i.test(text),
    };
  }
  let count = 0;
  function record(rel, rule) { findings.add('file-id=' + digest(rel).slice(0, 16) + ' rule=' + rule); }
  function scan(rel, data) {
    count++;
    // Private policy values are unconditional, including in filenames and binaries.
    if (forbidden(rel) || forbidden(data.toString('utf8'))) record(rel, 'forbidden');
    const hash = digest(data), registered = entries.get(rel) || [];
    const exempt = (rule, line) => registered.some(entry => entry.rules.includes(rule) && (line === undefined ? entry.sha256 === hash : entry.line_sha256 === digest(line)));
    let text;
    try { text = new TextDecoder('utf-8', {fatal: true}).decode(data); } catch { text = null; }
    // Native format detection catches renamed files and headers after padding.
    // Neither tool stream nor the private policy environment is disclosed.
    const format = spawnSync('file', ['--brief', '--mime-type', '-'], {
      input: data, stdio: ['pipe', 'pipe', 'pipe'], env: toolEnv, maxBuffer: 4096,
    });
    // file may close stdin after reading its detection window on large inputs.
    // Accept that pipe closure only after a successful tool exit and valid MIME.
    if ((format.error && format.error.code !== 'EPIPE') || format.status !== 0) fail('file utility unavailable or format inspection failed.');
    const mime = format.stdout.toString('utf8').trim().toLowerCase();
    if (!/^[a-z0-9][a-z0-9!#$&^_.+-]*\/[a-z0-9][a-z0-9!#$&^_.+-]*$/.test(mime)) fail('file utility returned an invalid format.');
    const binary = binaryExtensions.has(path.extname(rel).toLowerCase())
      || !(mime.startsWith('text/') || textMimes.has(mime))
      || text === null || data.includes(0) || (text && /[\x01-\x08\x0e-\x1f]/.test(text));
    if (binary) {
      if (!exempt('binary') || /\.svg$/i.test(rel)) record(rel, 'binary');
      return;
    }
    for (const line of text.split(/\r?\n/)) {
      for (const [rule, hit] of Object.entries(addressChecks(normalized(line)))) if (hit && !exempt(rule, line)) record(rel, rule);
    }
    const checks = {
      ...commonChecks(normalized(text)),
      // A bare root ends at whitespace or a text delimiter. Every actual pool
      // token is denied unless it begins with an explicit placeholder.
      // This includes Unicode names and shell-variable spellings.
      mount: /\/mnt\/+(?!<[^<>\r\n/]+>)(?=[^\s"'`/])/u.test(text),
      email: [...text.replace(/\bssh:\/\/[^@\s/]+@[^\s/:"'`]+|\bgit[@][^\s/:"'`]+(?=:)/gi, '').matchAll(email)].some(match => !reservedHost(hostBase(match[1])) && !/^[a-z\d+_.-]+@users\.noreply\.github\.com$/i.test(match[0])),
      chinese: !/(?:^|\/)\w[^/]*\.zh-CN\.md$/.test(rel) && text.split(/\r?\n/).some(line => /\p{Script=Han}/u.test(line) && !exempt('chinese', line)),
      process: processWords.test(text),
    };
    for (const [rule, hit] of Object.entries(checks)) if (hit && !exempt(rule)) record(rel, rule);
  }
  function walk(dir, prefix = '') {
    for (const name of fs.readdirSync(dir).sort()) {
      // Git administration is not exported material, including snapshot metadata.
      if (!prefix && name === '.git') continue;
      const rel = prefix + name, full = path.join(dir, name), stat = fs.lstatSync(full);
      if (forbidden(rel)) record(rel, 'forbidden');
      for (const [rule, hit] of Object.entries({...addressChecks(normalized(rel)), ...commonChecks(normalized(rel))})) if (hit) record(rel, rule);
      if (stat.isDirectory()) walk(full, rel + '/');
      else if (stat.isSymbolicLink()) {
        const resolved = fs.realpathSync(full);
        if (resolved !== root && !resolved.startsWith(root + path.sep)) record(rel, 'external-link');
        scan(rel, Buffer.from(fs.readlinkSync(full)));
      } else if (stat.isFile()) scan(rel, fs.readFileSync(full));
      else record(rel, 'unsupported-file');
    }
  }
  walk(root);
  if (findings.size) { findings.forEach(f => console.error('FAIL: ' + f)); process.exit(1); }
  console.log('PASS: content privacy scan (' + count + ' files).');
  if (genericOnly) console.log('NOT RUN: private policy (explicit generic-only mode).');
  if (contentOnly) { console.log('NOT RUN: gitleaks (explicit local content-only mode).'); process.exit(0); }
  const config = path.join(root, '.gitleaks.toml');
  if (!fs.lstatSync(config).isFile()) fail('gitleaks configuration is required.');
  // Suppress both streams even on tool failure; external diagnostics may contain secrets.
  const result = spawnSync('gitleaks', ['dir', root, '--config', config, '--redact=100', '--no-banner', '--exit-code', '1'], {stdio: 'ignore', env: toolEnv});
  if (result.error || result.status !== 0) fail('gitleaks unavailable or scan failed; no tool output is disclosed.');
  console.log('PASS: gitleaks scan.');
} catch { fail('could not inspect export or policy; no input details are disclosed.'); }
JS
