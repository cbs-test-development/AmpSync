import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

import {
  HOST_NAME,
  PROTOCOL,
  MAX_COOKIES,
  MAX_VALUE_BYTES,
  createDebouncer,
  detectBrowser,
  filterCookies,
  isSharePointHost,
  probeUrl,
  siteRoot,
  validateIncoming,
} from '../lib.js';

const HOST = 'contoso.sharepoint.com';
const ck = (name, domain, extra = {}) => ({ name, value: `v-${name}`, domain, path: '/', ...extra });

test('constants match the spec', () => {
  assert.equal(HOST_NAME, 'io.github.ampsync.browserlink');
  assert.equal(PROTOCOL, 1);
});

test('manifest key hashes to the fixed extension ID', () => {
  const manifest = JSON.parse(readFileSync(new URL('../manifest.json', import.meta.url), 'utf8'));
  const digest = createHash('sha256').update(Buffer.from(manifest.key, 'base64')).digest().subarray(0, 16);
  const id = [...digest].map((b) => String.fromCharCode(97 + (b >> 4)) + String.fromCharCode(97 + (b & 15))).join('');
  assert.equal(id, 'foohiaphdliphaogfjlahdmbjemmaocp');
});

test('isSharePointHost accepts tenants and bare suffixes in every region', () => {
  for (const h of ['contoso.sharepoint.com', 'contoso-my.sharepoint.com', 'a.sharepoint.us', 'a.sharepoint.cn', 'a.sharepoint.de', 'sharepoint.com', 'CONTOSO.SharePoint.com']) {
    assert.equal(isSharePointHost(h), true, h);
  }
});

test('isSharePointHost rejects lookalikes and junk', () => {
  for (const h of [
    'evilsharepoint.com',
    'sharepoint.com.evil.net',
    'contoso.sharepoint.org',
    'sharepoint.co',
    'example.com',
    'contoso.sharepoint.com/',
    'contoso.sharepoint.com:8443',
    'user@contoso.sharepoint.com',
    '.sharepoint.com',
    '',
    'localhost',
  ]) {
    assert.equal(isSharePointHost(h), false, h);
  }
  for (const h of [null, undefined, 42, {}, ['contoso.sharepoint.com']]) assert.equal(isSharePointHost(h), false);
});

test('filterCookies keeps host cookies and the parent-domain rtFa', () => {
  const out = filterCookies(HOST, [
    ck('FedAuth', HOST, { expirationDate: 1763300000.9 }),
    ck('rtFa', '.sharepoint.com', { expirationDate: 1763300100 }),
    ck('other', HOST),
  ]);
  assert.deepEqual(out.map((c) => [c.name, c.domain]).sort(), [['FedAuth', HOST], ['other', HOST], ['rtFa', 'sharepoint.com']]);
  assert.equal(out.find((c) => c.name === 'FedAuth').expires, 1763300000);
  assert.equal(out.find((c) => c.name === 'rtFa').expires, 1763300100);
});

test('filterCookies strips the leading dot and lower-cases the domain', () => {
  const out = filterCookies(HOST, [ck('FedAuth', '.Contoso.SharePoint.com'), ck('rtFa', '.sharepoint.com')]);
  assert.deepEqual(out.map((c) => c.domain).sort(), ['contoso.sharepoint.com', 'sharepoint.com']);
});

test('filterCookies gives null for session cookies expiry', () => {
  const out = filterCookies(HOST, [ck('FedAuth', HOST, { session: true }), ck('rtFa', '.sharepoint.com')]);
  assert.equal(out[0].expires, null);
  assert.equal(out[1].expires, null);
});

test('filterCookies drops foreign domains, other tenants and other suffixes', () => {
  const out = filterCookies(HOST, [
    ck('FedAuth', HOST),
    ck('rtFa', 'sharepoint.com'),
    ck('x1', 'fabrikam.sharepoint.com'),
    ck('x2', 'evil.com'),
    ck('x3', '.login.microsoftonline.com'),
    ck('x4', 'sharepoint.us'),
    ck('x5', 'sub.contoso.sharepoint.com'),
  ]);
  assert.deepEqual(out.map((c) => c.name).sort(), ['FedAuth', 'rtFa']);
});

