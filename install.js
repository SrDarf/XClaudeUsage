#!/usr/bin/env node
/*
 * XClaudeUsage automated installer.
 *
 * Cross-platform (Windows / macOS / Linux). Single Node.js file, no npm
 * dependencies. Run with:
 *
 *     node install.js
 *
 * or directly from the web:
 *
 *     curl -fsSL https://raw.githubusercontent.com/SrDarf/XClaudeUsage/main/install.js | node
 *
 * Behavior is documented in README.md (section "Quick install (automated)").
 */

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const https = require('node:https');
const readline = require('node:readline');
const crypto = require('node:crypto');

const REPO_RAW_BASE =
  'https://raw.githubusercontent.com/SrDarf/XClaudeUsage/main';
const HOOK_FILES = ['xclaude-usage.js', 'xclaude-record.js'];
const HOOK_TIMEOUT = 10;

const HOME = os.homedir();
const CLAUDE_DIR = path.join(HOME, '.claude');
const HOOKS_DIR = path.join(CLAUDE_DIR, 'hooks');
const DATA_DIR = path.join(CLAUDE_DIR, 'data');
const SETTINGS_PATH = path.join(CLAUDE_DIR, 'settings.json');
const CLOUD_CONFIG_PATH = path.join(DATA_DIR, 'xclaude-cloud.json');

const BASE_EVENTS = ['Stop', 'SubagentStop'];
const CLOUD_EVENTS = ['Stop', 'SubagentStop', 'SubagentStart', 'PostToolUse'];

function abort(msg) {
  console.error(`\n[install] ABORT: ${msg}\n`);
  process.exit(1);
}

function info(msg) {
  console.log(`[install] ${msg}`);
}

function fail(msg) {
  console.error(`[install] ${msg}`);
}

function nodeMajorMinor() {
  const m = process.version.match(/^v(\d+)\.(\d+)/);
  if (!m) return [0, 0];
  return [Number(m[1]), Number(m[2])];
}

function hookCommand(scriptName) {
  const hookPath = path.join(HOOKS_DIR, scriptName).split(path.sep).join('/');
  return `node "${hookPath}"`;
}

function isXClaudeStatusline(s) {
  return Boolean(
    s && typeof s === 'object' && typeof s.command === 'string' &&
      s.command.includes('xclaude-usage.js'),
  );
}

function isXClaudeHookEntry(entry) {
  return Boolean(
    entry && typeof entry === 'object' && typeof entry.command === 'string' &&
      entry.command.includes('xclaude-record.js'),
  );
}

function httpRequest(url, { method = 'GET', headers = {} } = {}) {
  return new Promise((resolve, reject) => {
    const req = https.request(url, { method, headers }, (res) => {
      if (
        res.statusCode &&
        res.statusCode >= 300 &&
        res.statusCode < 400 &&
        res.headers.location
      ) {
        res.resume();
        resolve(httpRequest(res.headers.location, { method, headers }));
        return;
      }
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => {
        resolve({
          statusCode: res.statusCode || 0,
          headers: res.headers,
          body: Buffer.concat(chunks),
        });
      });
    });
    req.on('error', reject);
    req.end();
  });
}

// Persist GitHub raw ETags so we can short-circuit downloads with
// If-None-Match. The CDN ETag is opaque (Fastly-internal, not derivable from
// the file contents), so we have to remember what we saw last time.
const ETAGS_PATH = path.join(DATA_DIR, 'xclaude-installer-etags.json');

function loadETags() {
  if (!fs.existsSync(ETAGS_PATH)) return {};
  try {
    const obj = JSON.parse(fs.readFileSync(ETAGS_PATH, 'utf8'));
    return (obj && typeof obj === 'object') ? obj : {};
  } catch (_) {
    return {};
  }
}

function saveETags(etags) {
  fs.mkdirSync(DATA_DIR, { recursive: true });
  fs.writeFileSync(ETAGS_PATH, JSON.stringify(etags, null, 2) + '\n');
}

async function checkConnectivity() {
  try {
    const r = await httpRequest(`${REPO_RAW_BASE}/${HOOK_FILES[0]}`, {
      method: 'HEAD',
    });
    if (r.statusCode !== 200) {
      abort(
        `cannot reach GitHub (HTTP ${r.statusCode}). Check your connection and try again.`,
      );
    }
  } catch (e) {
    abort(`cannot reach GitHub: ${e.message}. Check your connection.`);
  }
}

