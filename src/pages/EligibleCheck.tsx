import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";
import { BadgeCheck, CheckSquare, Loader2, Square, Zap } from "lucide-react";
import { ipc } from "../lib/ipc";
import type { ChainRow, EnqueueBatchResult } from "../lib/types";
import { EmptyState, PageHeader, StatusDot, pushToast } from "../components/ui";
import { shortAddress } from "../lib/utils";
import {
  filterWalletsByGroup,
  groupNameMap,
  useWalletStore,
} from "../store/app";

interface EligRow {
  wallet_id: number | null;
  address: string;
  collection: string;
  eligible: boolean | null;
  detail: string;
  checked_at: number;
}

interface HistRow {
  id: number;
  wallet_id: number | null;
  collection: string;
  result: number;
  detail: string | null;
  checked_at: number;
}

type GroupFilter = number | "all" | "ungrouped";

export function EligibleCheckPage() {
  const { wallets, groups, load: loadWallets } = useWalletStore();
  const [chains, setChains] = useState<ChainRow[]>([]);
  const [selected, setSelected] = useState<number[]>([]);
  const [chainId, setChainId] = useState("");
  const [collection, setCollection] = useState("");
  const [results, setResults] = useState<EligRow[]>([]);
  const [history, setHistory] = useState<HistRow[]>([]);
  const [running, setRunning] = useState(false);
  const [minting, setMinting] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [groupFilter, setGroupFilter] = useState<GroupFilter>("all");

  const groupNames = useMemo(() => groupNameMap(groups), [groups]);
  const visibleWallets = useMemo(
    () => filterWalletsByGroup(wallets, groupFilter),
    [wallets, groupFilter],
  );

  const load = useCallback(async () => {
    try {
      await loadWallets();
      const [c, h] = await Promise.all([
        ipc<ChainRow[]>("chain_list"),
        ipc<HistRow[]>("eligibility_history", { limit: 30 }),
      ]);
      setChains(c);
      setHistory(h);
      setChainId((prev) => prev || (c[0] ? String(c[0].chain_id) : ""));
    } catch (e) {
      setErr(String(e));
    }
  }, [loadWallets]);

  useEffect(() => {
    void load();
  }, [load]);

  function toggle(id: number) {
    setSelected((s) => (s.includes(id) ? s.filter((x) => x !== id) : [...s, id]));
  }

  function selectVisible() {
    const ids = visibleWallets.map((w) => w.id);
    setSelected((s) => Array.from(new Set([...s, ...ids])));
  }

  function clearVisible() {
    const ids = new Set(visibleWallets.map((w) => w.id));
    setSelected((s) => s.filter((id) => !ids.has(id)));
  }

  async function onRun(e: FormEvent) {
    e.preventDefault();
    if (selected.length === 0 || !chainId || !collection.trim()) return;
    setRunning(true);
    setErr(null);
    setResults([]);
    try {
      const rows = await ipc<EligRow[]>("eligibility_run", {
        walletIds: selected,
        chainId: Number(chainId),
        collection: collection.trim(),
      });
      setResults(rows);
      await load();
    } catch (e2) {
      setErr(String(e2));
    } finally {
      setRunning(false);
    }
  }

  const allVisibleSelected =
    visibleWallets.length > 0 && visibleWallets.every((w) => selected.includes(w.id));

  const eligibleWalletIds = useMemo(
    () =>
      results
        .filter((r) => r.eligible === true && r.wallet_id != null)
        .map((r) => r.wallet_id as number),
    [results],
  );

  async function onMintEligible() {
    if (!chainId || !collection.trim() || eligibleWalletIds.length === 0) return;
    setMinting(true);
    setErr(null);
    try {
      const result = await ipc<EnqueueBatchResult>("mint_opensea_enqueue", {
        args: {
          wallet_ids: eligibleWalletIds,
          chain_id: Number(chainId),
          collection: collection.trim(),
          quantity: 1,
          token_id: "0",
          mode: "execute",
        },
      });
      const queued = result.tasks.length;
      const skipped = result.skipped.length;
      if (queued === 0) {
        const reason =
          result.skipped
            .map((s) => `#${s.wallet_id}: ${s.reason}`)
            .join("; ")
            .slice(0, 400) || "Nothing enqueued";
        setErr(reason);
        pushToast("OpenSea mint failed", "error", reason);
        return;
      }
      pushToast(
        "OpenSea stage mint queued",
        "ok",
        skipped > 0
          ? `${queued} queued · ${skipped} skipped`
          : `${queued} wallet(s) · qty 1`,
      );
      if (skipped > 0) {
        setErr(
          result.skipped
            .map((s) => `#${s.wallet_id}: ${s.reason}`)
            .join("; ")
            .slice(0, 400),
        );
      }
    } catch (e) {
      setErr(String(e));
      pushToast("OpenSea mint failed", "error", String(e).slice(0, 160));
    } finally {
      setMinting(false);
    }
  }

  return (
    <div className="p-6">
      <PageHeader
        suite="Operations"
        title="Eligible Check"
        subtitle="OpenSea stages (FCFS/presale via SIWE) first, then SeaDrop public drop or holdings"
      />

      <form onSubmit={onRun} className="mb-4 space-y-3 rounded-[14px] border border-line bg-card p-4">
        {/* group filter chips */}
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-[11px] uppercase tracking-wide text-muted">Group</span>
          <button
            type="button"
            onClick={() => setGroupFilter("all")}
            className={`rounded-full border px-2.5 py-1 text-[11px] ${
              groupFilter === "all"
                ? "border-accent bg-accent/10 text-accent"
                : "border-line text-muted hover:text-fg"
            }`}
          >
            All ({wallets.length})
          </button>
          <button
            type="button"
            onClick={() => setGroupFilter("ungrouped")}
            className={`rounded-full border px-2.5 py-1 text-[11px] ${
              groupFilter === "ungrouped"
                ? "border-accent bg-accent/10 text-accent"
                : "border-line text-muted hover:text-fg"
            }`}
          >
            Ungrouped ({wallets.filter((w) => w.group_id == null).length})
          </button>
          {groups.map((g) => (
            <button
              key={g.id}
              type="button"
              onClick={() => setGroupFilter(g.id)}
              className={`rounded-full border px-2.5 py-1 text-[11px] ${
                groupFilter === g.id
                  ? "border-accent bg-accent/10 text-accent"
                  : "border-line text-muted hover:text-fg"
              }`}
            >
              {g.name} ({g.wallet_count})
            </button>
          ))}
          <div className="flex-1" />
          <button
            type="button"
            onClick={allVisibleSelected ? clearVisible : selectVisible}
            disabled={visibleWallets.length === 0}
            className="flex items-center gap-1 rounded-lg border border-line px-2.5 py-1 text-[11px] text-muted hover:text-fg disabled:opacity-40"
          >
            {allVisibleSelected ? (
              <>
                <CheckSquare className="h-3.5 w-3.5" /> Deselect visible
              </>
            ) : (
              <>
                <Square className="h-3.5 w-3.5" /> Select visible ({visibleWallets.length})
              </>
            )}
          </button>
        </div>

        <div className="flex flex-wrap gap-2">
          {visibleWallets.map((w) => (
            <button
              key={w.id}
              type="button"
              onClick={() => toggle(w.id)}
              className={`flex items-center gap-1.5 rounded-lg border px-3 py-1.5 font-mono text-[12px] ${
                selected.includes(w.id)
                  ? "border-accent bg-accent/15 text-accent"
                  : "border-line text-muted hover:border-muted/50"
              }`}
            >
              {w.label} · {shortAddress(w.address, 3)}
              {w.group_id != null ? (
                <span className="rounded border border-accent/30 bg-accent/10 px-1 font-sans text-[10px] text-accent">
                  {groupNames.get(w.group_id) ?? "—"}
                </span>
              ) : null}
            </button>
          ))}
          {wallets.length === 0 ? (
            <span className="text-[13px] text-muted">No wallets — create some first.</span>
          ) : visibleWallets.length === 0 ? (
            <span className="text-[13px] text-muted">No wallets in this group filter.</span>
          ) : null}
        </div>
        <div className="flex gap-2">
          <select
            value={chainId}
            onChange={(e) => setChainId(e.target.value)}
            className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          >
            <option value="">Chain…</option>
            {chains.map((c) => (
              <option key={c.id} value={c.chain_id}>
                {c.name}
              </option>
            ))}
          </select>
          <input
            placeholder="Collection address (0x…)"
            value={collection}
            onChange={(e) => setCollection(e.target.value)}
            className="flex-1 rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[13px] outline-none focus:border-accent"
          />
          <button
            type="submit"
            disabled={running || selected.length === 0 || !chainId || !collection.trim()}
            className="flex items-center gap-1.5 rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
          >
            <BadgeCheck className="h-4 w-4" />
            {running ? "Checking…" : `Run (${selected.length})`}
          </button>
        </div>
        {err ? <div className="text-[12px] text-danger">{err}</div> : null}
      </form>

      {results.length > 0 ? (
        <div className="mb-4 overflow-hidden rounded-[14px] border border-line bg-card">
          <div className="flex items-center justify-between border-b border-line px-4 py-2.5">
            <span className="text-[12px] font-semibold">Results</span>
            <button
              type="button"
              onClick={() => void onMintEligible()}
              disabled={
                minting ||
                eligibleWalletIds.length === 0 ||
                !chainId ||
                !collection.trim()
              }
              className="flex items-center gap-1.5 rounded-lg bg-accent px-3 py-1.5 text-[12px] font-semibold text-white disabled:opacity-40"
            >
              {minting ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Zap className="h-3.5 w-3.5" />
              )}
              {minting
                ? "Queueing…"
                : `Mint eligible via OpenSea (${eligibleWalletIds.length})`}
            </button>
          </div>
          {results.map((r, i) => (
            <div
              key={i}
              className="flex items-center justify-between border-b border-line/60 px-4 py-2.5 text-[13px] last:border-0"
            >
              <div className="font-mono text-[12px]">{r.address}</div>
              <div className="flex items-center gap-3 text-[12px]">
                <span className="text-muted">{r.detail}</span>
                <span
                  className={
                    r.eligible === true
                      ? "text-ok"
                      : r.eligible === null
                        ? "text-danger"
                        : "text-muted"
                  }
                >
                  {r.eligible === true
                    ? "ELIGIBLE"
                    : r.eligible === null
                      ? "ERROR"
                      : "NOT ELIGIBLE"}
                </span>
                <StatusDot ok={r.eligible === true} />
              </div>
            </div>
          ))}
        </div>
      ) : null}

      <div className="overflow-hidden rounded-[14px] border border-line bg-card">
        <div className="border-b border-line px-4 py-2.5 text-[12px] font-semibold">History</div>
        {history.length === 0 ? (
          <EmptyState title="No checks yet" description="Run an eligibility check to populate history." />
        ) : (
          history.map((h) => (
            <div
              key={h.id}
              className="grid grid-cols-[1fr_1.2fr_100px_140px] gap-2 border-b border-line/60 px-4 py-2.5 text-[13px] last:border-0"
            >
              <div className="truncate font-mono text-[12px]">{h.collection}</div>
              <div className="truncate text-muted">{h.detail || "—"}</div>
              <div className={h.result ? "text-ok" : "text-muted"}>
                {h.result ? "Yes" : "No"}
              </div>
              <div className="text-[12px] text-muted">{new Date(h.checked_at).toLocaleString()}</div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
