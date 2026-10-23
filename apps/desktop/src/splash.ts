// The startup splash (F7, D25): drives the markup in index.html. Runs before the app bundle.
//
// The main window is created hidden. This script decides how the splash plays (the setting, and
// the OS reduce-motion preference), waits for its first frame, asks the backend to show the
// window, and then lets it play: at least 3 s, at most 6 s, leaving as soon as the app is ready
// and the launch-time sign-in check has answered. A click or any key skips it.
import { invoke } from "@tauri-apps/api/core";
import {
  READY_EVENT,
  creditParts,
  faceInitials,
  playback,
  shouldLeave,
  skipsSplash,
  statusAt,
  type SplashInfo,
} from "./core/splash";

let frontendReady = false;
let markReady!: () => void;
const ready = new Promise<void>((resolve) => (markReady = resolve));
window.addEventListener(READY_EVENT, () => {
  frontendReady = true;
  markReady();
});

const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

async function showWindow() {
  try {
    await invoke("show_window");
  } catch {
    /* the backend shows the window by itself after a few seconds */
  }
}

function applyTheme() {
  try {
    const t = localStorage.getItem("ampsync-theme");
    if (t === "dark" || t === "light") document.documentElement.setAttribute("data-theme", t);
  } catch {
    /* no storage: follow the system */
  }
}

/** Fonts (briefly) and two frames, so the window opens on a finished first picture. */
async function firstFrame() {
  const fonts = document.fonts
    ? Promise.all([
        document.fonts.load('600 40px "IBM Plex Sans Condensed"'),
        document.fonts.load('12px "IBM Plex Mono"'),
      ]).catch(() => undefined)
    : Promise.resolve();
  await Promise.race([fonts, delay(300)]);
  // A hidden window may not run animation frames; do not wait for them for long.
  await Promise.race([new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r()))), delay(250)]);
}

function remove(splash: HTMLElement) {
  splash.classList.add("hidden");
  splash.remove();
  document.getElementById("app")?.removeAttribute("inert");
}

function fillFooter(info: SplashInfo) {
  const version = document.getElementById("sp-version");
  if (version) version.textContent = `v${info.version}`;
  const parts = creditParts(info.credit);
  const credit = document.getElementById("sp-credit");
  if (!parts || !credit || !info.credit) return;
  const faces = document.getElementById("sp-faces");
  for (const p of info.credit.people) {
    const tile = document.createElement("b");
    if (p.avatar) {
      const img = document.createElement("img");
      img.alt = "";
      img.src = p.avatar;
      tile.append(img);
    } else {
      tile.textContent = faceInitials(p.login);
    }
    faces?.append(tile);
  }
  const name = document.getElementById("sp-name");
  const others = document.getElementById("sp-others");
  if (name) name.textContent = parts.name;
  if (others) others.textContent = parts.others;
  credit.hidden = false;
}

async function run() {
  const splash = document.getElementById("splash");
  if (!splash) return;
  applyTheme();
  // Plain browser (npm run dev without Tauri): nothing to show the window for.
  if (!("__TAURI_INTERNALS__" in window)) {
    remove(splash);
    return;
  }

  let info: SplashInfo | null = null;
  try {
    info = await Promise.race([invoke<SplashInfo>("splash_info"), delay(2000).then(() => null)]);
  } catch {
    info = null;
  }
  const how = info ? playback(info.mode, matchMedia("(prefers-reduced-motion: reduce)").matches) : "none";
  if (!info || how === "none") {
    // No animation: the window shows as soon as the app is ready (or the backend's timer fires).
    remove(splash);
    await ready;
    await showWindow();
    return;
  }

  const calm = how === "calm";
  const state = info;
  const appRoot = document.getElementById("app");
  appRoot?.setAttribute("inert", "");
  splash.classList.add("live");
  if (calm) splash.classList.add("calm");
  fillFooter(state);
  const statusEl = document.getElementById("sp-status");
  const setStatus = (text: string) => {
    if (statusEl && statusEl.textContent !== text) statusEl.textContent = text;
  };
  setStatus(statusAt(0, state, state.checked, calm));

  await firstFrame();
  await showWindow();
  const t0 = performance.now();
  const elapsed = () => performance.now() - t0;
  splash.classList.add("play");

  let answered = state.checked;
  let leaving = false;
  let ticker: ReturnType<typeof setInterval> | undefined;
  let poller: ReturnType<typeof setInterval> | undefined;

  const leave = () => {
    if (leaving) return;
    leaving = true;
    clearInterval(ticker);
    clearInterval(poller);
    splash.classList.remove("wait");
    splash.classList.add("lift");
    appRoot?.removeAttribute("inert");
    splash.addEventListener("animationend", (e) => {
      if (e.target === splash) remove(splash);
    });
    setTimeout(() => remove(splash), 1000);
  };

  const tick = () => {
    if (leaving) return;
    const t = elapsed();
    setStatus(statusAt(t, state, answered, calm));
    if (shouldLeave(t, frontendReady, answered)) leave();
    else if (!calm && t >= 2900) splash.classList.add("wait");
  };

  ticker = setInterval(tick, 100);
  // The check is asked about, not listened for: no event can be missed by a late listener.
  poller = setInterval(async () => {
    try {
      const now = await invoke<SplashInfo>("splash_info");
      state.queued = now.queued;
      state.paused = now.paused;
      state.sites = now.sites;
      answered = now.checked;
      if (answered) clearInterval(poller);
    } catch {
      /* try again on the next beat */
    }
  }, 250);
  void ready.then(() => {
    void showWindow();
    tick();
  });

  splash.addEventListener("click", leave);
  window.addEventListener("keydown", (e) => {
    if (skipsSplash(e.key)) leave();
  });
}

run().catch((e) => {
  // Whatever went wrong, the splash must never cover the app or keep the window hidden.
  console.error("startup splash failed", e);
  const splash = document.getElementById("splash");
  if (splash) remove(splash);
  void showWindow();
});