async function downloadFile(url, destPath, prevETag) {
  // Send If-None-Match so GitHub's CDN can answer 304 (no body) when the file
  // hasn't changed since we last saw it. Only valid if the local file still
  // exists — otherwise we genuinely need the body, even if the ETag matches.
  const headers = {};
  if (prevETag && fs.existsSync(destPath)) {
    headers['If-None-Match'] = prevETag;
  }
  const r = await httpRequest(url, { headers });
  const newETag =
    (r.headers && (r.headers.etag || r.headers.ETag)) || prevETag || null;
  if (r.statusCode === 304) {
    return { skipped: true, etag: newETag };
  }
  if (r.statusCode !== 200) {
    throw new Error(`HTTP ${r.statusCode} for ${url}`);
  }
  fs.mkdirSync(path.dirname(destPath), { recursive: true });
  const tmp = `${destPath}.download-${process.pid}`;
  fs.writeFileSync(tmp, r.body);
  fs.renameSync(tmp, destPath);
  return { skipped: false, etag: newETag };
}

function readSettings() {
  if (!fs.existsSync(SETTINGS_PATH)) return {};
  let raw;
  try {
    raw = fs.readFileSync(SETTINGS_PATH, 'utf8');
  } catch (e) {
    abort(`cannot read ${SETTINGS_PATH}: ${e.message}`);
  }
  if (raw.trim() === '') return {};
  try {
    const parsed = JSON.parse(raw);
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
      abort(`${SETTINGS_PATH} does not contain a JSON object at the top level.`);
    }
    return parsed;
  } catch (e) {
    abort(
      `${SETTINGS_PATH} is not valid JSON (${e.message}). Fix it manually before running this installer.`,
    );
  }
}

function backupSettings() {
  if (!fs.existsSync(SETTINGS_PATH)) return null;
  const stamp = new Date().toISOString().replace(/[:.]/g, '-');
  const dest = `${SETTINGS_PATH}.backup-${stamp}`;
  fs.copyFileSync(SETTINGS_PATH, dest);
  return dest;
}

function writeSettingsIfChanged(obj) {
  const body = `${JSON.stringify(obj, null, 2)}\n`;
  let originalBody = null;
  if (fs.existsSync(SETTINGS_PATH)) {
    try { originalBody = fs.readFileSync(SETTINGS_PATH, 'utf8'); } catch (_) {}
  }
  if (originalBody === body) return { changed: false, backup: null };
  let backup = null;
  if (originalBody !== null) {
    const stamp = new Date().toISOString().replace(/[:.]/g, '-');
    backup = `${SETTINGS_PATH}.backup-${stamp}`;
    fs.copyFileSync(SETTINGS_PATH, backup);
  }
  fs.mkdirSync(CLAUDE_DIR, { recursive: true });
  const tmp = `${SETTINGS_PATH}.write-${process.pid}`;
  fs.writeFileSync(tmp, body);
  fs.renameSync(tmp, SETTINGS_PATH);
  return { changed: true, backup };
}

function ensureXClaudeStatusline(settings) {
  const desired = { type: 'command', command: hookCommand('xclaude-usage.js') };
  const existing = settings.statusLine;
  if (existing === undefined || existing === null) {
    settings.statusLine = desired;
    return { action: 'created' };
  }
  if (isXClaudeStatusline(existing)) {
    settings.statusLine = { ...existing, ...desired };
    return { action: 'updated' };
  }
  abort(
    `a non-XClaude statusLine is already configured in ${SETTINGS_PATH}. ` +
      `Refusing to overwrite it. Remove or rename it manually, then re-run the installer.`,
  );
}

function ensureXClaudeHooks(settings, events) {
  if (!settings.hooks || typeof settings.hooks !== 'object') {
    settings.hooks = {};
  }
  const desiredEntry = {
    type: 'command',
    command: hookCommand('xclaude-record.js'),
    timeout: HOOK_TIMEOUT,
  };
  const summary = {};
  for (const event of events) {
    const list = Array.isArray(settings.hooks[event]) ? settings.hooks[event] : [];
    let touched = false;
    for (const group of list) {
      if (!group || !Array.isArray(group.hooks)) continue;
      for (let i = 0; i < group.hooks.length; i++) {
        if (isXClaudeHookEntry(group.hooks[i])) {
          group.hooks[i] = { ...group.hooks[i], ...desiredEntry };
          touched = true;
        }
      }
    }
    if (!touched) {
      list.push({ hooks: [desiredEntry] });
      summary[event] = 'added';
    } else {
      summary[event] = 'updated';
    }
    settings.hooks[event] = list;
  }
  return summary;
}