test('filterCookies returns null when FedAuth or rtFa is missing', () => {
  assert.equal(filterCookies(HOST, [ck('rtFa', '.sharepoint.com')]), null);
  assert.equal(filterCookies(HOST, [ck('FedAuth', HOST)]), null);
  assert.equal(filterCookies(HOST, [ck('FedAuth', 'evil.com'), ck('rtFa', '.sharepoint.com')]), null);
  assert.equal(filterCookies(HOST, []), null);
});

test('filterCookies returns null for a bad host or bad input', () => {
  const good = [ck('FedAuth', HOST), ck('rtFa', '.sharepoint.com')];
  assert.equal(filterCookies('evil.com', good), null);
  assert.equal(filterCookies(HOST, null), null);
  assert.equal(filterCookies(HOST, [null, 5, {}]), null);
});

test('filterCookies drops values over 16 KB but keeps exactly 16 KB', () => {
  const big = 'a'.repeat(MAX_VALUE_BYTES + 1);
  const edge = 'a'.repeat(MAX_VALUE_BYTES);
  assert.equal(filterCookies(HOST, [{ ...ck('FedAuth', HOST), value: big }, ck('rtFa', '.sharepoint.com')]), null);
  const out = filterCookies(HOST, [{ ...ck('FedAuth', HOST), value: edge }, ck('rtFa', '.sharepoint.com'), { ...ck('junk', HOST), value: big }]);
  assert.deepEqual(out.map((c) => c.name).sort(), ['FedAuth', 'rtFa']);
});

test('filterCookies counts bytes, not characters', () => {
  const wide = 'é'.repeat(MAX_VALUE_BYTES / 2 + 1); // 2 bytes each
  assert.equal(filterCookies(HOST, [{ ...ck('FedAuth', HOST), value: wide }, ck('rtFa', '.sharepoint.com')]), null);
});

test('filterCookies caps the list at 64 and keeps the required cookies', () => {
  const many = Array.from({ length: 100 }, (_, i) => ck(`c${i}`, HOST));
  const out = filterCookies(HOST, [...many, ck('FedAuth', HOST), ck('rtFa', '.sharepoint.com')]);
  assert.equal(out.length, MAX_COOKIES);
  assert.ok(out.some((c) => c.name === 'FedAuth'));
  assert.ok(out.some((c) => c.name === 'rtFa'));
});

test('validateIncoming accepts ping and app', () => {
  assert.deepEqual(validateIncoming({ type: 'ping', id: 'p7', extra: 1 }), { type: 'ping', id: 'p7' });
  assert.deepEqual(validateIncoming({ type: 'app', running: false }), { type: 'app', running: false });
});

test('validateIncoming rejects bad ping and app', () => {
  assert.equal(validateIncoming({ type: 'ping' }), null);
  assert.equal(validateIncoming({ type: 'ping', id: '' }), null);
  assert.equal(validateIncoming({ type: 'ping', id: 'x'.repeat(65) }), null);
  assert.equal(validateIncoming({ type: 'ping', id: 7 }), null);
  assert.equal(validateIncoming({ type: 'app', running: 'yes' }), null);
  assert.equal(validateIncoming({ type: 'app' }), null);
});

test('validateIncoming accepts a refresh and normalises it', () => {
  const out = validateIncoming({ type: 'refresh', id: 'r42', host: 'Contoso.SharePoint.com', start_url: 'https://contoso.sharepoint.com/sites/X', interactive: true });
  assert.deepEqual(out, { type: 'refresh', id: 'r42', host: HOST, start_url: 'https://contoso.sharepoint.com/sites/X', interactive: true });
  const silent = validateIncoming({ type: 'refresh', id: 'r1', host: HOST, start_url: `https://${HOST}/` });
  assert.equal(silent.interactive, false);
});

test('validateIncoming rejects refresh with a bad host, URL or flag', () => {
  const base = { type: 'refresh', id: 'r1', host: HOST, start_url: `https://${HOST}/sites/X`, interactive: false };
  assert.ok(validateIncoming(base));
  assert.equal(validateIncoming({ ...base, id: undefined }), null);
  assert.equal(validateIncoming({ ...base, host: 'evilsharepoint.com', start_url: 'https://evilsharepoint.com/' }), null);
  assert.equal(validateIncoming({ ...base, start_url: 'https://evil.com/' }), null);
  assert.equal(validateIncoming({ ...base, start_url: `http://${HOST}/` }), null);
  assert.equal(validateIncoming({ ...base, start_url: `https://${HOST}.evil.com/` }), null);
  assert.equal(validateIncoming({ ...base, start_url: 'javascript:alert(1)' }), null);
  assert.equal(validateIncoming({ ...base, start_url: 'not a url' }), null);
  assert.equal(validateIncoming({ ...base, start_url: 5 }), null);
  assert.equal(validateIncoming({ ...base, interactive: 'true' }), null);
});

