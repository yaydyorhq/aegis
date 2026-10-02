import { create } from "zustand";
import { ipc } from "../lib/ipc";
import type { MintTaskRow } from "../lib/types";
import { notifyOs, playChime } from "../lib/notify";
import { pushToast } from "../components/ui";
import { shortAddress } from "../lib/utils";

/**
 * App-wide mint task feed. One 4s poll (started from AppShell) keeps the task
 * list warm and fires status-change toasts no matter which page the user is
 * on — the previous per-page poll went silent the moment a mint confirmed
 * while the user was looking at the Dashboard.
 */

interface MintTaskFeed {
  tasks: MintTaskRow[];
  refresh: () => Promise<void>;
}

const seenIds = new Set<number>();
const seenStatuses = new Map<number, string>();
let primed = false;
let suppressNewToast = false;
let timer: number | null = null;

function applyNotifications(next: MintTaskRow[]): void {
  const statusHits: { title: string; detail: string; tone: "ok" | "error" }[] =
    [];
  const newIds: number[] = [];
  for (const t of next) {
    if (!primed) {
      // First load after app start — seed history without toasting it.
      seenIds.add(t.id);
      seenStatuses.set(t.id, t.status);
      continue;
    }
    if (!seenIds.has(t.id)) {
      seenIds.add(t.id);
      seenStatuses.set(t.id, t.status);
      if (!suppressNewToast) newIds.push(t.id);
    } else {
      const prev = seenStatuses.get(t.id);
      if (prev && prev !== t.status) {
        seenStatuses.set(t.id, t.status);
        if (t.status === "confirmed" || t.status === "simulated") {
          statusHits.push({
            title: `Mint #${t.id} ${t.status}`,
            detail: t.tx_hash
              ? shortAddress(t.tx_hash, 8)
              : shortAddress(t.contract, 6),
            tone: "ok",
          });
        } else if (
          t.status === "failed" ||
          t.status === "canceled" ||
          t.status === "cancelled"
        ) {
          statusHits.push({
            title: `Mint #${t.id} ${t.status}`,
            detail: (t.error || t.status).slice(0, 140),
            tone: "error",
          });
        }
      }
    }
  }
  primed = true;
  suppressNewToast = false;
  if (newIds.length > 0) {
    const list = newIds.map((id) => `#${id}`).join(", ");
    const row = next.find((t) => t.id === newIds[0]);
    pushToast(
      newIds.length === 1
        ? `New mint task ${list}`
        : `New mint tasks ${list}`,
      "info",
      row ? shortAddress(row.contract, 6) : undefined,
    );
  }
  for (const hit of statusHits) {
    pushToast(hit.title, hit.tone, hit.detail);
    // Mint results deserve to reach you even when the window is minimized —
    // an OS notification while hidden (the toast already covers the visible
    // case), and a chime either way. Both behind the Settings toggles.
    if (document.hidden) void notifyOs(hit.title, hit.detail);
    playChime(hit.tone === "ok");
  }
}

export const useMintTaskFeed = create<MintTaskFeed>((set) => ({
  tasks: [],
  refresh: async () => {
    try {
      const t = await ipc<MintTaskRow[]>("mint_list");
      applyNotifications(t);
      set({ tasks: t });
    } catch {
      /* keep the last known list on transient errors */
    }
  },
}));

/** The page that just enqueued already toasted — skip the duplicate. */
export function suppressNextNewTaskToast(): void {
  suppressNewToast = true;
}

/** Idempotent — safe under StrictMode double-effects. */
export function startMintTaskFeed(): void {
  if (timer != null) return;
  void useMintTaskFeed.getState().refresh();
  timer = window.setInterval(() => {
    void useMintTaskFeed.getState().refresh();
  }, 4000);
}
