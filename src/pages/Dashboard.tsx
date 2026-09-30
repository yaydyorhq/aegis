import { useCallback, useEffect, useMemo, useState } from "react";
import { ChartNoAxesCombined, Clock, Flame, FolderOpen, KeyRound, Network, ShieldCheck, TrendingDown, TrendingUp, Wallet, Zap } from "lucide-react";
import { ipc } from "../lib/ipc";
import type { ActivityRow, CollectionPnlPoint, ModuleStatusItem, PortfolioLive, StatsOverview } from "../lib/types";
import { StatusDot } from "../components/ui";
import { cn, formatEth, greeting, shortAddress } from "../lib/utils";
import { useAppStore, useVaultStore, useWalletStore } from "../store/app";
import { useNavigate } from "react-router-dom";

function agoLabel(ts: number): string {
  const mins = Math.floor((Date.now() - ts) / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

// ── Terminal primitives ─────────────────────────────────────────────
// Bordered board with an uppercase micro-label strip. Everything data-ish
// below it is expected to use font-mono + tabular-nums.

function Board({
  label,
  right,
  footer,
  children,
  className,
}: {
  label: React.ReactNode;
  right?: React.ReactNode;
  footer?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section
      className={cn(
        "flex flex-col overflow-hidden rounded-[14px] border border-line bg-card",
        className,
      )}
    >
      <div className="flex items-center justify-between gap-3 border-b border-line px-4 py-2">
        <span className="truncate text-[10.5px] font-semibold uppercase tracking-[0.1em] text-muted">
          {label}
        </span>
        {right}
      </div>
      <div className="min-h-0 flex-1">{children}</div>
      {footer ? (
        <div className="flex items-center justify-between gap-3 border-t border-line px-4 py-2">
          {footer}
        </div>
      ) : null}
    </section>
  );
}

function BoardLabel({ children }: { children: React.ReactNode }) {
  return (
    <span className="font-mono text-[10px] uppercase tracking-[0.08em] text-muted">
      {children}
    </span>
  );
}

/** One cell of a hairline stat strip — label micro-caps, mono value. */
function TermStat({
  label,
  value,
  title,
}: {
  label: string;
  value: string;
  title?: string;
}) {
  return (
    <div className="min-w-0 bg-card px-3.5 py-2.5">
      <div className="text-[9.5px] font-semibold uppercase tracking-[0.1em] text-muted">
        {label}
      </div>
      <div
        className="mt-1 truncate font-mono text-[13px] tabular-nums text-fg"
        title={title}
      >
        {value}
      </div>
    </div>
  );
}

function DeltaChip({ value, label }: { value: number; label: string }) {
  const up = value >= 0;
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded border px-1.5 py-0.5 font-mono text-[11px] tabular-nums",
        up ? "border-ok/40 text-ok" : "border-danger/40 text-danger",
      )}
    >
      {up ? <TrendingUp className="h-3 w-3" /> : <TrendingDown className="h-3 w-3" />}
      {label} {up ? "+" : ""}
      {formatEth(String(value))}
    </span>
  );
}

/** Net-worth trail across recent Collection PnL scans — inline SVG, trend-colored. */
function Sparkline({ points, className }: { points: number[]; className?: string }) {
  const w = 100;
  const h = 32;
  const min = Math.min(...points);
  const max = Math.max(...points);
  const span = max - min || Math.abs(max) || 1;
  const step = w / (points.length - 1);
  const coords = points.map((p, i) => {
    const x = i * step;
    const y = h - 2 - ((p - min) / span) * (h - 4);
    return [x, y] as const;
  });
  const line = coords.map(([x, y]) => `${x.toFixed(2)},${y.toFixed(2)}`).join(" ");
  const area = `0,${h} ${line} ${w},${h}`;
  const [x, y] = coords[coords.length - 1];
  const up = points[points.length - 1] >= points[0];
  return (
    <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className={className}>
      <polygon points={area} className="fill-accent/10" />
      <polyline
        points={line}
        fill="none"
        strokeWidth={1.25}
        vectorEffect="non-scaling-stroke"
        className={up ? "stroke-ok" : "stroke-danger"}
      />
      <circle cx={x} cy={y} r={1.2} className={up ? "fill-ok" : "fill-danger"} />
    </svg>
  );
}

