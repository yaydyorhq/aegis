import { useEffect, useState, type ReactNode } from "react";
import { getVersion } from "@tauri-apps/api/app";
import {
  Bell,
  Info,
  Lock,
  Monitor,
  Moon,
  Palette,
  RefreshCw,
  ShieldCheck,
  Sun,
  Trash2,
  User,
} from "lucide-react";
import { PageHeader, StatusDot, Switch, pushToast } from "../components/ui";
import { ipc } from "../lib/ipc";
import { cn } from "../lib/utils";
import {
  notifyOsEnabled,
  notifySoundEnabled,
  setNotifyOsEnabled,
  setNotifySoundEnabled,
} from "../lib/notify";
import { useAppStore, useVaultStore, type ThemePref } from "../store/app";

const THEME_OPTIONS: { value: ThemePref; label: string; icon: typeof Monitor }[] = [
  { value: "system", label: "System", icon: Monitor },
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
];

const selectCls =
  "w-full rounded-lg border border-line bg-bg px-3 py-2 text-[12.5px] outline-none focus:border-accent sm:w-auto";

// ── consistent section + setting-row primitives ─────────────────────

function Section({
  icon: Icon,
  title,
  right,
  children,
  className,
}: {
  icon: typeof Bell;
  title: string;
  right?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section
      className={cn(
        "overflow-hidden rounded-[14px] border border-line bg-card",
        className,
      )}
    >
      <div className="flex items-center justify-between gap-3 border-b border-line px-5 py-3">
        <div className="flex items-center gap-2 text-[13px] font-semibold text-fg">
          <Icon className="h-4 w-4 text-muted" />
          {title}
        </div>
        {right}
      </div>
      <div className="px-5 py-2">{children}</div>
    </section>
  );
}

/** Label + description above the control on narrow windows, side-by-side on
 *  wide ones — hairline between rows. */
function Row({
  label,
  description,
  children,
}: {
  label: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-2.5 py-3 first:pt-1 last:pb-1 sm:flex-row sm:items-center sm:justify-between sm:gap-4 [&:not(:first-child)]:border-t [&:not(:first-child)]:border-line/60">
      <div className="min-w-0">
        <div className="text-[12.5px] text-fg">{label}</div>
        {description ? (
          <div className="mt-0.5 text-[11.5px] leading-relaxed text-muted">{description}</div>
        ) : null}
      </div>
      <div className="w-full sm:w-auto sm:shrink-0">{children}</div>
    </div>
  );
}