test('validateIncoming ignores unknown types and non-objects', () => {
  for (const m of [{ type: 'future' }, { type: 'hello' }, {}, null, undefined, 'ping', 5, [], [{ type: 'ping', id: 'a' }]]) {
    assert.equal(validateIncoming(m), null);
  }
});

test('createDebouncer fires once per key after the last call', () => {
  const queue = [];
  let nextId = 1;
  const timers = {
    setTimeout: (fn) => {
      const id = nextId++;
      queue.push({ id, fn });
      return id;
    },
    clearTimeout: (id) => {
      const i = queue.findIndex((t) => t.id === id);
      if (i >= 0) queue.splice(i, 1);
    },
  };
  const d = createDebouncer(2000, timers);
  const calls = [];
  d.schedule('a', () => calls.push('a1'));
  d.schedule('a', () => calls.push('a2'));
  d.schedule('b', () => calls.push('b'));
  assert.equal(queue.length, 2);
  queue.splice(0).forEach((t) => t.fn());
  assert.deepEqual(calls, ['a2', 'b']);
  d.schedule('a', () => calls.push('a3'));
  d.cancel('a');
  assert.equal(queue.length, 0);
});

test('createDebouncer works with real timers', async () => {
  const d = createDebouncer(10);
  let n = 0;
  d.schedule('k', () => n++);
  d.schedule('k', () => n++);
  await new Promise((r) => setTimeout(r, 40));
  assert.equal(n, 1);
});

test('siteRoot picks /sites, /teams and /personal, else the origin', () => {
  assert.equal(siteRoot('https://contoso.sharepoint.com/sites/X/Shared%20Documents/a'), 'https://contoso.sharepoint.com/sites/X');
  assert.equal(siteRoot('https://contoso.sharepoint.com/teams/Y'), 'https://contoso.sharepoint.com/teams/Y');
  assert.equal(siteRoot('https://contoso-my.sharepoint.com/personal/jo_contoso_com/Documents'), 'https://contoso-my.sharepoint.com/personal/jo_contoso_com');
  assert.equal(siteRoot('https://contoso.sharepoint.com/'), 'https://contoso.sharepoint.com');
  assert.equal(siteRoot('https://contoso.sharepoint.com/Shared%20Documents/x'), 'https://contoso.sharepoint.com');
  assert.equal(siteRoot('https://contoso.sharepoint.com/sites'), 'https://contoso.sharepoint.com');
  assert.equal(siteRoot('https://contoso.sharepoint.com/Sites/X?a=1#b'), 'https://contoso.sharepoint.com/Sites/X');
});

test('probeUrl appends the web query to the site root', () => {
  assert.equal(probeUrl('https://contoso.sharepoint.com/sites/X/Lib'), 'https://contoso.sharepoint.com/sites/X/_api/web?$select=Title');
  assert.equal(probeUrl('https://contoso.sharepoint.com'), 'https://contoso.sharepoint.com/_api/web?$select=Title');
  assert.equal(probeUrl('https://contoso-my.sharepoint.com/personal/jo/Documents'), 'https://contoso-my.sharepoint.com/personal/jo/_api/web?$select=Title');
});

test('detectBrowser prefers the Edge brand, then the UA, else chrome', () => {
  assert.equal(detectBrowser([{ brand: 'Chromium' }, { brand: 'Microsoft Edge' }], ''), 'edge');
  assert.equal(detectBrowser(undefined, 'Mozilla/5.0 Chrome/120 Safari/537.36 Edg/120.0'), 'edge');
  assert.equal(detectBrowser([{ brand: 'Google Chrome' }], 'Mozilla/5.0 Chrome/120 Safari/537.36'), 'chrome');
  assert.equal(detectBrowser(undefined, undefined), 'chrome');
});