export function DashboardPage({ onQuickTask }: { onQuickTask?: () => void }) {
  const name = useAppStore((s) => s.profileName);
  const unlocked = useVaultStore((s) => s.status?.unlocked ?? false);
  const lock = useVaultStore((s) => s.lock);
  const refreshVault = useVaultStore((s) => s.refresh);
  const { wallets, groups, load: loadWallets } = useWalletStore();
  const nav = useNavigate();
  const [stats, setStats] = useState<StatsOverview | null>(null);
  const [modules, setModules] = useState<ModuleStatusItem[]>([]);
  const [recent, setRecent] = useState<ActivityRow[]>([]);
  const [pnlHistory, setPnlHistory] = useState<CollectionPnlPoint[]>([]);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  /** Realtime native balances (separate, slower poll — RPC-friendly). */
  const [live, setLive] = useState<PortfolioLive | null>(null);
  const [liveOn, setLiveOn] = useState(true);
  /** Which chain the LIVE WALLETS board shows; null = first in response. */
  const [liveChainId, setLiveChainId] = useState<number | null>(null);

  const fetchLive = useCallback(async () => {
    try {
      setLive(await ipc<PortfolioLive>("portfolio_live"));
    } catch {
      /* keep the last snapshot on the board; its age shows in the footer */
    }
  }, []);

  useEffect(() => {
    if (!liveOn) return;
    let stopped = false;
    const tick = () => {
      if (!stopped && !document.hidden) void fetchLive();
    };
    void tick();
    const t = setInterval(tick, 20_000);
    // Hidden window → skip ticks; on return, refresh immediately.
    const onVisible = () => {
      if (!document.hidden) tick();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      stopped = true;
      clearInterval(t);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [liveOn, fetchLive]);

  const load = useCallback(async () => {
    try {
      const [s, m, a, h] = await Promise.all([
        ipc<StatsOverview>("stats_overview"),
        ipc<ModuleStatusItem[]>("module_status"),
        ipc<ActivityRow[]>("activity_list", { limit: 8 }),
        ipc<CollectionPnlPoint[]>("collection_pnl_history", { limit: 30 }),
      ]);
      setStats(s);
      setModules(m);
      setRecent(a);
      setPnlHistory(h);
      setLoadErr(null);
      void loadWallets();
    } catch (e) {
      setLoadErr(String(e));
    }
  }, [loadWallets]);

  useEffect(() => {
    void load();
    const t = setInterval(() => void load(), 5000);
    return () => clearInterval(t);
  }, [load]);

  const g = useMemo(() => greeting(), []);

  const pf = stats?.portfolio ?? null;
  const col = pf?.collection ?? null;
  const flow = pf?.net_flow_eth ?? null;
  const lastAt = col?.scanned_at ?? pf?.latest_at ?? null;
  const netRaw = col?.net_eth ?? null;
  const netNum = netRaw != null ? Number(netRaw) : null;
  const netStr =
    netRaw != null
      ? netNum != null && netNum > 0 && !netRaw.startsWith("+")
        ? `+${formatEth(netRaw)}`
        : formatEth(netRaw)
      : null;
  const netCls = netNum == null ? "text-muted" : netNum >= 0 ? "text-ok" : "text-danger";
  const flowStr =
    flow != null
      ? Number(flow) > 0 && !flow.startsWith("+")
        ? `+${formatEth(flow)}`
        : formatEth(flow)
      : null;
  const flowCls = flow == null ? "text-muted" : Number(flow) < 0 ? "text-danger" : "text-ok";
  const hasData = !!(col || (pf && pf.pairs > 0));
  const spark = useMemo(
    () =>
      pnlHistory
        .map((p) => (p.net_eth != null ? Number(p.net_eth) : Number.NaN))
        .filter((x) => Number.isFinite(x)),
    [pnlHistory],
  );
  const selectedLiveChain =
    live?.chains.find((c) => c.chain_id === liveChainId) ?? live?.chains[0] ?? null;
  const walletById = useMemo(() => {
    const m = new Map<number, (typeof wallets)[number]>();
    for (const w of wallets) m.set(w.id, w);
    return m;
  }, [wallets]);
  const liveFailedTotal = useMemo(
    () => (live ? live.chains.reduce((n, c) => n + c.failed, 0) : 0),
    [live],
  );
  /** Change vs the previous scan — the terminal-style Δ chip. */
  const lastDelta = useMemo(() => {
    if (spark.length < 2) return null;
    return spark[spark.length - 1] - spark[spark.length - 2];
  }, [spark]);

  return (
    <div className="relative min-h-full">
      <div
        className="pointer-events-none absolute inset-0 opacity-[0.18]"
        style={{
          background:
            "radial-gradient(1000px 400px at 70% -10%, #4f7cff33, transparent), radial-gradient(800px 300px at 10% 0%, #f5a52422, transparent)",
        }}
      />
      <div className="relative p-6">
        {loadErr ? (
          <div className="mb-4 rounded-lg border border-danger/30 bg-danger/10 px-4 py-2.5 text-[12px] text-danger">
            Failed to load dashboard: {loadErr}
          </div>
        ) : null}
        <div className="mb-6 flex items-start justify-between gap-4">
          <div>
            <div className="mb-1 font-mono text-[11px] uppercase tracking-[0.1em] text-muted">
              Suite / Dashboard
            </div>
            <h1 className="text-[26px] font-semibold tracking-tight">
              Hi, {g} {name}
            </h1>
            <p className="mt-1 text-[13px] text-muted">
              Your local command center for EVM wallets, tasks, and NFT operations.
            </p>
          </div>
          <button
            onClick={() => (onQuickTask ? onQuickTask() : nav("/minting"))}
            className="flex items-center gap-1.5 rounded-xl bg-fg px-4 py-2.5 text-[13px] font-semibold text-bg shadow-lg hover:opacity-90"
          >
            <Zap className="h-4 w-4" /> Quick task
          </button>
        </div>

        <div className="mb-6 grid grid-cols-1 gap-4 lg:grid-cols-[1fr_300px]">
          {/* ── Portfolio board ── */}
          <Board
            label={
              col ? (
                <>
                  Portfolio · PnL ·{" "}
                  <span className="font-mono normal-case tracking-normal text-fg">
                    {shortAddress(col.contract, 8)}
                  </span>
                </>
              ) : (
                "Portfolio · PnL"
              )
            }
            right={
              <BoardLabel>
                {lastAt ? `UPDATED ${agoLabel(lastAt).toUpperCase()}` : "NO SCANS YET"}
              </BoardLabel>
            }
            footer={
              <>
                <BoardLabel>
                  {col
                    ? `SOURCE: COLLECTION PNL · FLOOR ${col.floor_source ? col.floor_source.toUpperCase() : "N/A"}`
                    : "SOURCE: PORTFOLIO SCANS"}
                </BoardLabel>
                <button
                  onClick={() => nav("/pnl")}
                  className="font-mono text-[10px] uppercase tracking-[0.08em] text-accent hover:underline"
                >
                  Open PnL →
                </button>
              </>
            }
            className="min-h-[260px]"
          >
            {hasData ? (
              <div className="flex h-full flex-col">
                {col ? (
                  <>
                    <div className="flex flex-wrap items-end justify-between gap-x-4 gap-y-2 px-4 pb-3 pt-3">
                      <div className="min-w-0">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="text-[9.5px] font-semibold uppercase tracking-[0.1em] text-muted">
                            Net PnL
                          </span>
                          {col.roi_pct != null ? (
                            <span
                              className={cn(
                                "inline-flex items-center gap-1 rounded border px-1.5 py-0.5 font-mono text-[10.5px] tabular-nums",
                                col.roi_pct >= 0
                                  ? "border-ok/40 text-ok"
                                  : "border-danger/40 text-danger",
                              )}
                            >
                              {col.roi_pct >= 0 ? (
                                <TrendingUp className="h-3 w-3" />
                              ) : (
                                <TrendingDown className="h-3 w-3" />
                              )}
                              {col.roi_pct >= 0 ? "+" : ""}
                              {col.roi_pct.toFixed(1)}% ROI
                            </span>
                          ) : null}
                          {lastDelta != null ? (
                            <DeltaChip value={lastDelta} label="Δ SCAN" />
                          ) : null}
                        </div>
                        <div className="mt-1.5 flex flex-wrap items-baseline gap-x-2.5">
                          <span
                            className={cn(
                              "font-mono text-[34px] font-semibold leading-none tracking-tight tabular-nums",
                              netCls,
                            )}
                            title={
                              netRaw != null
                                ? `${netRaw} ${col.native_symbol} (exact)`
                                : undefined
                            }
                          >
                            {netStr ?? "N/A"}
                          </span>
                          <span className="font-mono text-[12px] uppercase text-muted">
                            {col.native_symbol}
                          </span>
                        </div>
                      </div>
                      <div className="text-right">
                        <div className="text-[9.5px] font-semibold uppercase tracking-[0.1em] text-muted">
                          Floor
                        </div>
                        <div className="mt-1 font-mono text-[13px] tabular-nums">
                          {col.floor_eth
                            ? `${formatEth(col.floor_eth)} ${col.native_symbol}`
                            : "N/A"}
                        </div>
                      </div>
                    </div>

                    {spark.length >= 2 ? (
                      <div
                        className="border-y border-line bg-bg/40 px-1"
                        title={`Net worth across last ${spark.length} scans`}
                      >
                        <Sparkline points={spark} className="h-16 w-full" />
                      </div>
                    ) : null}

                    <div className="grid grid-cols-2 gap-px bg-line md:grid-cols-5">
                      <TermStat
                        label="Spent"
                        value={`${formatEth(col.spent_eth)} ${col.native_symbol}`}
                        title={col.spent_eth}
                      />
                      <TermStat label="Holding" value={`${col.holding} / ${col.balance}`} />
                      <TermStat
                        label="Gas"
                        value={`${formatEth(col.gas_eth)} ${col.native_symbol}`}
                        title={col.gas_eth}
                      />
                      <TermStat label="Wallets" value={String(col.wallets)} />
                      <TermStat
                        label="Scans"
                        value={String(pf?.pairs ?? 0)}
                        title={pf?.latest_at ? new Date(pf.latest_at).toLocaleString() : undefined}
                      />
                    </div>

                    {pf && pf.pairs > 0 ? (
                      <div className="flex items-center justify-between px-4 py-2.5 text-[12px]">
                        <span className="text-muted">
                          Portfolio net flow ·{" "}
                          <span className="font-mono tabular-nums">
                            {pf.wallets} WALLET(S) · {pf.pairs} SCAN(S)
                          </span>
                        </span>
                        <span className={cn("font-mono tabular-nums", flowCls)}>
                          {flowStr != null ? `${flowStr} ETH` : "N/A"}
                        </span>
                      </div>
                    ) : null}
                  </>
                ) : pf ? (
                  <div className="grid grid-cols-2 gap-px bg-line md:grid-cols-4">
                    <TermStat label="Net flow" value={flowStr != null ? `${flowStr} ETH` : "N/A"} />
                    <TermStat label="Scans" value={String(pf.pairs)} />
                    <TermStat label="Wallets" value={String(pf.wallets)} />
                    <TermStat label="Last" value={pf.latest_at ? agoLabel(pf.latest_at) : "—"} />
                  </div>
                ) : null}
              </div>
            ) : (
              <div className="flex h-full min-h-[160px] flex-col items-center justify-center gap-2 px-6 text-center">
                <div className="flex h-10 w-10 items-center justify-center rounded-full border border-line text-accent">
                  <ChartNoAxesCombined className="h-5 w-5" />
                </div>
                <div className="font-mono text-[12px] uppercase tracking-[0.1em]">
                  No PnL scans yet
                </div>
                <div className="max-w-sm text-[12px] text-muted">
                  Run a Collection PnL scan (or a portfolio net-flow scan) and its
                  results will show up here automatically.
                </div>
                <button
                  onClick={() => nav("/pnl")}
                  className="mt-1 rounded-lg bg-accent px-3.5 py-2 text-[13px] font-semibold text-white hover:opacity-90"
                >
                  Open PnL scanner →
                </button>
              </div>
            )}
          </Board>

          {/* ── Right column: system + live wallets ── */}
          <div className="flex min-w-0 flex-col gap-4">
          <Board
            label="System status"
            right={<ShieldCheck className="h-3.5 w-3.5 text-muted" />}
            footer={
              <BoardLabel>
                <ShieldCheck className="mr-1 inline h-3 w-3" />
                SECRETS STAY ON THIS DEVICE
              </BoardLabel>
            }
          >
            <div className="divide-y divide-line/60">
              {modules.map((m) => (
                <div
                  key={m.label}
                  className="flex items-center justify-between gap-2 px-4 py-[9px] text-[12.5px]"
                >
                  <div className="flex min-w-0 items-center gap-2.5">
                    <StatusDot ok={m.ok} />
                    <span className="truncate">{m.label}</span>
                  </div>
                  <span className="shrink-0 font-mono text-[11.5px] tabular-nums text-muted">
                    {m.detail}
                  </span>
                </div>
              ))}
            </div>
          </Board>

          <Board
            label={
              <span className="flex items-center gap-1.5">
                <StatusDot tone={liveOn ? "info" : "idle"} pulse={liveOn} />
                Live wallets
                {live ? (
                  <span className="font-mono normal-case tracking-normal">
                    · {live.chains.length} chains
                  </span>
                ) : null}
              </span>
            }
            right={
              <span
                className="font-mono text-[11.5px] tabular-nums text-fg"
                title={
                  live && selectedLiveChain
                    ? `${selectedLiveChain.chain_name} total · updated ${agoLabel(live.fetched_at)}`
                    : undefined
                }
              >
                {selectedLiveChain
                  ? `Σ ${formatEth(selectedLiveChain.total_eth)} ${selectedLiveChain.native_symbol}`
                  : "Σ —"}
              </span>
            }
            footer={
              <>
                <BoardLabel>
                  {live
                    ? `UPDATED ${agoLabel(live.fetched_at).toUpperCase()}${liveFailedTotal > 0 ? ` · ${liveFailedTotal} RPC FAILED` : ""}`
                    : "WAITING FOR FIRST POLL"}
                </BoardLabel>
                <button
                  onClick={() => setLiveOn((v) => !v)}
                  className="font-mono text-[10px] uppercase tracking-[0.08em] text-accent hover:underline"
                >
                  {liveOn ? "Pause" : "Resume"}
                </button>
              </>
            }
          >
            {live != null && live.chains.length > 0 ? (
              <div className="flex flex-wrap gap-1 border-b border-line/60 px-3 py-2">
                {live.chains.map((c) => (
                  <button
                    key={c.chain_id}
                    onClick={() => setLiveChainId(c.chain_id)}
                    className={cn(
                      "rounded-full border px-2 py-0.5 font-mono text-[10px] uppercase tracking-[0.06em] transition-colors",
                      selectedLiveChain?.chain_id === c.chain_id
                        ? "border-accent bg-accent/10 text-accent"
                        : "border-line text-muted hover:text-fg",
                    )}
                  >
                    {c.chain_name}
                    {c.failed > 0 ? ` ·${c.failed}` : ""}
                  </button>
                ))}
              </div>
            ) : null}
            <div className="max-h-[232px] divide-y divide-line/60 overflow-y-auto">
              {live == null || selectedLiveChain == null ? (
                <div className="px-4 py-6 text-center font-mono text-[11px] uppercase tracking-[0.08em] text-muted">
                  {live == null ? "Loading…" : "No enabled chains"}
                </div>
              ) : selectedLiveChain.balances.length === 0 ? (
                <div className="px-4 py-6 text-center font-mono text-[11px] uppercase tracking-[0.08em] text-muted">
                  No wallets yet
                </div>
              ) : (
                selectedLiveChain.balances.map((b) => {
                  const w = walletById.get(b.wallet_id);
                  return (
                    <div
                      key={b.wallet_id}
                      className="flex items-center justify-between gap-2 px-4 py-2"
                    >
                      <div className="min-w-0">
                        <div className="truncate text-[12.5px]">
                          {w ? w.label : `#${b.wallet_id}`}
                        </div>
                        <div className="font-mono text-[10px] text-muted">
                          {w ? shortAddress(w.address, 4) : "—"}
                        </div>
                      </div>
                      <div
                        className={cn(
                          "shrink-0 font-mono text-[12.5px] tabular-nums",
                          b.balance_eth == null && "text-muted",
                        )}
                      >
                        {b.balance_eth != null
                          ? `${formatEth(b.balance_eth)} ${selectedLiveChain.native_symbol}`
                          : "—"}
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          </Board>
          </div>
        </div>

        {/* ── Ops board ── */}
        <div className="mb-3 flex items-baseline justify-between">
          <div>
            <div className="text-[12px] text-muted">Workspace</div>
            <div className="text-[15px] font-semibold">Quick statistics</div>
          </div>
          <BoardLabel>Live local state</BoardLabel>
        </div>

        <div className="mb-6 grid grid-cols-2 gap-px overflow-hidden rounded-[14px] border border-line bg-line lg:grid-cols-5">
          {(
            [
              {
                label: "Wallets",
                value: stats?.wallets ?? 0,
                hint: "stored locally",
                icon: <Wallet className="h-4 w-4" />,
                to: "/wallets",
              },
              {
                label: "Groups",
                value: groups.length,
                hint:
                  groups.length === 0
                    ? "no groups yet"
                    : `${groups.reduce((n, g) => n + g.wallet_count, 0)} wallets grouped`,
                icon: <FolderOpen className="h-4 w-4" />,
                to: "/wallets",
              },
              {
                label: "Tasks",
                value: stats?.tasks ?? 0,
                hint: (stats?.tasks ?? 0) === 0 ? "no active queue" : "active queue",
                icon: <Flame className="h-4 w-4" />,
                to: "/minting",
              },
              {
                label: "RPC endpoints",
                value: stats?.rpc_endpoints ?? 0,
                hint: "enabled chains",
                icon: <Network className="h-4 w-4" />,
                to: "/chains",
              },
              {
                label: "Eligible checks",
                value: stats?.eligible_checks ?? 0,
                hint: "recorded locally",
                icon: <ShieldCheck className="h-4 w-4" />,
                to: "/eligible",
              },
            ] as const
          ).map((c) => (
            <button
              key={c.label}
              onClick={() => nav(c.to)}
              className="bg-card p-4 text-left transition-colors hover:bg-line/40"
            >
              <div className="flex items-start justify-between">
                <div className="text-[9.5px] font-semibold uppercase tracking-[0.1em] text-muted">
                  {c.label}
                </div>
                <div className="text-muted">{c.icon}</div>
              </div>
              <div className="mt-2 font-mono text-[26px] font-semibold leading-none tabular-nums text-fg">
                {c.value}
              </div>
              <div className="mt-1.5 truncate font-mono text-[10px] uppercase tracking-[0.06em] text-muted">
                {c.hint}
              </div>
            </button>
          ))}
        </div>

        <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
          <Board
            label="Recent activity"
            right={<Clock className="h-3.5 w-3.5 text-muted" />}
            footer={<BoardLabel>APPEND-ONLY LOCAL LEDGER</BoardLabel>}
          >
            <div className="divide-y divide-line/60">
              {recent.length === 0 ? (
                <div className="px-4 py-6 text-center font-mono text-[11px] uppercase tracking-[0.08em] text-muted">
                  No activity yet
                </div>
              ) : (
                recent.map((a) => (
                  <div key={a.id} className="flex items-start gap-3 px-4 py-2.5 text-[13px]">
                    <StatusDot ok={!!a.ok} />
                    <div className="min-w-0 flex-1">
                      <div className="truncate">{a.summary}</div>
                      <div className="truncate font-mono text-[10.5px] text-muted">
                        {a.kind} · {new Date(a.created_at).toLocaleTimeString()}
                      </div>
                    </div>
                  </div>
                ))
              )}
            </div>
          </Board>

          <Board
            label="Session"
            right={<KeyRound className="h-3.5 w-3.5 text-muted" />}
            footer={<BoardLabel>AEGIS 0.1.0 · TAURI 2 + REACT + SQLITE</BoardLabel>}
          >
            <div className="space-y-1 p-2">
              {[
                ["Vault", unlocked ? "Unlocked" : "Locked"],
                ["Profile", name],
                ["API providers", String(stats?.api_providers ?? 0)],
                ["Activity records", String(stats?.activity_records ?? 0)],
              ].map(([k, v]) => (
                <div
                  key={k}
                  className="flex items-center justify-between rounded-lg px-2.5 py-2 text-[12.5px] hover:bg-line/40"
                >
                  <span className="text-muted">{k}</span>
                  <span
                    className={cn(
                      "font-mono tabular-nums",
                      k === "Vault" ? (unlocked ? "text-ok" : "text-warn") : "text-fg",
                    )}
                  >
                    {v}
                  </span>
                </div>
              ))}
              {unlocked ? (
                <button
                  onClick={() => void lock()}
                  className="mt-1 w-full rounded-lg border border-line bg-bg py-2 font-mono text-[11px] uppercase tracking-[0.08em] text-fg hover:border-danger/50 hover:text-danger"
                >
                  Lock vault now
                </button>
              ) : (
                <button
                  onClick={() => void refreshVault()}
                  className="mt-1 w-full rounded-lg bg-accent py-2 font-mono text-[11px] font-semibold uppercase tracking-[0.08em] text-white"
                >
                  Unlock vault
                </button>
              )}
            </div>
          </Board>
        </div>
      </div>
    </div>
  );
}
