import { useEffect, useState, type FormEvent } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { Lock, RefreshCw, ShieldCheck } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useVaultStore } from "../../store/app";
import { cn } from "../../lib/utils";
import { LogoMark } from "../../components/LogoMark";

function WindowDots() {
  const win = getCurrentWindow();
  return (
    <div className="flex items-center gap-1.5" data-tauri-drag-region>
      <button
        onClick={() => void win.minimize()}
        className="h-3 w-3 rounded-full bg-[#28c840] hover:brightness-110"
        title="Minimize"
        aria-label="Minimize"
      />
      <button
        onClick={() => void win.toggleMaximize()}
        className="h-3 w-3 rounded-full bg-[#f5bf4f] hover:brightness-110"
        title="Maximize"
        aria-label="Maximize"
      />
      <button
        onClick={() => void win.close()}
        className="h-3 w-3 rounded-full bg-[#f74c3a] hover:brightness-110"
        title="Close"
        aria-label="Close"
      />
    </div>
  );
}

function Brand() {
  return (
    <div className="flex items-center gap-2.5" data-tauri-drag-region>
      <LogoMark className="h-7 w-7" />
      <span className="font-mono text-[13px] font-semibold uppercase tracking-[0.18em]">
        Aegis
      </span>
    </div>
  );
}

export function VaultGate() {
  const { status, refresh, unlock, loading, error } = useVaultStore();
  const [pass, setPass] = useState("");
  const [pass2, setPass2] = useState("");
  const [version, setVersion] = useState("0.2.0");
  const [shake, setShake] = useState(0);

  useEffect(() => {
    void getVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (!status) {
    return (
      <div className="fixed inset-0 z-50 flex flex-col overflow-hidden bg-bg">
        <div
          className="pointer-events-none absolute inset-0 opacity-[0.16]"
          style={{
            background:
              "radial-gradient(900px 380px at 50% -10%, #4f7cff33, transparent)",
          }}
        />
        <div className="relative flex h-[52px] shrink-0 items-center justify-between px-5">
          <Brand />
          <WindowDots />
        </div>
        <div className="relative flex min-h-0 flex-1 flex-col items-center justify-center gap-4">
          <div className="font-mono text-[12px] uppercase tracking-[0.18em] text-muted">
            Initializing vault…
          </div>
          <button
            onClick={() => void refresh()}
            className="flex items-center gap-2 rounded-lg border border-line bg-card px-4 py-2 font-mono text-[11px] uppercase tracking-[0.1em] text-fg hover:border-muted/40"
          >
            <RefreshCw className="h-3.5 w-3.5" /> Retry
          </button>
          {error ? (
            <div className="max-w-md text-center font-mono text-[11px] text-danger">
              {error}
            </div>
          ) : null}
        </div>
        <div className="relative flex h-10 shrink-0 items-center justify-center">
          <span className="font-mono text-[10px] uppercase tracking-[0.14em] text-muted">
            Aegis {version} · local command center
          </span>
        </div>
      </div>
    );
  }

  if (status.unlocked) return null;

  const isNew = !status.initialized;

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    if (isNew && pass !== pass2) return;
    try {
      await unlock(pass);
      setPass("");
      setPass2("");
    } catch {
      setShake((n) => n + 1); // retrigger the shake on every failed attempt
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex flex-col overflow-hidden bg-bg">
      <div
        className="pointer-events-none absolute inset-0 opacity-[0.16]"
        style={{
          background:
            "radial-gradient(900px 380px at 50% -10%, #4f7cff33, transparent), radial-gradient(700px 280px at 85% 110%, #f5a52418, transparent)",
        }}
      />
      <div className="relative flex h-[52px] shrink-0 items-center justify-between px-5">
        <Brand />
        <WindowDots />
      </div>

      <div className="relative flex min-h-0 flex-1 items-center justify-center px-4">
        <form
          key={shake}
          onSubmit={onSubmit}
          className={cn(
            "w-[400px] overflow-hidden rounded-[14px] border border-line bg-card shadow-2xl",
            shake > 0 && error && "animate-shake",
          )}
        >
          <div className="flex items-center justify-between gap-2 border-b border-line px-5 py-3">
            <div className="flex items-center gap-2">
              {isNew ? (
                <ShieldCheck className="h-4 w-4 text-accent" />
              ) : (
                <Lock className="h-4 w-4 text-accent" />
              )}
              <span className="font-mono text-[11px] font-semibold uppercase tracking-[0.14em] text-fg">
                {isNew ? "Create local vault" : "Vault locked"}
              </span>
            </div>
            <span className="font-mono text-[10px] uppercase tracking-[0.1em] text-muted">
              {isNew ? "setup" : "secure"}
            </span>
          </div>

          <div className="space-y-3.5 px-5 py-5">
            <p className="text-[12px] leading-relaxed text-muted">
              {isNew
                ? "Encrypts all wallets on this device. This passphrase is the only way in."
                : "Enter your passphrase to decrypt wallets for signing."}
            </p>

            <label className="block">
              <span className="mb-1.5 block font-mono text-[10px] uppercase tracking-[0.1em] text-muted">
                Passphrase
              </span>
              <input
                type="password"
                autoFocus
                value={pass}
                onChange={(e) => setPass(e.target.value)}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[13px] outline-none focus:border-accent"
                placeholder="min. 8 characters"
              />
            </label>

            {isNew ? (
              <label className="block">
                <span className="mb-1.5 block font-mono text-[10px] uppercase tracking-[0.1em] text-muted">
                  Confirm
                </span>
                <input
                  type="password"
                  value={pass2}
                  onChange={(e) => setPass2(e.target.value)}
                  className="w-full rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[13px] outline-none focus:border-accent"
                  placeholder="repeat passphrase"
                />
              </label>
            ) : null}

            {error ? (
              <div className="font-mono text-[11px] text-danger">{error}</div>
            ) : null}
            {isNew && pass && pass !== pass2 ? (
              <div className="font-mono text-[11px] text-warn">
                Passphrases do not match
              </div>
            ) : null}

            <button
              type="submit"
              disabled={loading || !pass || (isNew && pass !== pass2)}
              className="w-full rounded-lg bg-accent py-2.5 font-mono text-[11.5px] font-semibold uppercase tracking-[0.14em] text-white transition hover:opacity-90 disabled:opacity-40"
            >
              {loading
                ? "Deriving key…"
                : isNew
                  ? "Create vault"
                  : "Unlock"}
            </button>
          </div>

          <div className="border-t border-line px-5 py-2.5 text-center font-mono text-[9.5px] uppercase tracking-[0.12em] text-muted">
            Argon2id · AES-256-GCM · zero data leaves this device
          </div>
        </form>
      </div>

      <div className="relative flex h-10 shrink-0 items-center justify-center">
        <span className="font-mono text-[10px] uppercase tracking-[0.14em] text-muted">
          Aegis {version} · local command center
        </span>
      </div>
    </div>
  );
}
