/**
 * OS notifications + mint-result chime for the global task feed.
 *
 * OS notifications fire only while the window is hidden/minimized — when
 * you're looking at the app, the toast already covers it. The chime plays on
 * mint results regardless (snipers want to hear it land), behind the same
 * Settings toggles. Preferences are localStorage-only: they don't need the
 * backend or a pre-paint apply, unlike the theme.
 */

import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

const NOTIFY_OS_KEY = "aegis.notify_os";
const NOTIFY_SOUND_KEY = "aegis.notify_sound";

export function notifyOsEnabled(): boolean {
  try {
    return localStorage.getItem(NOTIFY_OS_KEY) !== "0";
  } catch {
    return true;
  }
}

export function setNotifyOsEnabled(on: boolean): void {
  try {
    localStorage.setItem(NOTIFY_OS_KEY, on ? "1" : "0");
  } catch {
    /* storage unavailable */
  }
}

export function notifySoundEnabled(): boolean {
  try {
    return localStorage.getItem(NOTIFY_SOUND_KEY) !== "0";
  } catch {
    return true;
  }
}

export function setNotifySoundEnabled(on: boolean): void {
  try {
    localStorage.setItem(NOTIFY_SOUND_KEY, on ? "1" : "0");
  } catch {
    /* storage unavailable */
  }
}

/** Fire an OS notification — silently no-ops when disabled/unavailable. */
export async function notifyOs(title: string, body: string): Promise<void> {
  if (!notifyOsEnabled()) return;
  try {
    let granted = await isPermissionGranted();
    if (!granted) {
      granted = (await requestPermission()) === "granted";
    }
    if (granted) {
      sendNotification({ title, body });
    }
  } catch {
    /* plugin unavailable (e.g. tests / unsupported webview) */
  }
}

let audioCtx: AudioContext | null = null;

/** Two-tone chime: rising pair on success, falling pair on failure. */
export function playChime(ok: boolean): void {
  if (!notifySoundEnabled()) return;
  try {
    audioCtx ??= new AudioContext();
    const ctx = audioCtx;
    if (ctx.state === "suspended") void ctx.resume();
    const now = ctx.currentTime;
    const tones = ok ? [880, 1318.5] : [330, 220];
    tones.forEach((freq, i) => {
      const start = now + i * 0.16;
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "sine";
      osc.frequency.value = freq;
      gain.gain.setValueAtTime(0.0001, start);
      gain.gain.exponentialRampToValueAtTime(0.1, start + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.0001, start + 0.28);
      osc.connect(gain).connect(ctx.destination);
      osc.start(start);
      osc.stop(start + 0.3);
    });
  } catch {
    /* audio unavailable */
  }
}
