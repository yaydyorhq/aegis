import { useCallback, useEffect, useState, type FormEvent } from "react";
import { Network, PlugZap, Trash2 } from "lucide-react";
import { ipc } from "../lib/ipc";
import type { ChainRow, RpcTestResult } from "../lib/types";
import { EmptyState, PageHeader, StatusDot } from "../components/ui";

interface TestState {
  [id: number]: RpcTestResult;
}

export function ChainsRpcPage() {
  const [chains, setChains] = useState<ChainRow[]>([]);
  const [tests, setTests] = useState<TestState>({});
  const [testing, setTesting] = useState<number | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [form, setForm] = useState({
    name: "",
    chainId: "",
    rpcUrl: "",
    symbol: "ETH",
    explorer: "",
  });
  const [err, setErr] = useState<string | null>(null);
  const [editId, setEditId] = useState<number | null>(null);

  const load = useCallback(async () => {
    try {
      setChains(await ipc<ChainRow[]>("chain_list"));
      setErr(null);
    } catch (e) {
      setErr(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function onTest(id: number) {
    setTesting(id);
    try {
      const r = await ipc<RpcTestResult>("chain_test", { id });
      setTests((t) => ({ ...t, [id]: r }));
    } catch (e) {
      setTests((t) => ({
        ...t,
        [id]: { ok: false, chain_id_returned: null, latency_ms: null, error: String(e) },
      }));
    } finally {
      setTesting(null);
    }
  }

  async function onTestAll() {
    for (const c of chains.filter((x) => x.enabled)) {
      await onTest(c.id);
    }
  }

  function startEdit(c: ChainRow) {
    setEditId(c.id);
    setForm({
      name: c.name,
      chainId: String(c.chain_id),
      rpcUrl: c.rpc_url,
      symbol: c.symbol,
      explorer: c.explorer ?? "",
    });
    setShowForm(true);
    setErr(null);
  }

  function resetForm() {
    setEditId(null);
    setForm({ name: "", chainId: "", rpcUrl: "", symbol: "ETH", explorer: "" });
    setShowForm(false);
  }

  function openAdd() {
    setEditId(null);
    setForm({ name: "", chainId: "", rpcUrl: "", symbol: "ETH", explorer: "" });
    setShowForm(true);
    setErr(null);
  }

  async function onSave(e: FormEvent) {
    e.preventDefault();
    setErr(null);
    try {
      const chainIdNum = Number(form.chainId);
      if (!Number.isInteger(chainIdNum) || chainIdNum <= 0) {
        setErr("Chain ID must be a positive integer");
        return;
      }
      const rpc = form.rpcUrl.trim();
      if (!/^https?:\/\//.test(rpc)) {
        setErr("RPC must start with http:// or https://");
        return;
      }
      if (!form.name.trim()) {
        setErr("Name is required");
        return;
      }
      await ipc("chain_upsert", {
        chain: {
          id: editId,
          name: form.name.trim(),
          chain_id: chainIdNum,
          rpc_url: rpc,
          symbol: form.symbol.trim() || "ETH",
          explorer: form.explorer.trim() || null,
          enabled: true,
        },
      });
      resetForm();
      await load();
    } catch (e) {
      setErr(String(e));
    }
  }

  async function onDelete(id: number) {
    try {
      await ipc("chain_delete", { id });
      await load();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <div className="p-6">
      <PageHeader
        suite="Infrastructure"
        title="Chains & RPC"
        subtitle="Network endpoints used for all on-chain calls"
        action={
          <div className="flex gap-2">
            <button
              onClick={onTestAll}
              className="flex items-center gap-1.5 rounded-lg border border-line bg-card px-3 py-2 text-[13px] text-fg hover:border-muted/40"
            >
              <PlugZap className="h-4 w-4" /> Test all
            </button>
            <button
              onClick={openAdd}
              className="flex items-center gap-1.5 rounded-lg bg-fg px-3 py-2 text-[13px] font-semibold text-bg hover:opacity-90"
            >
              <Network className="h-4 w-4" /> Add RPC
            </button>
          </div>
        }
      />

      {showForm ? (
        <form onSubmit={onSave} className="mb-4 grid grid-cols-2 gap-2 rounded-[14px] border border-line bg-card p-4">
          <input
            placeholder="Name"
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
            className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          />
          <input
            placeholder="Chain ID"
            value={form.chainId}
            onChange={(e) => setForm({ ...form, chainId: e.target.value })}
            className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          />
          <input
            placeholder="https://rpc.example.com"
            value={form.rpcUrl}
            onChange={(e) => setForm({ ...form, rpcUrl: e.target.value })}
            className="col-span-2 rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          />
          <input
            placeholder="Symbol"
            value={form.symbol}
            onChange={(e) => setForm({ ...form, symbol: e.target.value })}
            className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          />
          <input
            placeholder="Explorer (optional)"
            value={form.explorer}
            onChange={(e) => setForm({ ...form, explorer: e.target.value })}
            className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          />
          <div className="col-span-2 flex justify-end">
            <button
              type="submit"
              disabled={!form.name || !form.chainId || !form.rpcUrl}
              className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
            >
              Save
            </button>
          </div>
        </form>
      ) : null}

      {err ? <div className="mb-3 text-[12px] text-danger">{err}</div> : null}

      <div className="overflow-hidden rounded-[14px] border border-line bg-card">
        <div className="grid grid-cols-[1.2fr_80px_1fr_70px_140px_50px] gap-2 border-b border-line px-4 py-2.5 text-[11px] uppercase tracking-wide text-muted">
          <div>Network</div>
          <div>Chain ID</div>
          <div>RPC</div>
          <div>Status</div>
          <div>Latency</div>
          <div />
        </div>
        {chains.length === 0 ? (
          <EmptyState title="No chains configured" description="Add a custom RPC endpoint." />
        ) : (
          chains.map((c) => {
            const t = tests[c.id];
            return (
              <div
                key={c.id}
                className="grid grid-cols-[1.2fr_80px_1fr_70px_140px_50px] items-center gap-2 border-b border-line/60 px-4 py-2.5 text-[13px] last:border-0 hover:bg-line/30"
              >
                <div className="font-medium">{c.name}</div>
                <div className="font-mono text-[12px] text-muted">{c.chain_id}</div>
                <div className="truncate font-mono text-[11px] text-muted" title={c.rpc_url}>
                  {c.rpc_url}
                </div>
                <div className="flex items-center gap-1.5 text-[12px]">
                  <StatusDot ok={!!t?.ok} />
                  {t ? (t.ok ? "OK" : "Fail") : "—"}
                </div>
                <div className="text-[12px] text-muted" title={t?.error ?? undefined}>
                  {testing === c.id
                    ? "Testing…"
                    : t?.latency_ms != null
                      ? `${t.latency_ms} ms${t.error ? " · fail" : ""}`
                      : t?.error
                        ? "fail"
                        : "—"}
                </div>
                <div className="flex justify-end gap-1">
                  <button
                    onClick={() => onTest(c.id)}
                    disabled={testing === c.id}
                    className="rounded-md px-2 py-1 text-[12px] text-accent hover:bg-line disabled:opacity-40"
                  >
                    Test
                  </button>
                  <button
                    onClick={() => startEdit(c)}
                    className="rounded-md px-2 py-1 text-[12px] text-muted hover:bg-line hover:text-fg"
                  >
                    Edit
                  </button>
                  <button
                    onClick={() => onDelete(c.id)}
                    className="rounded-md p-1 text-muted hover:bg-line hover:text-danger"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>
            );
          })
        )}
      </div>

      <div className="mt-3 text-[12px] text-muted">
        {chains.filter((c) => c.enabled).length} enabled · {chains.length} total
      </div>
    </div>
  );
}