function ask(rl, question) {
  return new Promise((resolve) => rl.question(question, (a) => resolve(a)));
}

function readExistingCloudConfig() {
  if (!fs.existsSync(CLOUD_CONFIG_PATH)) return null;
  try {
    const raw = fs.readFileSync(CLOUD_CONFIG_PATH, 'utf8');
    const obj = JSON.parse(raw);
    if (
      obj && typeof obj === 'object' &&
      typeof obj.libsql_url === 'string' && obj.libsql_url &&
      typeof obj.auth_token === 'string' && obj.auth_token &&
      typeof obj.device_id === 'string' && obj.device_id &&
      (obj.libsql_url.startsWith('libsql://') || obj.libsql_url.startsWith('https://'))
    ) {
      return {
        libsql_url: obj.libsql_url,
        auth_token: obj.auth_token,
        device_id: obj.device_id,
      };
    }
  } catch (_) {
    // fall through; treat as missing
  }
  return null;
}

function maskToken(token) {
  if (!token || token.length <= 12) return '***';
  return `${token.slice(0, 8)}…${token.slice(-4)} (${token.length} chars)`;
}

async function promptCloud(rl) {
  const ans = (await ask(
    rl,
    'Enable Turso cloud sync for multi-device aggregation? [y/N]: ',
  )).trim().toLowerCase();
  if (ans !== 'y' && ans !== 'yes') {
    return { enabled: false };
  }

  const existing = readExistingCloudConfig();
  if (existing) {
    console.log(
      `\nFound an existing cloud config at ${CLOUD_CONFIG_PATH}:\n` +
        `  libsql_url: ${existing.libsql_url}\n` +
        `  auth_token: ${maskToken(existing.auth_token)}\n` +
        `  device_id:  ${existing.device_id}\n`,
    );
    const reuse = (await ask(rl, 'Use this existing config? [Y/n]: ')).trim().toLowerCase();
    if (reuse === '' || reuse === 'y' || reuse === 'yes') {
      return { enabled: true, ...existing };
    }
  }

  console.log(
    '\nYou must have already created the Turso database (see the manual\n' +
      'instructions in README.md, section "Multi-device sync"). The installer\n' +
      'will NOT create the database for you — it only stores the credentials.\n',
  );
  let libsql_url = '';
  while (!libsql_url) {
    libsql_url = (await ask(rl, 'libsql_url (libsql://... or https://...): ')).trim();
    if (!libsql_url) {
      fail('libsql_url cannot be empty.');
      continue;
    }
    if (!libsql_url.startsWith('libsql://') && !libsql_url.startsWith('https://')) {
      fail('libsql_url must start with libsql:// or https://');
      libsql_url = '';
    }
  }
  let auth_token = '';
  while (!auth_token) {
    auth_token = (await ask(rl, 'auth_token: ')).trim();
    if (!auth_token) fail('auth_token cannot be empty.');
  }
  let device_id = '';
  while (!device_id) {
    device_id = (await ask(rl, 'device_id (e.g. luka-laptop): ')).trim();
    if (!device_id) fail('device_id cannot be empty.');
  }
  return { enabled: true, libsql_url, auth_token, device_id };
}

function writeCloudConfig(cloud) {
  fs.mkdirSync(DATA_DIR, { recursive: true });
  const body = `${JSON.stringify(
    {
      libsql_url: cloud.libsql_url,
      auth_token: cloud.auth_token,
      device_id: cloud.device_id,
    },
    null,
    2,
  )}\n`;
  // Skip rewriting an identical file to avoid bumping mtime needlessly.
  if (fs.existsSync(CLOUD_CONFIG_PATH)) {
    try {
      if (fs.readFileSync(CLOUD_CONFIG_PATH, 'utf8') === body) {
        return { skipped: true };
      }
    } catch (_) {}
  }
  const tmp = `${CLOUD_CONFIG_PATH}.write-${process.pid}`;
  fs.writeFileSync(tmp, body);
  fs.renameSync(tmp, CLOUD_CONFIG_PATH);
  return { skipped: false };
}

