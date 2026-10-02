import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import {
  BadgeCheck,
  CheckCircle2,
  Clock,
  CheckSquare,
  LayoutGrid,
  Loader2,
  Rows3,
  Square,
  XCircle,
  Zap,
} from "lucide-react";
import { ipc } from "../lib/ipc";
import type {
  ChainRow,
  EnqueueBatchResult,
  StageMatrixResult,
} from "../lib/types";
import { EmptyState, PageHeader, pushToast } from "../components/ui";
import { shortAddress } from "../lib/utils";
import {
  filterWalletsByGroup,
  groupNameMap,
  useWalletStore,
} from "../store/app";

interface HistRow {
  id: number;
  wallet_id: number | null;
  collection: string;
  result: number;
  detail: string | null;
  checked_at: number;
}

type GroupFilter = number | "all" | "ungrouped";
type ViewMode = "table" | "cards";

/** Friendly labels for OpenSea + SeaDrop stage enum values. */
function stageLabel(name: string): string {
  const map: Record<string, string> = {
    // OpenSea stage types
    PUBLIC_SALE: "Public",
    SIGNED_PRESALE: "Pre-sale",
    MERKLE_PRESALE: "Allowlist",
    PRESALE: "Pre-sale",
    FCFS: "FCFS",
    GTD: "GTD",
    WL: "Allowlist",
    ALLOWLIST: "Allowlist",
    CLAIM: "Claim",
    PRIVATE: "Private",
    // On-chain fallback
    ONCHAIN: "On-chain",
  };
  return map[name] ?? name;
}

