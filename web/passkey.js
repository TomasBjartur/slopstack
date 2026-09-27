// Passkey ceremonies (vanilla JS): the log-in and sign-up pages' buttons,
// and login() for the log-in dialog every page has (web/app.js).
const enc = (buf) => { const b = new Uint8Array(buf); let s = ''; for (let i = 0; i < b.length; i++) s += String.fromCharCode(b[i]); return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, ''); };
const dec = (s) => { const t = atob(s.replace(/-/g, '+').replace(/_/g, '/')); const b = new Uint8Array(t.length); for (let i = 0; i < t.length; i++) b[i] = t.charCodeAt(i); return b; };
const post = (url, data) => fetch(url, { method: 'POST', headers: { 'Content-Type': 'application/x-www-form-urlencoded' }, body: new URLSearchParams(data), credentials: 'same-origin' });

export const supported = () => !!window.PublicKeyCredential;

// Where to go after signing in: the page's ?next= if it is a path on this
// site (not another site, not back to logging in), else where the server
// says (the dashboard).
export function destination(fallback) {
  const next = new URLSearchParams(location.search).get('next') || '';
  // (Not "//host" nor "/\\host": browsers read a backslash as a slash.)
  if (/^\/[^/\\]/.test(next) && !next.includes("\\") && !/^\/(login|signup|verify|recover)\b/.test(next)) return next;
  return fallback;
}

// Logs in with a passkey: answers the server's next page (throws with a
// message a person can read).
export async function login() {
  const r = await post('/passkey/login/options', {});
  if (!r.ok) throw new Error('The server is busy. Try again in a moment.');
  const o = await r.json();
  let cred;
  try {
    cred = await navigator.credentials.get({ publicKey: { challenge: dec(o.challenge), rpId: o.rpId, userVerification: 'required', timeout: 120000 } });
  } catch (e) {
    throw new Error('Cancelled, or no passkey for this site on this device.');
  }
  const res = await post('/passkey/login', { id: enc(cred.rawId), cd: enc(cred.response.clientDataJSON), ad: enc(cred.response.authenticatorData), sig: enc(cred.response.signature) });
  if (!res.ok) throw new Error('That passkey was not accepted. Try again, or get a link to add a new one.');
  return res.text();
}

async function register(t) {
  const r = await post('/passkey/register/options', { t });
  if (!r.ok) throw new Error('This link has expired. Ask for a new one.');
  const o = await r.json();
  let cred;
  try {
    cred = await navigator.credentials.create({ publicKey: { challenge: dec(o.challenge), rp: { id: o.rpId, name: o.rpName }, user: { id: dec(o.userId), name: o.userName, displayName: o.userDisplay }, pubKeyCredParams: [{ type: 'public-key', alg: -7 }], authenticatorSelection: { residentKey: 'required', userVerification: 'required' }, attestation: 'none', timeout: 120000 } });
  } catch (e) {
    throw new Error('Cancelled or not supported on this device.');
  }
  const res = await post('/passkey/register', { t, cd: enc(cred.response.clientDataJSON), att: enc(cred.response.attestationObject) });
  if (!res.ok) throw new Error('That did not work. Please try again.');
  return res.text();
}

// A page's button: the ceremony with the button disabled (a double click
// cannot start two), its progress in the status line, then on.
export function bind(btn, statusEl, ceremony, onDone) {
  const status = (m) => { if (statusEl) statusEl.textContent = m; };
  btn.addEventListener('click', async () => {
    if (!supported()) { status('This browser does not support passkeys. Try a recent Chrome, Safari, Firefox or Edge.'); return; }
    btn.disabled = true;
    status('Waiting for your device…');
    try {
      const to = await ceremony();
      status('Done. Taking you in…');
      onDone(to);
    } catch (e) {
      status(e.message);
      btn.disabled = false;
    }
  });
}

const statusEl = document.getElementById('passkey-status');
const reg = document.getElementById('passkey-register');
if (reg) bind(reg, statusEl, () => register(reg.dataset.token), (to) => location.assign(destination(to)));
const loginBtn = document.getElementById('passkey-login');
if (loginBtn) bind(loginBtn, statusEl, login, (to) => location.assign(destination(to)));
