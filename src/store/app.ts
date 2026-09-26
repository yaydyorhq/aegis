import { create } from "zustand";
import { ipc } from "../lib/ipc";
import type { VaultStatus } from "../lib/types";

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
}

export const useAppStore = create<AppState>((set) => ({
  profileName: "XieBall",
  setProfileName: (n) => set({ profileName: n }),
}));