function openInteractiveStdin() {
  // We prefer talking to the controlling terminal directly. That way the
  // `curl ... | node` and `irm ... | node` invocations also work: stdin is the
  // pipe carrying the script, but the user is still at a real terminal we
  // can read from and write to.
  const ttyPath = process.platform === 'win32' ? '\\\\.\\CON' : '/dev/tty';
  try {
    const fd = fs.openSync(ttyPath, 'r+');
    const input = fs.createReadStream('', { fd, autoClose: false });
    const output = fs.createWriteStream('', { fd, autoClose: false });
    const rl = readline.createInterface({ input, output, terminal: true });
    // Just close the readline interface. The fd and the read/write streams
    // wrapping it are intentionally left open — process.exit(0) at the end of
    // main() reaps them. Trying to closeSync(fd) here races with async writes
    // still in the output stream's buffer (EBADF on write).
    const close = () => {
      try { rl.close(); } catch (_) {}
    };
    return { rl, close };
  } catch (e) {
    if (process.stdin.isTTY && process.stdout.isTTY) {
      const rl = readline.createInterface({
        input: process.stdin,
        output: process.stdout,
      });
      return { rl, close: () => rl.close() };
    }
    abort(
      `this installer is interactive and needs a terminal. Cannot open ${ttyPath}: ${e.message}. ` +
        `As a fallback, download it first and run \`node install.js\` directly.`,
    );
  }
}

async function main() {
  info('XClaudeUsage installer');
  const [maj, min] = nodeMajorMinor();
  if (maj < 18) {
    abort(`Node.js >= 18 required (found ${process.version}).`);
  }
  if (maj < 22 || (maj === 22 && min < 5)) {
    info(
      `note: Node ${process.version} works but lacks node:sqlite. The statusline ` +
        `will run in legacy single-session mode. Upgrade to Node 22.5+ for full features.`,
    );
  }

  info(`checking GitHub connectivity...`);
  await checkConnectivity();

  info(`reading ${SETTINGS_PATH}...`);
  const settings = readSettings();

  if (settings.statusLine !== undefined && !isXClaudeStatusline(settings.statusLine)) {
    abort(
      `a non-XClaude statusLine is already configured in ${SETTINGS_PATH}. ` +
        `Refusing to overwrite it. Remove or rename it manually, then re-run.`,
    );
  }

  const tty = openInteractiveStdin();
  let cloud;
  try {
    cloud = await promptCloud(tty.rl);
  } finally {
    tty.close();
  }

  const events = cloud.enabled ? CLOUD_EVENTS : BASE_EVENTS;

  fs.mkdirSync(HOOKS_DIR, { recursive: true });
  const etags = loadETags();
  const downloadResults = {};
  let etagsChanged = false;
  for (const fname of HOOK_FILES) {
    const url = `${REPO_RAW_BASE}/${fname}`;
    const dest = path.join(HOOKS_DIR, fname);
    const result = await downloadFile(url, dest, etags[fname]);
    downloadResults[fname] = result.skipped ? 'up to date' : 'downloaded';
    info(`${fname}: ${downloadResults[fname]}`);
    if (result.etag && result.etag !== etags[fname]) {
      etags[fname] = result.etag;
      etagsChanged = true;
    }
  }
  if (etagsChanged) saveETags(etags);

  let cloudResult = null;
  if (cloud.enabled) {
    cloudResult = writeCloudConfig(cloud);
    info(`cloud config: ${cloudResult.skipped ? 'unchanged' : 'written'}`);
  }

  const statusResult = ensureXClaudeStatusline(settings);
  const hooksResult = ensureXClaudeHooks(settings, events);
  const settingsResult = writeSettingsIfChanged(settings);
  if (settingsResult.changed && settingsResult.backup) {
    info(`backed up settings.json to ${settingsResult.backup}`);
  } else if (!settingsResult.changed) {
    info(`settings.json: unchanged (no backup needed)`);
  }

  console.log('\n[install] done.');
  if (settingsResult.changed) {
    console.log(`  statusLine: ${statusResult.action}`);
    for (const [ev, action] of Object.entries(hooksResult)) {
      console.log(`  hook ${ev}: ${action}`);
    }
  } else {
    console.log(`  settings.json: up to date (no changes)`);
  }
  for (const [fname, action] of Object.entries(downloadResults)) {
    console.log(`  ${fname}: ${action}`);
  }
  console.log(`  cloud sync: ${cloud.enabled ? 'enabled' : 'disabled'}`);
  if (cloudResult) {
    console.log(`  cloud config: ${cloudResult.skipped ? 'up to date' : 'written'}`);
  }
  console.log('\nRestart Claude Code in a fresh session to pick up the changes.');
  console.log(
    'Tip: install right before starting a brand-new session — see the\n' +
      '"First-run tip" in README.md if you are installing mid-session.',
  );
}

main()
  .then(() => process.exit(0))
  .catch((e) => {
    abort(e && e.stack ? e.stack : String(e));
  });
