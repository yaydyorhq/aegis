import { useCallback, useEffect, useState, type FormEvent } from "react";
import { KeyRound, Trash2 } from "lucide-react";
import { ipc } from "../lib/ipc";
import { EmptyState, PageHeader } from "../components/ui";

interface ApiKeyRow {
  id: number;
  provider: string;
  masked: string;
  base_url: string | null;
}

const PRESETS = ["opensea", "alchemy", "infura", "etherscan", "custom"];

export function ApiSettingsPage() {
  const [keys, setKeys] = useState<ApiKeyRow[]>([]);
  const [provider, setProvider] = useState("opensea");
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setKeys(await ipc<ApiKeyRow[]>("api_key_list"));
    } catch (e) {
      setErr(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function onSave(e: FormEvent) {
    e.preventDefault();
    setErr(null);
    try {
      await ipc("api_key_set", {
        provider,
        key,
        baseUrl: baseUrl || null,
      });
      setKey("");
      setBaseUrl("");
      await load();
    } catch (e2) {
      setErr(String(e2));
    }
  }

  async function onDelete(id: number) {
    try {
      await ipc("api_key_delete", { id });
      await load();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <div className="p-6">
      <PageHeader
        suite="Infrastructure"
        title="API Settings"
        subtitle="Provider keys are encrypted in the local vault — never leave this device"
      />

      <form onSubmit={onSave} className="mb-4 space-y-2 rounded-[14px] border border-line bg-card p-4">
        <div className="flex gap-2">
          <select
            value={provider}
            onChange={(e) => setProvider(e.target.value)}
            className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          >
            {PRESETS.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
          <input
            type="password"
            placeholder="API key"
            value={key}
            onChange={(e) => setKey(e.target.value)}
            className="flex-1 rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          />
          <input
            placeholder="Base URL (optional)"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            className="flex-1 rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          />
          <button
            type="submit"
            disabled={!key.trim()}
            className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
          >
            Save
          </button>
        </div>
        {err ? <div className="text-[12px] text-danger">{err}</div> : null}
      </form>

      <div className="overflow-hidden rounded-[14px] border border-line bg-card">
        <div className="border-b border-line px-4 py-2 font-mono text-[10.5px] font-semibold uppercase tracking-[0.1em] text-muted">Configured providers</div>
        {keys.length === 0 ? (
          <EmptyState title="No API keys" description="Add a provider key to unlock external data sources." />
        ) : (
          keys.map((k) => (
            <div
              key={k.id}
              className="flex items-center justify-between border-b border-line/60 px-4 py-2.5 text-[13px] last:border-0"
            >
              <div className="flex items-center gap-2.5">
                <KeyRound className="h-4 w-4 text-muted" />
                <span className="font-medium">{k.provider}</span>
                <span className="font-mono text-[12px] text-muted">{k.masked}</span>
                {k.base_url ? <span className="text-[11px] text-muted">{k.base_url}</span> : null}
              </div>
              <button
                onClick={() => onDelete(k.id)}
                className="rounded p-1 text-muted hover:bg-line hover:text-danger"
              >
                <Trash2 className="h-4 w-4" />
              </button>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
