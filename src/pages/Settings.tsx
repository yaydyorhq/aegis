import { useEffect, useState } from "react";
import { Lock, ShieldCheck, Info, Monitor, Moon, Sun } from "lucide-react";
import { PageHeader, StatusDot } from "../components/ui";
import { cn } from "../lib/utils";
import { useAppStore, useVaultStore, type ThemePref } from "../store/app";

const THEME_OPTIONS: { value: ThemePref; label: string; icon: typeof Monitor }[] = [
  { value: "system", label: "System", icon: Monitor },
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
];

export function SettingsPage() {
  const profileName = useAppStore((s) => s.profileName);
  const setProfileName = useAppStore((s) => s.setProfileName);
  const theme = useAppStore((s) => s.theme);
  const setTheme = useAppStore((s) => s.setTheme);
  const { status, lock, refresh } = useVaultStore();
  const [draft, setDraft] = useState(profileName);
  const [saved, setSaved] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    setDraft(profileName);
  }, [profileName]);

  function saveProfile() {
    const n = draft.trim() || profileName;
    setProfileName(n);
    setSaved(true);
    setTimeout(() => setSaved(false), 1500);
  }

  async function onLock() {
    try {
      await lock();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <div className="p-6">
      <PageHeader suite="Infrastructure" title="Settings" subtitle="Local preferences — stored on this device only" />

      <div className="grid max-w-3xl gap-4">
        <section className="rounded-[14px] border border-line bg-card p-5">
          <div className="mb-3 text-[13px] font-semibold">Appearance</div>
          <div className="inline-flex rounded-lg border border-line bg-bg p-0.5">
            {THEME_OPTIONS.map(({ value, label, icon: Icon }) => (
              <button
                key={value}
                onClick={() => setTheme(value)}
                className={cn(
                  "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[12.5px] transition-colors",
                  theme === value
                    ? "bg-line font-medium text-fg"
                    : "text-muted hover:text-fg",
                )}
              >
                <Icon className="h-3.5 w-3.5" />
                {label}
              </button>
            ))}
          </div>
          <p className="mt-2 text-[12px] text-muted">
            System follows your OS appearance. Saved on this device.
          </p>
        </section>

        <section className="rounded-[14px] border border-line bg-card p-5">
          <div className="mb-3 text-[13px] font-semibold">Profile</div>
          <div className="flex gap-2">
            <input
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              className="flex-1 rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              placeholder="Display name"
            />
            <button
              onClick={saveProfile}
              className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white"
            >
              {saved ? "Saved" : "Save"}
            </button>
          </div>
        </section>

        <section className="rounded-[14px] border border-line bg-card p-5">
          <div className="mb-3 flex items-center justify-between">
            <div className="text-[13px] font-semibold">Vault</div>
            <div className="flex items-center gap-2 text-[12px] text-muted">
              <StatusDot ok={!!status?.unlocked} />
              {status?.unlocked ? "Unlocked" : status?.initialized ? "Locked" : "Not initialized"}
            </div>
          </div>
          <div className="mb-3 space-y-1.5 text-[12.5px] text-muted">
            <div className="flex items-center gap-2">
              <ShieldCheck className="h-4 w-4 text-ok" /> Argon2id key derivation · AES-256-GCM encryption
            </div>
            <div className="flex items-center gap-2">
              <Lock className="h-4 w-4" /> Private keys never leave this device; DEK held only in process memory
            </div>
          </div>
          {status?.unlocked ? (
            <button
              onClick={onLock}
              className="rounded-lg border border-line bg-bg px-4 py-2 text-[13px] text-danger hover:border-danger/50"
            >
              Lock vault now
            </button>
          ) : (
            <div className="text-[12px] text-warn">Unlock the vault from the gate overlay to manage secrets.</div>
          )}
          {err ? <div className="mt-2 text-[12px] text-danger">{err}</div> : null}
        </section>

        <section className="rounded-[14px] border border-line bg-card p-5">
          <div className="mb-3 text-[13px] font-semibold">About</div>
          <div className="space-y-1.5 text-[12.5px] text-muted">
            <div className="flex items-center gap-2">
              <Info className="h-4 w-4" /> Aegis 0.1.0 · Tauri 2 + React + SQLite
            </div>
            <div>Local database: app data dir · aegis.db</div>
            <div>All RPC, mint, NFT, and eligibility calls run from this machine.</div>
          </div>
        </section>
      </div>
    </div>
  );
}
