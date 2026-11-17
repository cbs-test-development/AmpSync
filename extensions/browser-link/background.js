// AmpSync browser link service worker. It only ever handles SharePoint cookies and
// never writes a cookie value to the console.
import { HOST_NAME, PROTOCOL, createDebouncer, detectBrowser, filterCookies, isSharePointHost, probeUrl, validateIncoming } from './lib.js';

const ALARM = 'ampsync-reconnect';
const SILENT_WAIT_MS = 45_000;
const INTERACTIVE_WAIT_MS = 180_000;
const PROBE_TIMEOUT_MS = 15_000;
const PUSH_DEBOUNCE_MS = 2_000;
const MAX_BACKOFF_MS = 30_000;
const MAX_FRAME_BYTES = 256 * 1024;

let port = null;
let appRunning = null; // null until the host answers hello
let backoff = 1_000;
let retryTimer = null;

// host -> wake(), for refreshes waiting on a new FedAuth.
const waiters = new Map();
const pushes = createDebouncer(PUSH_DEBOUNCE_MS);

function log(...args) {
  console.log('[ampsync]', ...args);
}

function send(msg) {
  if (!port) return false;
  if (new TextEncoder().encode(JSON.stringify(msg)).length > MAX_FRAME_BYTES) {
    log('dropping oversized message', msg.type);
    return false;
  }
  try {
    port.postMessage(msg);
    return true;
  } catch (e) {
    log('send failed', e && e.message);
    return false;
  }
}

function connect() {
  if (port) return;
  let p;
  try {
    p = chrome.runtime.connectNative(HOST_NAME);
  } catch (e) {
    log('connectNative threw', e && e.message);
    scheduleRetry();
    return;
  }
  port = p;
  p.onMessage.addListener(onHostMessage);
  p.onDisconnect.addListener(() => {
    const reason = chrome.runtime.lastError && chrome.runtime.lastError.message;
    log('host disconnected', reason || '');
    if (port === p) port = null;
    appRunning = null;
    scheduleRetry();
  });
  const ua = navigator.userAgentData;
  send({
    type: 'hello',
    ext_version: chrome.runtime.getManifest().version,
    browser: detectBrowser(ua && ua.brands, navigator.userAgent),
    protocol: PROTOCOL,
  });
}

// Backs off up to 30 s; the 5-minute alarm is the safety net if the worker was asleep.
function scheduleRetry() {
  if (retryTimer) return;
  retryTimer = setTimeout(() => {
    retryTimer = null;
    connect();
  }, backoff);
  backoff = Math.min(backoff * 2, MAX_BACKOFF_MS);
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

async function readSession(host) {
  const cookies = await chrome.cookies.getAll({ url: `https://${host}/` });
  return filterCookies(host, cookies);
}

// rtFa can land a moment after FedAuth, so give a half-written session a few tries.
async function readSessionSettled(host) {
  for (let i = 0; i < 4; i++) {
    const cookies = await readSession(host);
    if (cookies) return cookies;
    await sleep(500);
  }
  return null;
}

async function pushSession(host, id, reason) {
  const cookies = await readSessionSettled(host);
  if (!cookies) return false;
  log('session', reason, host, cookies.map((c) => c.name).join(','));
  return send({ type: 'session', id, host, reason, cookies });
}

function sendError(id, host, code) {
  send({ type: 'error', id, host, code });
}

chrome.cookies.onChanged.addListener(({ cookie, removed }) => {
  if (removed || cookie.name !== 'FedAuth') return;
  const host = cookie.domain.replace(/^\./, '').toLowerCase();
  if (!isSharePointHost(host)) return;
  const wake = waiters.get(host);
  if (wake) {
    // A refresh is waiting for exactly this and will send the session itself.
    pushes.cancel(host);
    wake();
    return;
  }
  pushes.schedule(host, () => {
    pushSession(host, null, 'changed').catch((e) => log('push failed', e && e.message));
  });
});

async function probe(startUrl) {
  try {
    const res = await fetch(probeUrl(startUrl), {
      credentials: 'include',
      redirect: 'manual',
      headers: { Accept: 'application/json' },
      signal: AbortSignal.timeout(PROBE_TIMEOUT_MS),
    });
    // 403 still means "signed in": guests and people with only a sharing link are often refused
    // the site itself. Signed out shows up as a login redirect (opaque, status 0) or 401.
    return res.status === 200 || res.status === 403;
  } catch {
    return false;
  }
}

function waitForFedAuth(host, ms) {
  return new Promise((resolve) => {
    const wake = () => done(true);
    const done = (ok) => {
      clearTimeout(timer);
      if (waiters.get(host) === wake) waiters.delete(host);
      resolve(ok);
    };
    const timer = setTimeout(() => done(false), ms);
    waiters.set(host, wake);
  });
}

// Open start_url in a tab the user does not have to look at.
async function openTab(url, interactive) {
  const windows = await chrome.windows.getAll({ windowTypes: ['normal'] });
  const target = windows.find((w) => w.focused) || windows[0];
  if (!target) {
    const win = await chrome.windows.create({ url, state: interactive ? 'normal' : 'minimized', focused: interactive });
    return { tabId: win.tabs[0].id, windowId: win.id, ownWindow: true };
  }
  const tab = await chrome.tabs.create({ url, active: interactive, windowId: target.id });
  if (interactive) await chrome.windows.update(target.id, { focused: true });
  return { tabId: tab.id, windowId: target.id, ownWindow: false };
}

async function closeTab(opened) {
  try {
    if (opened.ownWindow) await chrome.windows.remove(opened.windowId);
    else await chrome.tabs.remove(opened.tabId);
  } catch {
    // The user may have closed it already.
  }
}

const refreshing = new Set();

async function handleRefresh({ id, host, start_url, interactive }) {
  if (refreshing.has(host)) return sendError(id, host, 'bad_request');
  refreshing.add(host);
  try {
    if (!(await chrome.permissions.contains({ origins: [`https://${host}/*`] }))) {
      return sendError(id, host, 'no_permission');
    }
    if ((await probe(start_url)) && (await pushSession(host, id, 'requested'))) return;

    const opened = await openTab(start_url, interactive);
    const ok = await waitForFedAuth(host, interactive ? INTERACTIVE_WAIT_MS : SILENT_WAIT_MS);
    if (ok) {
      const sent = await pushSession(host, id, 'requested');
      await closeTab(opened);
      if (!sent) sendError(id, host, 'signin_required');
    } else {
      // Leave an interactive tab open so the user can carry on; a silent one is just closed.
      if (!interactive) await closeTab(opened);
      sendError(id, host, interactive ? 'timeout' : 'signin_required');
    }
  } catch (e) {
    log('refresh failed', host, e && e.message);
    sendError(id, host, 'signin_required');
  } finally {
    refreshing.delete(host);
  }
}

function onHostMessage(raw) {
  const msg = validateIncoming(raw);
  if (!msg) return;
  switch (msg.type) {
    case 'ping':
      send({ type: 'pong', id: msg.id });
      break;
    case 'app':
      appRunning = msg.running;
      if (msg.running) backoff = 1_000;
      break;
    case 'refresh':
      handleRefresh(msg);
      break;
  }
}

// The popup only reads this; nothing else in the extension talks to the worker.
chrome.runtime.onMessage.addListener((msg, sender, reply) => {
  if (sender.id !== chrome.runtime.id || !msg || msg.type !== 'status') return;
  reply({ connected: port !== null && appRunning === true });
});

chrome.alarms.create(ALARM, { periodInMinutes: 5 });
chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === ALARM && !port) connect();
});
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);

connect();
