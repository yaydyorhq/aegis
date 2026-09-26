import { create } from "zustand";
import { ipc } from "../lib/ipc";
import type { VaultStatus, WalletGroup, WalletRow } from "../lib/types";

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
  setProfileName: (n: string) => void;
  loadProfile: () => Promise<void>;
}

export const useAppStore = create<AppState>((set, get) => ({
  profileName: "XieBall",
  setProfileName: (n) => {
    set({ profileName: n });
    void ipc("meta_set", { key: "profile_name", value: n }).catch(() => {
      /* keep in-memory even if persist fails */
    });
  },
  loadProfile: async () => {
    if (get().profileName !== "XieBall") return;
    try {
      const v = await ipc<string | null>("meta_get", { key: "profile_name" });
      if (v && v.trim()) set({ profileName: v });
    } catch {
      /* default remains */
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
