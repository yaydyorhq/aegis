import { useEffect, useState, type FormEvent } from "react";
import { Lock, ShieldCheck } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useVaultStore } from "../../store/app";

export function VaultGate() {
  const { status, refresh, unlock, loading, error } = useVaultStore();
  const [pass, setPass] = useState("");
  const [pass2, setPass2] = useState("");
  const win = getCurrentWindow();

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (!status) {
    return (
      <div className="flex h-screen flex-col items-center justify-center gap-3 bg-bg text-muted">
        <div className="absolute right-4 top-4 flex items-center gap-1.5" data-tauri-drag-region>
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
        <div>Loading vault…</div>
        {error ? (
          <div className="max-w-md text-center text-[12px] text-danger">{error}</div>
        ) : null}
        <button
          onClick={() => void refresh()}
          className="rounded-lg border border-line bg-card px-4 py-2 text-[13px] text-fg hover:border-muted/40"
        >
          Retry
        </button>
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
      /* store holds error */
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-bg/95 backdrop-blur-sm">
      <div className="absolute right-4 top-4 flex items-center gap-1.5" data-tauri-drag-region>
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
      <form
        onSubmit={onSubmit}
        className="w-[380px] rounded-2xl border border-line bg-card p-6 shadow-2xl"
      >
        <div className="mb-5 flex items-center gap-3">
          <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-accent/15 text-accent">
            {isNew ? <ShieldCheck className="h-5 w-5" /> : <Lock className="h-5 w-5" />}
          </div>
          <div>
            <div className="text-[15px] font-semibold">
              {isNew ? "Create local vault" : "Unlock vault"}
            </div>
            <div className="text-[12px] text-muted">
              {isNew
                ? "Encrypts all wallets on this device"
                : "Enter passphrase to decrypt wallets"}
            </div>
          </div>
        </div>

        <label className="mb-1.5 block text-[12px] text-muted">Passphrase</label>
        <input
          type="password"
          autoFocus
          value={pass}
          onChange={(e) => setPass(e.target.value)}
          className="mb-3 w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          placeholder="min. 8 characters"
        />

        {isNew ? (
          <>
            <label className="mb-1.5 block text-[12px] text-muted">Confirm</label>
            <input
              type="password"
              value={pass2}
              onChange={(e) => setPass2(e.target.value)}
              className="mb-3 w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              placeholder="repeat passphrase"
            />
          </>
        ) : null}

        {error ? <div className="mb-3 text-[12px] text-danger">{error}</div> : null}
        {isNew && pass && pass !== pass2 ? (
          <div className="mb-3 text-[12px] text-warn">Passphrases do not match</div>
        ) : null}

        <button
          type="submit"
          disabled={loading || !pass || (isNew && pass !== pass2)}
          className="w-full rounded-lg bg-fg py-2.5 text-[13px] font-semibold text-bg transition hover:opacity-90 disabled:opacity-40"
        >
          {loading ? "Deriving key…" : isNew ? "Create vault" : "Unlock"}
        </button>

        <p className="mt-3 text-center text-[11px] leading-relaxed text-muted">
          Secrets stay on this device. Argon2id + AES-256-GCM.
        </p>
      </form>
    </div>
  );
}
