import { useCallback, useEffect, useMemo, useState } from "react";
import { Activity as ActivityIcon, ChartNoAxesCombined, Clock, Flame, FolderOpen, KeyRound, Network, ShieldCheck, Wallet, Zap } from "lucide-react";
import { ipc } from "../lib/ipc";
import type { ActivityRow, ModuleStatusItem, StatsOverview } from "../lib/types";
import { Panel, StatCard, StatusDot } from "../components/ui";
import { greeting, shortAddress } from "../lib/utils";
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

function MiniStat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg border border-line bg-bg px-2 py-1.5 text-center">
      <div className="text-[10px] uppercase tracking-wide text-muted">{label}</div>
      <div className="truncate text-[12px] font-medium text-fg">{value}</div>
    </div>
  );
}

export function DashboardPage({ onQuickTask }: { onQuickTask?: () => void }) {
  const name = useAppStore((s) => s.profileName);
  const unlocked = useVaultStore((s) => s.status?.unlocked ?? false);
  const lock = useVaultStore((s) => s.lock);
  const refreshVault = useVaultStore((s) => s.refresh);
  const { groups, load: loadWallets } = useWalletStore();
  const nav = useNavigate();
  const [stats, setStats] = useState<StatsOverview | null>(null);
  const [modules, setModules] = useState<ModuleStatusItem[]>([]);
  const [recent, setRecent] = useState<ActivityRow[]>([]);
  const [loadErr, setLoadErr] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [s, m, a] = await Promise.all([
        ipc<StatsOverview>("stats_overview"),
        ipc<ModuleStatusItem[]>("module_status"),
        ipc<ActivityRow[]>("activity_list", { limit: 8 }),
      ]);
      setStats(s);
      setModules(m);
      setRecent(a);
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
  const netNum = col?.net_eth != null ? Number(col.net_eth) : null;
  const netCls = netNum == null ? "text-muted" : netNum >= 0 ? "text-ok" : "text-danger";
  const flowCls = flow == null ? "text-muted" : Number(flow) < 0 ? "text-danger" : "text-ok";
  const hasData = !!(col || (pf && pf.pairs > 0));

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
            <div className="mb-1 text-[12px] text-muted">Suite / Dashboard</div>
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
          <Panel
            subtitle="All-time PnL"
            title="Portfolio performance"
            right={
              <span className="text-[12px] text-muted">
                {lastAt ? `Updated ${agoLabel(lastAt)}` : "No scans yet"}
              </span>
            }
            footer={
              <>
                <span>
                  {col
                    ? "Source: Collection PnL"
                    : pf && pf.pairs > 0
                      ? "Source: portfolio scans"
                      : "Awaiting first scan"}
                </span>
                <button
                  onClick={() => nav("/pnl")}
                  className="text-[12px] text-accent hover:underline"
                >
                  Open PnL →
                </button>
              </>
            }
            className="min-h-[260px]"
          >
            {hasData ? (
              <div className="flex h-full flex-col gap-3">
                {col ? (
                  <div>
                    <div className="flex items-center gap-2 text-[12px] text-muted">
                      <ChartNoAxesCombined className="h-3.5 w-3.5 text-accent" />
                      Net PnL · {shortAddress(col.contract, 6)}
                      {col.floor_source ? (
                        <span className="rounded-full border border-line bg-bg px-1.5 py-0.5 text-[10px]">
                          floor: {col.floor_source}
                        </span>
                      ) : null}
                    </div>
                    <div className={`mt-1 text-[30px] font-semibold tracking-tight ${netCls}`}>
                      {netNum != null
                        ? `${netNum > 0 ? "+" : ""}${col.net_eth} ${col.native_symbol}`
                        : `n/a ${col.native_symbol}`}
                    </div>
                    {col.roi_pct != null ? (
                      <div className={`text-[12px] ${netCls}`}>
                        ROI {col.roi_pct >= 0 ? "+" : ""}
                        {col.roi_pct.toFixed(1)}%
                      </div>
                    ) : null}
                  </div>
                ) : null}

                <div className="grid grid-cols-4 gap-2">
                  {col ? (
                    <>
                      <MiniStat label="Spent" value={`${col.spent_eth} ${col.native_symbol}`} />
                      <MiniStat label="Holding" value={`${col.holding} / ${col.balance}`} />
                      <MiniStat label="Floor" value={col.floor_eth ? `${col.floor_eth} ETH` : "n/a"} />
                      <MiniStat label="Gas" value={`${col.gas_eth} ${col.native_symbol}`} />
                    </>
                  ) : pf ? (
                    <>
                      <MiniStat label="Net flow" value={flow != null ? `${flow} ETH` : "n/a"} />
                      <MiniStat label="Scans" value={`${pf.pairs}`} />
                      <MiniStat label="Wallets" value={`${pf.wallets}`} />
                      <MiniStat label="Last" value={pf.latest_at ? agoLabel(pf.latest_at) : "—"} />
                    </>
                  ) : null}
                </div>

                {col && pf && pf.pairs > 0 ? (
                  <div className="flex items-center justify-between rounded-lg border border-line bg-bg px-3 py-2 text-[12px]">
                    <span className="text-muted">
                      Portfolio net flow · {pf.wallets} wallet(s), {pf.pairs} chain scan(s)
                    </span>
                    <span className={flowCls}>
                      {flow != null ? `${flow} ETH` : "n/a"}
                    </span>
                  </div>
                ) : null}
              </div>
            ) : (
              <div className="flex h-full min-h-[160px] flex-col items-center justify-center gap-2 px-6 text-center">
                <div className="flex h-10 w-10 items-center justify-center rounded-full border border-line text-accent">
                  <ChartNoAxesCombined className="h-5 w-5" />
                </div>
                <div className="text-[14px] font-medium">No PnL scans yet</div>
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
          </Panel>

          <Panel subtitle="System" title="Module status">
            <div className="divide-y divide-line/60">
              {modules.map((m) => (
                <div key={m.label} className="flex items-center justify-between px-5 py-3 text-[13px]">
                  <div className="flex items-center gap-2.5">
                    <StatusDot ok={m.ok} />
                    {m.label}
                  </div>
                  <div className="text-muted">{m.detail}</div>
                </div>
              ))}
              <div className="flex items-center gap-2 px-5 py-3 text-[12px] text-muted">
                <ShieldCheck className="h-3.5 w-3.5" /> Secrets stay on this device
              </div>
            </div>
          </Panel>
        </div>

        <div className="mb-3 flex items-baseline justify-between">
          <div>
            <div className="text-[12px] text-muted">Workspace</div>
            <div className="text-[15px] font-semibold">Quick statistics</div>
          </div>
          <div className="text-[12px] text-muted">Live local state</div>
        </div>

        <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
          <StatCard
            label="Wallets"
            value={stats?.wallets ?? 0}
            hint={`${stats?.wallets ?? 0} stored locally`}
            icon={<Wallet className="h-4 w-4" />}
          />
          <StatCard
            label="Groups"
            value={groups.length}
            hint={
              groups.length === 0
                ? "No groups yet"
                : `${groups.reduce((n, g) => n + g.wallet_count, 0)} wallets grouped`
            }
            icon={<FolderOpen className="h-4 w-4" />}
          />
          <StatCard
            label="Tasks"
            value={stats?.tasks ?? 0}
            hint={(stats?.tasks ?? 0) === 0 ? "No tasks configured" : "Active queue"}
            icon={<Flame className="h-4 w-4" />}
          />
          <StatCard
            label="RPC endpoints"
            value={stats?.rpc_endpoints ?? 0}
            hint="Custom endpoints configured"
            icon={<Network className="h-4 w-4" />}
          />
          <StatCard
            label="Eligible checks"
            value={stats?.eligible_checks ?? 0}
            hint={(stats?.eligible_checks ?? 0) === 0 ? "No successful checks yet" : "Recorded locally"}
            icon={<ShieldCheck className="h-4 w-4" />}
          />
        </div>

        <div className="mt-6 grid grid-cols-1 gap-4 lg:grid-cols-2">
          <Panel title="Recent activity" right={<Clock className="h-4 w-4 text-muted" />}>
            <div className="divide-y divide-line/60">
              {recent.length === 0 ? (
                <div className="px-5 py-6 text-center text-[13px] text-muted">No activity yet</div>
              ) : (
                recent.map((a) => (
                  <div key={a.id} className="flex items-start gap-3 px-5 py-2.5 text-[13px]">
                    <StatusDot ok={!!a.ok} />
                    <div className="min-w-0 flex-1">
                      <div className="truncate">{a.summary}</div>
                      <div className="text-[11px] text-muted">
                        {a.kind} · {new Date(a.created_at).toLocaleTimeString()}
                      </div>
                    </div>
                  </div>
                ))
              )}
            </div>
          </Panel>

          <Panel title="Session" right={<KeyRound className="h-4 w-4 text-muted" />}>
            <div className="space-y-3 p-5 text-[13px]">
              <div className="flex items-center justify-between">
                <span className="text-muted">Vault</span>
                <span className={unlocked ? "text-ok" : "text-warn"}>
                  {unlocked ? "Unlocked" : "Locked"}
                </span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted">Profile</span>
                <span>{name}</span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted">API providers</span>
                <span>{stats?.api_providers ?? 0}</span>
              </div>
              {unlocked ? (
                <button
                  onClick={() => void lock()}
                  className="mt-2 w-full rounded-lg border border-line bg-bg py-2 text-[13px] text-fg hover:border-danger/50 hover:text-danger"
                >
                  Lock vault now
                </button>
              ) : (
                <button
                  onClick={() => void refreshVault()}
                  className="mt-2 w-full rounded-lg bg-accent py-2 text-[13px] font-semibold text-white"
                >
                  Unlock vault
                </button>
              )}
            </div>
          </Panel>
        </div>

        <div className="mt-4 flex items-center gap-2 text-[12px] text-muted">
          <ActivityIcon className="h-3.5 w-3.5" />
          {stats?.activity_records ?? 0} ledger records · local device
        </div>
      </div>
    </div>
  );
}