export function SettingsPage() {
  const profileName = useAppStore((s) => s.profileName);
  const setProfileName = useAppStore((s) => s.setProfileName);
  const theme = useAppStore((s) => s.theme);
  const setTheme = useAppStore((s) => s.setTheme);
  const { status, lock, refresh } = useVaultStore();
  const [draft, setDraft] = useState(profileName);
  const [saved, setSaved] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [notifyOs, setNotifyOs] = useState(notifyOsEnabled);
  const [notifySound, setNotifySound] = useState(notifySoundEnabled);
  /** Auto re-scan interval for the Collection PnL, minutes (0 = off). */
  const [autoscan, setAutoscan] = useState<string>("0");
  /** Vault auto-lock after idle minutes (0 = off). */
  const [autolock, setAutolock] = useState<string>("0");
  /** Append-only table retention in days (0 = keep everything). */
  const [retention, setRetention] = useState<string>("0");
  const [pruning, setPruning] = useState(false);
  /** Real app version from the Tauri build — proves which binary you run. */
  const [appVersion, setAppVersion] = useState("0.2.0");

  useEffect(() => {
    void getVersion()
      .then((v) => setAppVersion(v))
      .catch(() => {});
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    setDraft(profileName);
  }, [profileName]);

  // Settings persisted in the backend meta table, hydrated once on mount.
  useEffect(() => {
    void (async () => {
      try {
        const [v, a, r] = await Promise.all([
          ipc<string | null>("meta_get", { key: "pnl_autoscan" }),
          ipc<string | null>("meta_get", { key: "vault_autolock" }),
          ipc<string | null>("meta_get", { key: "db_retention_days" }),
        ]);
        if (v != null) setAutoscan(v);
        if (a != null) setAutolock(a);
        if (r != null) setRetention(r);
      } catch {
        /* defaults off */
      }
    })();
  }, []);

  function setMeta(key: string, value: string, failMsg: string) {
    void ipc("meta_set", { key, value }).catch(() => setErr(failMsg));
  }

  async function onPruneNow() {
    const days = Number(retention) || 0;
    if (days <= 0) return;
    setPruning(true);
    setErr(null);
    try {
      const r = await ipc<{
        activity: number;
        pnl_scans: number;
        collection_scans: number;
        eligibility: number;
        nft_cache: number;
      }>("db_prune", { retentionDays: days });
      const total =
        r.activity + r.pnl_scans + r.collection_scans + r.eligibility + r.nft_cache;
      pushToast("Prune complete", "ok", `${total} row(s) removed (${days}-day retention)`);
    } catch (e) {
      setErr(String(e));
    } finally {
      setPruning(false);
    }
  }

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
      <PageHeader
        suite="Infrastructure"
        title="Settings"
        subtitle="Local preferences — stored on this device only"
      />

      {err ? (
        <div className="mb-4 rounded-lg border border-danger/30 bg-danger/10 px-4 py-2.5 text-[12px] text-danger">
          {err}
        </div>
      ) : null}

      <div className="grid max-w-3xl items-start gap-4 lg:grid-cols-2">
        <Section icon={User} title="Profile">
          <div className="flex flex-col gap-2 py-2 first:pt-1 last:pb-1 sm:flex-row">
            <input
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              className="min-w-0 flex-1 rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              placeholder="Display name"
            />
            <button
              onClick={saveProfile}
              className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white"
            >
              {saved ? "Saved" : "Save"}
            </button>
          </div>
        </Section>

        <Section icon={Palette} title="Appearance">
          <div className="py-2 first:pt-1 last:pb-1">
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
            <p className="mt-2 text-[11.5px] text-muted">
              System follows your OS appearance. Saved on this device.
            </p>
          </div>
        </Section>

        <Section icon={Bell} title="Notifications">
          <div className="divide-y divide-line/60">
            <Row
              label="OS notification on mint result"
              description="Fires only while the window is hidden — the toast covers the visible case."
            >
              <Switch
                checked={notifyOs}
                onChange={(v) => {
                  setNotifyOs(v);
                  setNotifyOsEnabled(v);
                }}
                label="OS notification on mint result"
              />
            </Row>
            <Row
              label="Chime on mint result"
              description="Rising tone on confirmed, falling on failed."
            >
              <Switch
                checked={notifySound}
                onChange={(v) => {
                  setNotifySound(v);
                  setNotifySoundEnabled(v);
                }}
                label="Chime on mint result"
              />
            </Row>
          </div>
        </Section>

        <Section icon={RefreshCw} title="Automations">
          <div className="divide-y divide-line/60">
            <Row
              label="Portfolio auto re-scan"
              description="Re-runs your last Collection PnL scan (same contract, wallets, window, and fee) so the Dashboard sparkline and net PnL fill themselves. Scans are RPC-heavy — 5 minutes suits launch days, 60 minutes is plenty for holding."
            >
              <select
                value={autoscan}
                onChange={(e) => {
                  setAutoscan(e.target.value);
                  setMeta("pnl_autoscan", e.target.value, "Could not persist auto re-scan setting");
                }}
                className={selectCls}
              >
                <option value="0">Off</option>
                <option value="5">Every 5 minutes</option>
                <option value="15">Every 15 minutes</option>
                <option value="60">Every 60 minutes</option>
              </select>
            </Row>
          </div>
        </Section>

        <Section icon={ShieldCheck} title="Security" className="lg:col-span-2"
          right={
            <div className="flex items-center gap-2 text-[11.5px] text-muted">
              <StatusDot ok={!!status?.unlocked} />
              {status?.unlocked ? "Unlocked" : status?.initialized ? "Locked" : "Not initialized"}
            </div>
          }
        >
          <div className="divide-y divide-line/60">
            <div className="py-3 first:pt-1">
              <div className="space-y-1 text-[11.5px] text-muted">
                <div className="flex items-center gap-2">
                  <ShieldCheck className="h-3.5 w-3.5 text-ok" />
                  Argon2id key derivation · AES-256-GCM encryption
                </div>
                <div className="flex items-center gap-2">
                  <Lock className="h-3.5 w-3.5" />
                  Private keys never leave this device; the encryption key lives
                  only in process memory.
                </div>
              </div>
            </div>
            {status?.unlocked ? (
              <>
                <Row
                  label="Auto-lock when idle"
                  description="Locks the vault after the app sits idle, so keys don't stay decryptable forever."
                >
                  <select
                    value={autolock}
                    onChange={(e) => {
                      setAutolock(e.target.value);
                      setMeta("vault_autolock", e.target.value, "Could not persist auto-lock setting");
                    }}
                    className={selectCls}
                  >
                    <option value="0">Off</option>
                    <option value="5">After 5 minutes</option>
                    <option value="15">After 15 minutes</option>
                    <option value="30">After 30 minutes</option>
                  </select>
                </Row>
                <Row label="Lock now" description="Wipe the encryption key from memory — unlock to sign again.">
                  <button
                    onClick={onLock}
                    className="w-full rounded-lg border border-line bg-bg px-3.5 py-2 text-[12.5px] text-danger hover:border-danger/50 sm:w-auto"
                  >
                    Lock vault
                  </button>
                </Row>
              </>
            ) : (
              <div className="py-3 text-[12px] text-warn">
                Unlock the vault from the gate overlay to manage secrets.
              </div>
            )}
          </div>
        </Section>

        <Section icon={Trash2} title="Data retention" className="lg:col-span-2">
          <div className="divide-y divide-line/60">
            <Row
              label="Keep history for"
              description="Prunes old activity, PnL scans, eligibility checks, and NFT cache. Money trails — mint tasks and fund jobs — are never touched. A prune also runs at startup when a retention is set."
            >
              <select
                value={retention}
                onChange={(e) => {
                  setRetention(e.target.value);
                  setMeta("db_retention_days", e.target.value, "Could not persist retention setting");
                }}
                className={selectCls}
              >
                <option value="0">Keep everything</option>
                <option value="30">Last 30 days</option>
                <option value="90">Last 90 days</option>
                <option value="365">Last 365 days</option>
              </select>
            </Row>
            <Row label="Prune now" description={`Apply the ${Number(retention) > 0 ? `${retention}-day` : ""} retention immediately.`}>
              <button
                onClick={() => void onPruneNow()}
                disabled={pruning || retention === "0"}
                className="rounded-lg border border-line bg-bg px-3.5 py-2 text-[12.5px] text-danger hover:border-danger/50 disabled:opacity-40"
              >
                {pruning ? "Pruning…" : "Prune now"}
              </button>
            </Row>
          </div>
        </Section>

        <Section icon={Info} title="About" className="lg:col-span-2">
          <div className="space-y-1 py-2 text-[12px] leading-relaxed text-muted first:pt-1 last:pb-1">
            <div>Aegis {appVersion} · Tauri 2 + React + SQLite</div>
            <div>Local database: app data dir · aegis.db</div>
            <div>All RPC, mint, NFT, and eligibility calls run from this machine.</div>
          </div>
        </Section>
      </div>
    </div>
  );
}