export function EligibleCheckPage() {
  const { wallets, groups, load: loadWallets } = useWalletStore();
  const [chains, setChains] = useState<ChainRow[]>([]);
  const [selected, setSelected] = useState<number[]>([]);
  const [chainId, setChainId] = useState("");
  const [collection, setCollection] = useState("");
  const [matrix, setMatrix] = useState<StageMatrixResult | null>(null);
  const [history, setHistory] = useState<HistRow[]>([]);
  const [running, setRunning] = useState(false);
  const [queueing, setQueueing] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [groupFilter, setGroupFilter] = useState<GroupFilter>("all");
  const [viewMode, setViewMode] = useState<ViewMode>("table");
  /** Row indices (into matrix.rows) selected for manual queue. */
  const [manualSel, setManualSel] = useState<number[]>([]);

  const navigate = useNavigate();
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

  /** Rows with at least one LIVE+eligible stage — only these can be queued. */
  const eligibleRows = useMemo(
    () =>
      (matrix?.rows ?? []).filter(
        (r) => r.wallet_id != null && r.stages.some((s) => s.actionable),
      ),
    [matrix],
  );

  /** Rows that are eligible but every stage is scheduled for later. */
  const pendingRows = useMemo(
    () =>
      (matrix?.rows ?? []).filter(
        (r) =>
          r.wallet_id != null &&
          r.stages.some((s) => s.eligible) &&
          !r.stages.some((s) => s.actionable),
      ),
    [matrix],
  );

  async function onRun(e: FormEvent) {
    e.preventDefault();
    if (selected.length === 0 || !chainId || !collection.trim()) return;
    setRunning(true);
    setErr(null);
    setMatrix(null);
    setManualSel([]);
    try {
      const result = await ipc<StageMatrixResult>("eligibility_matrix_run", {
        walletIds: selected,
        chainId: Number(chainId),
        collection: collection.trim(),
      });
      setMatrix(result);
      await load();
    } catch (e2) {
      setErr(String(e2));
    } finally {
      setRunning(false);
    }
  }

  const allVisibleSelected =
    visibleWallets.length > 0 && visibleWallets.every((w) => selected.includes(w.id));

  async function createDraftTasks(walletIds: number[], label: string) {
    if (!chainId || !collection.trim() || walletIds.length === 0) return;
    setQueueing(true);
    setErr(null);
    try {
      const result = await ipc<EnqueueBatchResult>("mint_opensea_enqueue", {
        args: {
          wallet_ids: walletIds,
          chain_id: Number(chainId),
          collection: collection.trim(),
          quantity: 1,
          token_id: "0",
          mode: "execute",
          draft: true, // store as draft — user schedules & runs on Minting page
        },
      });
      const drafted = result.tasks.length;
      const skipped = result.skipped.length;
      if (drafted === 0) {
        const reason =
          result.skipped
            .map((s) => `#${s.wallet_id}: ${s.reason}`)
            .join("; ")
            .slice(0, 400) || "Nothing created";
        setErr(reason);
        pushToast("Task creation failed", "error", reason);
        return;
      }
      pushToast(
        `${label} created`,
        "ok",
        skipped > 0
          ? `${drafted} draft task(s) · ${skipped} skipped — opening Minting`
          : `${drafted} draft task(s) — schedule & run on Minting`,
      );
      if (skipped > 0) {
        setErr(
          result.skipped
            .map((s) => `#${s.wallet_id}: ${s.reason}`)
            .join("; ")
            .slice(0, 400),
        );
      }
      setManualSel([]);
      // Navigate to Minting so the user can schedule and run.
      setTimeout(() => navigate("/minting"), 600);
    } catch (e) {
      setErr(String(e));
      pushToast("Task creation failed", "error", String(e).slice(0, 160));
    } finally {
      setQueueing(false);
    }
  }

  function onCreateAll() {
    void createDraftTasks(
      eligibleRows.map((r) => r.wallet_id as number),
      "Auto-create eligible",
    );
  }

  function onCreateManual() {
    const ids = manualSel
      .map((i) => matrix?.rows[i]?.wallet_id)
      .filter((id): id is number => id != null);
    void createDraftTasks(ids, "Manual create");
  }

  function toggleManual(idx: number) {
    setManualSel((s) =>
      s.includes(idx) ? s.filter((x) => x !== idx) : [...s, idx],
    );
  }

  const stageCols = matrix?.stage_names ?? [];

  return (
    <div className="p-6">
      <PageHeader
        suite="Operations"
        title="Eligibility Check"
        subtitle="Hybrid: OpenSea stages (GTD/FCFS/WL/Public) + SeaDrop on-chain fallback"
        action={
          matrix ? (
            <div className="flex gap-1.5">
              <button
                type="button"
                onClick={() => setViewMode(viewMode === "table" ? "cards" : "table")}
                className="flex items-center gap-1.5 rounded-lg border border-line px-3 py-1.5 text-[12px] text-muted hover:text-fg"
              >
                {viewMode === "table" ? (
                  <>
                    <LayoutGrid className="h-3.5 w-3.5" /> Cards
                  </>
                ) : (
                  <>
                    <Rows3 className="h-3.5 w-3.5" /> Table
                  </>
                )}
              </button>
              <button
                type="button"
                onClick={onCreateAll}
                disabled={queueing || eligibleRows.length === 0}
                className="flex items-center gap-1.5 rounded-lg bg-accent px-3 py-1.5 text-[12px] font-semibold text-white disabled:opacity-40"
              >
                {queueing ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Zap className="h-3.5 w-3.5" />
                )}
                Create tasks ({eligibleRows.length})
              </button>
            </div>
          ) : undefined
        }
      />

      {/* ── Input form ─────────────────────────────────────────── */}
      <form onSubmit={onRun} className="mb-4 space-y-3 rounded-[14px] border border-line bg-card p-4">
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
            <option value="">Chain...</option>
            {chains.map((c) => (
              <option key={c.id} value={c.chain_id}>
                {c.name}
              </option>
            ))}
          </select>
          <input
            placeholder="Collection address (0x...) or OpenSea slug"
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
            {running ? "Checking..." : `Run (${selected.length})`}
          </button>
        </div>
        {err ? <div className="text-[12px] text-danger">{err}</div> : null}
      </form>

      {/* ── Matrix results ─────────────────────────────────────── */}
      {matrix && viewMode === "table" ? (
        <div className="mb-4 overflow-x-auto rounded-[14px] border border-line bg-card">
          <div className="flex items-center justify-between border-b border-line px-4 py-2.5">
            <span className="text-[12px] font-semibold">
              {matrix.slug} — {eligibleRows.length}/{matrix.rows.length} ready
              {pendingRows.length > 0 ? ` · ${pendingRows.length} scheduled` : ""}
            </span>
            <div className="flex gap-1.5">
              <button
                type="button"
                onClick={onCreateManual}
                disabled={queueing || manualSel.length === 0}
                className="flex items-center gap-1.5 rounded-lg border border-line px-3 py-1.5 text-[12px] text-muted hover:text-fg disabled:opacity-40"
              >
                {queueing ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Zap className="h-3.5 w-3.5" />
                )}
                Create selected ({manualSel.length})
              </button>
            </div>
          </div>

          {pendingRows.length > 0 ? (
            <div className="flex items-center gap-2 border-b border-warn/30 bg-warn/5 px-4 py-2 text-[12px] text-warn">
              <Clock className="h-3.5 w-3.5 shrink-0" />
              <span>
                {pendingRows.length} wallet(s) eligible but stage not live yet —
                {" "}
                {pendingRows[0]?.stages.find((s) => s.eligible && !s.actionable)?.schedule_hint ?? "check schedule"}
                . Queue disabled until the stage opens.
              </span>
            </div>
          ) : null}

          {/* Table header */}
          <div className="grid border-b border-line bg-line/30 px-4 py-2 text-[11px] font-semibold uppercase tracking-wide text-muted"
            style={{
              gridTemplateColumns: `28px minmax(100px,1fr) minmax(140px,1.4fr) ${stageCols
                .map(() => "68px")
                .join(" ")} 70px`,
            }}
          >
            <div>#</div>
            <div>Slug</div>
            <div>Wallet</div>
            {stageCols.map((s) => (
              <div key={s} className="text-center">
                {stageLabel(s)}
              </div>
            ))}
            <div className="text-center">Queue</div>
          </div>

          {/* Table rows */}
          {matrix.rows.map((r, ri) => {
            const anyEligible = r.stages.some((s) => s.eligible);
            const stageMap = new Map(r.stages.map((s) => [s.stage_name, s]));
            return (
              <div
                key={`${r.wallet_id}-${ri}`}
                className={`grid items-center border-b border-line/60 px-4 py-2 text-[13px] last:border-0 hover:bg-line/30 ${
                  manualSel.includes(ri) ? "bg-accent/5" : ""
                }`}
                style={{
                  gridTemplateColumns: `28px minmax(100px,1fr) minmax(140px,1.4fr) ${stageCols
                    .map(() => "68px")
                    .join(" ")} 70px`,
                }}
              >
                <div className="text-muted text-[12px]">{ri + 1}</div>
                <div className="truncate font-mono text-[12px] text-muted" title={r.slug}>
                  {r.slug}
                </div>
                <div className="truncate font-mono text-[12px]" title={r.address}>
                  {shortAddress(r.address, 4)}
                </div>
                {stageCols.map((sc) => {
                  const cell = stageMap.get(sc);
                  if (!cell) {
                    return (
                      <div key={sc} className="text-center text-muted" title="Not checked for this stage">
                        <span className="text-[14px] opacity-30">—</span>
                      </div>
                    );
                  }
                  const title = !cell.eligible
                    ? `${sc}: not eligible`
                    : cell.actionable
                      ? `${sc}: eligible — live now, ready to queue`
                      : `${sc}: eligible but ${cell.schedule_hint}`;
                  return (
                    <div key={sc} className="flex justify-center" title={title}>
                      {cell.eligible ? (
                        cell.actionable ? (
                          <CheckCircle2 className="h-4.5 w-4.5 text-ok" />
                        ) : (
                          <span className="flex items-center gap-0.5 text-warn">
                            <Clock className="h-4 w-4" />
                          </span>
                        )
                      ) : (
                        <XCircle className="h-4.5 w-4.5 text-danger/70" />
                      )}
                    </div>
                  );
                })}
                <div className="flex justify-center">
                  {anyEligible ? (
                    <input
                      type="checkbox"
                      checked={manualSel.includes(ri)}
                      onChange={() => toggleManual(ri)}
                      className="h-4 w-4 cursor-pointer accent-accent"
                    />
                  ) : (
                    <span className="text-[12px] text-muted">—</span>
                  )}
                </div>
              </div>
            );
          })}

          {matrix.rows.some((r) => r.error) && (
            <div className="border-t border-line px-4 py-2 text-[12px] text-danger">
              {matrix.rows
                .filter((r) => r.error)
                .map((r) => `${shortAddress(r.address, 4)}: ${r.error}`)
                .join(" · ")}
            </div>
          )}
        </div>
      ) : null}

      {/* ── Cards view ─────────────────────────────────────────── */}
      {matrix && viewMode === "cards" ? (
        <div className="mb-4 rounded-[14px] border border-line bg-card p-4">
          <div className="mb-3 flex items-center justify-between">
            <div>
              <div className="text-[14px] font-semibold">{matrix.slug}</div>
              <div className="truncate font-mono text-[12px] text-muted" title={matrix.collection}>
                {matrix.collection}
              </div>
            </div>
            <span className="rounded-full bg-accent/10 px-2.5 py-1 text-[12px] text-accent">
              {eligibleRows.length}/{matrix.rows.length} eligible
            </span>
          </div>

          <div className="space-y-2">
            {matrix.rows.map((r, ri) => {
              const anyEligible = r.stages.some((s) => s.eligible);
              const stageMap = new Map(r.stages.map((s) => [s.stage_name, s]));
              return (
                <div
                  key={`${r.wallet_id}-${ri}`}
                  className={`rounded-lg border px-3 py-2.5 ${
                    anyEligible ? "border-ok/30 bg-ok/5" : "border-line"
                  }`}
                >
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <input
                        type="checkbox"
                        checked={manualSel.includes(ri)}
                        onChange={() => toggleManual(ri)}
                        disabled={!anyEligible}
                        className="h-3.5 w-3.5 accent-accent"
                      />
                      <span className="font-mono text-[12px]" title={r.address}>
                        {shortAddress(r.address, 6)}
                      </span>
                      {r.wallet_id != null && (
                        <span className="text-[11px] text-muted">#{r.wallet_id}</span>
                      )}
                    </div>
                    <div className="flex gap-3">
                      {stageCols.map((sc) => {
                        const cell = stageMap.get(sc);
                        if (!cell) return null;
                        return (
                          <div
                            key={sc}
                            className="flex items-center gap-1 text-[11px]"
                            title={`${stageLabel(sc)}: ${
                              !cell.eligible
                                ? "Not eligible"
                                : cell.actionable
                                  ? "Eligible — live"
                                  : `Eligible — ${cell.schedule_hint}`
                            }${cell.max_quantity ? ` (max ${cell.max_quantity})` : ""}`}
                          >
                            {cell.eligible ? (
                              cell.actionable ? (
                                <CheckCircle2 className="h-3.5 w-3.5 text-ok" />
                              ) : (
                                <Clock className="h-3.5 w-3.5 text-warn" />
                              )
                            ) : (
                              <XCircle className="h-3.5 w-3.5 text-danger/60" />
                            )}
                            <span className="text-muted">{stageLabel(sc)}</span>
                          </div>
                        );
                      })}
                    </div>
                  </div>
                  {r.error && (
                    <div className="mt-1 pl-6 text-[11px] text-danger">{r.error}</div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      ) : null}

      {/* ── Empty state ────────────────────────────────────────── */}
      {!matrix && !running ? (
        <div className="mb-4">
          <EmptyState
            title="No check yet"
            description="Select wallets, pick a chain and collection, then Run to see the stage matrix."
          />
        </div>
      ) : null}

      {running ? (
        <div className="mb-4 flex items-center gap-2 rounded-[14px] border border-line bg-card px-4 py-3 text-[13px] text-muted">
          <Loader2 className="h-4 w-4 animate-spin" />
          Checking eligibility across {selected.length} wallet(s)...
        </div>
      ) : null}

      {/* ── History ────────────────────────────────────────────── */}
      <div className="overflow-hidden rounded-[14px] border border-line bg-card">
        <div className="border-b border-line px-4 py-2 font-mono text-[10.5px] font-semibold uppercase tracking-[0.1em] text-muted">History</div>
        {history.length === 0 ? (
          <EmptyState title="No checks yet" description="Run an eligibility check to populate history." />
        ) : (
          <>
            <div className="grid grid-cols-[170px_1fr_1.2fr_70px_150px] gap-2 border-b border-line/60 px-4 py-2 font-mono text-[9.5px] font-semibold uppercase tracking-[0.1em] text-muted">
              <div>Wallet</div>
              <div>Collection</div>
              <div>Detail</div>
              <div>Result</div>
              <div>When</div>
            </div>
            {history.map((h) => {
              const w =
                h.wallet_id != null
                  ? wallets.find((x) => x.id === h.wallet_id)
                  : undefined;
              return (
                <div
                  key={h.id}
                  className="grid grid-cols-[170px_1fr_1.2fr_70px_150px] items-center gap-2 border-b border-line/60 px-4 py-2.5 text-[13px] last:border-0"
                >
                  <div
                    className="min-w-0 truncate text-[12px]"
                    title={
                      w
                        ? `${w.label} · ${w.address}`
                        : h.wallet_id != null
                          ? `wallet #${h.wallet_id}`
                          : undefined
                    }
                  >
                    {w ? w.label : h.wallet_id != null ? `#${h.wallet_id}` : "—"}
                  </div>
                  <div className="truncate font-mono text-[12px]">{h.collection}</div>
                  <div className="truncate text-muted">{h.detail || "—"}</div>
                  <div className={h.result ? "text-ok" : "text-muted"}>
                    {h.result ? "Yes" : "No"}
                  </div>
                  <div className="font-mono text-[10.5px] tabular-nums text-muted">{new Date(h.checked_at).toLocaleString()}</div>
                </div>
              );
            })}
          </>
        )}
      </div>
    </div>
  );
}
