// Pure helpers for the AmpSync browser link. No chrome.* calls here, so they run under node --test.

export const HOST_NAME = 'io.github.ampsync.browserlink';
export const PROTOCOL = 1;

// Keep in step with SHAREPOINT_SUFFIXES in crates/ampsync-core (link module).
export const SHAREPOINT_SUFFIXES = ['sharepoint.com', 'sharepoint.us', 'sharepoint.cn', 'sharepoint.de'];

export const MAX_COOKIES = 64;
export const MAX_VALUE_BYTES = 16 * 1024;
export const REQUIRED_COOKIES = ['FedAuth', 'rtFa'];

const HOST_RE = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/;
const MAX_ID_LENGTH = 64;
const encoder = new TextEncoder();

// The suffix a host belongs to, or null. Matching is on whole labels, so
// "evilsharepoint.com" and "sharepoint.com.evil.net" both fail.
function suffixOf(host) {
  for (const suffix of SHAREPOINT_SUFFIXES) {
    if (host === suffix || host.endsWith('.' + suffix)) return suffix;
  }
  return null;
}

export function isSharePointHost(host) {
  if (typeof host !== 'string' || host.length > 253) return false;
  const h = host.toLowerCase();
  return HOST_RE.test(h) && suffixOf(h) !== null;
}

// Turn Chrome's cookie list for `host` into protocol cookies, or null when the
// session is not complete (FedAuth and rtFa are both needed to talk to SharePoint).
export function filterCookies(host, chromeCookies) {
  if (!isSharePointHost(host) || !Array.isArray(chromeCookies)) return null;
  const h = host.toLowerCase();
  const suffix = suffixOf(h);
  const kept = [];
  for (const c of chromeCookies) {
    if (!c || typeof c.name !== 'string' || typeof c.value !== 'string' || typeof c.domain !== 'string') continue;
    const domain = c.domain.replace(/^\./, '').toLowerCase();
    // rtFa lives on the bare suffix domain; anything else (other tenants, other sites) is not ours to send.
    if (domain !== h && domain !== suffix) continue;
    if (encoder.encode(c.value).length > MAX_VALUE_BYTES) continue;
    kept.push({
      name: c.name,
      value: c.value,
      domain,
      path: typeof c.path === 'string' && c.path ? c.path : '/',
      expires: typeof c.expirationDate === 'number' && Number.isFinite(c.expirationDate) ? Math.floor(c.expirationDate) : null,
    });
  }
  for (const name of REQUIRED_COOKIES) {
    if (!kept.some((c) => c.name === name)) return null;
  }
  // If the cap bites, the two required cookies must survive it.
  kept.sort((a, b) => Number(REQUIRED_COOKIES.includes(b.name)) - Number(REQUIRED_COOKIES.includes(a.name)));
  return kept.slice(0, MAX_COOKIES);
}

function validId(id) {
  return typeof id === 'string' && id.length > 0 && id.length <= MAX_ID_LENGTH;
}

// Everything from the native host is untrusted. Returns a clean copy of a message
// we understand, or null for anything else (unknown types are ignored on purpose).
export function validateIncoming(msg) {
  if (!msg || typeof msg !== 'object' || Array.isArray(msg)) return null;
  switch (msg.type) {
    case 'ping':
      return validId(msg.id) ? { type: 'ping', id: msg.id } : null;
    case 'app':
      return typeof msg.running === 'boolean' ? { type: 'app', running: msg.running } : null;
    case 'refresh': {
      if (!validId(msg.id) || !isSharePointHost(msg.host) || typeof msg.start_url !== 'string') return null;
      const host = msg.host.toLowerCase();
      let url;
      try {
        url = new URL(msg.start_url);
      } catch {
        return null;
      }
      // The tab we open must be on the host we were asked about.
      if (url.protocol !== 'https:' || url.hostname !== host) return null;
      if (msg.interactive !== undefined && typeof msg.interactive !== 'boolean') return null;
      return { type: 'refresh', id: msg.id, host, start_url: url.href, interactive: msg.interactive === true };
    }
    default:
      return null;
  }
}

// Calls fn once, `ms` after the last schedule() for the same key.
export function createDebouncer(ms, timers = { setTimeout, clearTimeout }) {
  const pending = new Map();
  return {
    schedule(key, fn) {
      if (pending.has(key)) timers.clearTimeout(pending.get(key));
      pending.set(
        key,
        timers.setTimeout(() => {
          pending.delete(key);
          fn();
        }, ms),
      );
    },
    cancel(key) {
      if (pending.has(key)) timers.clearTimeout(pending.get(key));
      pending.delete(key);
    },
  };
}

// The site root is /sites/X, /teams/X or /personal/X on the start URL, else the origin.
export function siteRoot(startUrl) {
  const url = new URL(startUrl);
  const parts = url.pathname.split('/').filter(Boolean);
  if (parts.length >= 2 && ['sites', 'teams', 'personal'].includes(parts[0].toLowerCase())) {
    return `${url.origin}/${parts[0]}/${parts[1]}`;
  }
  return url.origin;
}

// A cheap authenticated call: 200 only if the session is live.
export function probeUrl(startUrl) {
  return `${siteRoot(startUrl)}/_api/web?$select=Title`;
}

export function detectBrowser(brands, userAgent) {
  const hasEdgeBrand = Array.isArray(brands) && brands.some((b) => b && b.brand === 'Microsoft Edge');
  return hasEdgeBrand || (typeof userAgent === 'string' && userAgent.includes('Edg/')) ? 'edge' : 'chrome';
}
