import { create } from "zustand";
import { ipc } from "../lib/ipc";
import type { VaultStatus, WalletGroup, WalletRow } from "../lib/types";

// ── Theme (system / light / dark) ───────────────────────────────────
// Canonical value lives in the backend `meta` table; localStorage mirrors it
// so the right palette applies before the first paint (no dark→light flash).

export type ThemePref = "system" | "light" | "dark";
const THEME_MIRROR_KEY = "aegis.theme";
const lightQuery = window.matchMedia("(prefers-color-scheme: light)");

function resolveTheme(pref: ThemePref): "light" | "dark" {
  return pref === "system" ? (lightQuery.matches ? "light" : "dark") : pref;
}

export function applyTheme(pref: ThemePref): void {
  const resolved = resolveTheme(pref);
  document.documentElement.classList.toggle("light", resolved === "light");
  document.documentElement.classList.toggle("dark", resolved === "dark");
}

/** Apply the stored preference synchronously — call before React renders. */
export function initTheme(): ThemePref {
  let pref: ThemePref = "system";
  try {
    const stored = localStorage.getItem(THEME_MIRROR_KEY);
    if (stored === "light" || stored === "dark" || stored === "system") {
      pref = stored;
    }
  } catch {
    /* storage unavailable — system default */
  }
  applyTheme(pref);
  lightQuery.addEventListener("change", () => {
    if (useAppStore.getState().theme === "system") applyTheme("system");
  });
  return pref;
}

interface VaultState {
  status: VaultStatus | null;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  unlock: (pass: string) => Promise<void>;
  lock: () => Promise<void>;
}

export const useVaultStore = create<VaultState>((set, get) => ({
  status: null,
  loading: false,
  error: null,
  refresh: async () => {
    try {
      const status = await ipc<VaultStatus>("vault_status");
      set({ status, error: null });
    } catch (e) {
      set({ error: String(e) });
    }
  },
  unlock: async (pass: string) => {
    set({ loading: true, error: null });
    try {
      await ipc("vault_unlock", { pass });
      set({ loading: false });
      await get().refresh();
    } catch (e) {
      set({ loading: false, error: String(e) });
      throw e;
    }
  },
  lock: async () => {
    await ipc("vault_lock");
    await get().refresh();
  },
}));

interface AppState {
  profileName: string;
  theme: ThemePref;
  setProfileName: (n: string) => void;
  setTheme: (t: ThemePref) => void;
  loadProfile: () => Promise<void>;
}

export const useAppStore = create<AppState>((set, get) => ({
  profileName: "XieBall",
  theme: "system",
  setProfileName: (n) => {
    set({ profileName: n });
    void ipc("meta_set", { key: "profile_name", value: n }).catch(() => {
      /* keep in-memory even if persist fails */
    });
  },
  setTheme: (t) => {
    set({ theme: t });
    applyTheme(t);
    try {
      localStorage.setItem(THEME_MIRROR_KEY, t);
    } catch {
      /* mirror is best-effort */
    }
    void ipc("meta_set", { key: "theme", value: t }).catch(() => {});
  },
  loadProfile: async () => {
    if (get().profileName !== "XieBall") return;
    try {
      const [profile, theme] = await Promise.all([
        ipc<string | null>("meta_get", { key: "profile_name" }),
        ipc<ThemePref | null>("meta_get", { key: "theme" }),
      ]);
      if (profile && profile.trim()) set({ profileName: profile });
      if (theme === "light" || theme === "dark" || theme === "system") {
        set({ theme });
        applyTheme(theme);
      }
    } catch {
      /* defaults remain */
    }
  },
}));

// Hydrate profile on first import (App entry will also call loadProfile).
void useAppStore.getState().loadProfile();

// ── Shared wallet + groups state ────────────────────────────────────
// Every page that needs wallets/groups uses this store instead of
// refetching independently — keeps group assignment in sync app-wide.

interface WalletState {
  wallets: WalletRow[];
  groups: WalletGroup[];
  loading: boolean;
  error: string | null;
  load: () => Promise<void>;
  refresh: () => Promise<void>;
}

export const useWalletStore = create<WalletState>((set) => ({
  wallets: [],
  groups: [],
  loading: false,
  error: null,
  load: async () => {
    set({ loading: true });
    try {
      const [wallets, groups] = await Promise.all([
        ipc<WalletRow[]>("wallet_list"),
        ipc<WalletGroup[]>("group_list"),
      ]);
      set({ wallets, groups, loading: false, error: null });
    } catch (e) {
      set({ loading: false, error: String(e) });
    }
  },
  refresh: async () => {
    try {
      const [wallets, groups] = await Promise.all([
        ipc<WalletRow[]>("wallet_list"),
        ipc<WalletGroup[]>("group_list"),
      ]);
      set({ wallets, groups, error: null });
    } catch (e) {
      set({ error: String(e) });
    }
  },
}));

/** Pure helpers usable outside React (or with useMemo in components). */
export function groupNameMap(groups: WalletGroup[]): Map<number, string> {
  const m = new Map<number, string>();
  for (const g of groups) m.set(g.id, g.name);
  return m;
}

export function filterWalletsByGroup(
  wallets: WalletRow[],
  filter: number | "all" | "ungrouped",
): WalletRow[] {
  if (filter === "all") return wallets;
  if (filter === "ungrouped") return wallets.filter((w) => w.group_id == null);
  return wallets.filter((w) => w.group_id === filter);
}
