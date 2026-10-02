import { useCallback, useEffect, useMemo, useState } from "react";
import { ChartNoAxesCombined, History } from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { ipc } from "../lib/ipc";
import type {
  ChainRow,
  CollectionPnlResult,
  CollectionPnlRow,
  PnlHistoryRow,
  PnlResult,
} from "../lib/types";
import { EmptyState, PageHeader } from "../components/ui";
import { formatEth, shortAddress } from "../lib/utils";
import {
  filterWalletsByGroup,
  groupNameMap,
  useWalletStore,
} from "../store/app";

type GroupFilter = number | "all" | "ungrouped";
type PnlTab = "collection" | "portfolio";

function flowClass(v: string): string {
  if (v.startsWith("+")) return "text-ok";
  if (v.startsWith("-")) return "text-danger";
  return "text-muted";
}

function fmtTime(ms: number): string {
  return new Date(ms).toLocaleString();
}

const PHASE_LABEL: Record<string, string> = {
  transfers: "Transfer logs",
  txs: "Transactions + Seaport orders",
  balances: "Balances",
  floor: "Floor price",
};

const selectCls =
  "rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent";
const inputCls =
  "rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent";
const runBtnCls =
  "flex items-center gap-1.5 rounded-lg bg-fg px-3 py-2 text-[13px] font-semibold text-bg hover:opacity-90 disabled:opacity-40";

function labelCls(on: boolean): string {
  return `cursor-pointer rounded-lg border px-2.5 py-1.5 text-[12px] ${
    on
      ? "border-accent/60 bg-accent/10 text-fg"
      : "border-line bg-bg text-muted hover:text-fg"
  }`;
}

// ───────────────────────── Collection PnL tab ─────────────────────────

interface CollectionProgress {
  phase: string;
  done: number;
  total: number;
}

function CollectionTab() {
  const { wallets, groups, load: loadWallets } = useWalletStore();
  const [chains, setChains] = useState<ChainRow[]>([]);
  const [chainId, setChainId] = useState("");
  const [contract, setContract] = useState(
    () => localStorage.getItem("pnl.collectionContract") ?? "",
  );
  const [groupFilter, setGroupFilter] = useState<GroupFilter>("all");
  // null = every wallet (auto-includes new ones)
  const [selected, setSelected] = useState<Set<number> | null>(null);
  const [extraText, setExtraText] = useState("");
  const [windowBlocks, setWindowBlocks] = useState(() => {
    const saved = localStorage.getItem("pnl.collectionWindowBlocks.v2");
    return saved !== null ? Number(saved) || 0 : 0;
  });
  const [feePct, setFeePct] = useState(() => {
    const saved = localStorage.getItem("pnl.collectionFeePct");
    return saved !== null ? Number(saved) || 0 : 2.5;
  });
  const [scanning, setScanning] = useState(false);
  const [progress, setProgress] = useState<CollectionProgress | null>(null);
  const [result, setResult] = useState<CollectionPnlResult | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const groupNames = useMemo(() => groupNameMap(groups), [groups]);
  const filtered = useMemo(
    () => filterWalletsByGroup(wallets, groupFilter),
    [wallets, groupFilter],
  );
  const enabledChains = useMemo(() => chains.filter((c) => c.enabled), [chains]);
  const isChecked = (id: number) => selected === null || selected.has(id);
  const selectedCount = filtered.filter((w) => isChecked(w.id)).length;

  useEffect(() => {
    const un = listen<CollectionProgress>("collection-pnl-progress", (e) => {
      setProgress(e.payload);
    });
    return () => {
      void un.then((f) => f());
    };
  }, []);

  useEffect(() => {
    void (async () => {
      try {
        const c = await ipc<ChainRow[]>("chain_list");
        setChains(c);
        await loadWallets();
        const enabled = c.filter((x) => x.enabled);
        setChainId((prev) => {
          if (prev && enabled.some((x) => String(x.chain_id) === prev))
            return prev;
          return enabled[0] ? String(enabled[0].chain_id) : "";
        });
      } catch (e) {
        setErr(String(e));
      }
    })();
  }, [loadWallets]);

  function toggleOne(id: number, on: boolean) {
    const base = selected ?? new Set(filtered.map((w) => w.id));
    if (on) base.add(id);
    else base.delete(id);
    setSelected(base);
  }

  function toggleAll(on: boolean) {
    setSelected(on ? null : new Set());
  }

  function setWindow(v: number) {
    const n = Math.max(0, Math.min(100_000_000, v || 0));
    setWindowBlocks(n);
    localStorage.setItem("pnl.collectionWindowBlocks.v2", String(n));
  }

  function setFee(v: number) {
    const n = Math.max(0, Math.min(10, Number.isFinite(v) ? v : 0));
    setFeePct(n);
    localStorage.setItem("pnl.collectionFeePct", String(n));
  }

  function saveContract(v: string) {
    setContract(v);
    localStorage.setItem("pnl.collectionContract", v.trim());
  }

  const contractOk = /^0x[0-9a-fA-F]{40}$/.test(contract.trim());
  const canRun = contractOk && !!chainId && selectedCount > 0 && !scanning;

  async function onScan() {
    if (!canRun) return;
    setScanning(true);
    setErr(null);
    setResult(null);
    setProgress({ phase: "transfers", done: 0, total: 0 });
    try {
      const walletIds = filtered
        .filter((w) => isChecked(w.id))
        .map((w) => w.id);
      const extraAddresses = extraText
        .split(/[\s,;]+/)
        .map((s) => s.trim())
        .filter(Boolean);
      const r = await ipc<CollectionPnlResult>("collection_pnl_scan", {
        contract: contract.trim(),
        chainId: Number(chainId),
        walletIds,
        extraAddresses,
        windowBlocks,
        // % → basis points (backend divides by 10_000): 2.5% = 250 bps.
        feeBps: Math.round(feePct * 100),
      });
      setResult(r);
    } catch (e) {
      setErr(String(e));
    } finally {
      setScanning(false);
      setProgress(null);
    }
  }

  const t = result?.totals;

  return (
    <div className="space-y-4">
      {/* controls */}
      <div className="flex flex-wrap items-end gap-2">
        <label className="flex flex-col gap-1 text-[11px] text-muted">
          Chain
          <select
            value={chainId}
            onChange={(e) => setChainId(e.target.value)}
            className={selectCls}
          >
            <option value="">Chain…</option>
            {enabledChains.map((c) => (
              <option key={c.id} value={c.chain_id}>
                {c.name}
              </option>
            ))}
          </select>
        </label>
        <label className="flex min-w-[320px] flex-1 flex-col gap-1 text-[11px] text-muted">
          Collection contract
          <input
            value={contract}
            onChange={(e) => saveContract(e.target.value)}
            placeholder="0x… collection (ERC-721)"
            spellCheck={false}
            className={`${inputCls} font-mono`}
          />
        </label>
        <label className="flex flex-col gap-1 text-[11px] text-muted">
          Window (blocks)
          <input
            type="number"
            min={0}
            max={100_000_000}
            step={10_000}
            value={windowBlocks}
            disabled={scanning}
            onChange={(e) => setWindow(Number(e.target.value))}
            className={`${inputCls} w-28 disabled:opacity-50`}
          />
        </label>
        <label className="flex flex-col gap-1 text-[11px] text-muted">
          Floor fee %
          <input
            type="number"
            min={0}
            max={10}
            step={0.1}
            value={feePct}
            disabled={scanning}
            onChange={(e) => setFee(Number(e.target.value))}
            className={`${inputCls} w-20 disabled:opacity-50`}
          />
        </label>
        <button onClick={onScan} disabled={!canRun} className={runBtnCls}>
          <ChartNoAxesCombined className="h-4 w-4" />
          {scanning ? "Scanning…" : "Run Collection PnL"}
        </button>
      </div>
      <div className="text-[11px] text-muted">
        Window 0 = full history from genesis (recommended). Fast chains matter:
        Robinhood Chain runs ~10 blocks/sec, so 50,000 blocks only covers the
        last ~1.4 hours. Fee % approximates the marketplace fee and is applied
        to floor-based unrealized value (OpenSea stats and in-window Seaport
        sales).
      </div>

      {/* wallet picker */}
      <div className="rounded-[14px] border border-line bg-card">
        <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-2.5">
          <select
            value={groupFilter}
            onChange={(e) => {
              const v = e.target.value;
              setGroupFilter(
                v === "all" || v === "ungrouped" ? v : Number(v),
              );
            }}
            className={`${selectCls} w-auto`}
          >
            <option value="all">All groups ({wallets.length})</option>
            <option value="ungrouped">
              Ungrouped ({wallets.filter((w) => w.group_id == null).length})
            </option>
            {groups.map((g) => (
              <option key={g.id} value={g.id}>
                {g.name} ({g.wallet_count})
              </option>
            ))}
          </select>
          <label className="flex cursor-pointer items-center gap-1.5 text-[12px] text-muted">
            <input
              type="checkbox"
              checked={selected === null && filtered.length > 0}
              onChange={(e) => toggleAll(e.target.checked)}
            />
            select all
          </label>
          <span className="text-[12px] text-muted">
            {selectedCount}/{filtered.length} selected
          </span>
        </div>
        <div className="flex max-h-44 flex-wrap gap-1.5 overflow-y-auto p-3">
          {filtered.map((w) => {
            const g =
              w.group_id != null ? groupNames.get(w.group_id) : null;
            return (
              <label key={w.id} className={labelCls(isChecked(w.id))}>
                <input
                  type="checkbox"
                  className="mr-1.5 align-middle"
                  checked={isChecked(w.id)}
                  disabled={scanning}
                  onChange={(e) => toggleOne(w.id, e.target.checked)}
                />
                {w.label}
                <span className="ml-1 font-mono text-[10px] text-muted">
                  {shortAddress(w.address)}
                </span>
                {g ? <span className="ml-1 text-accent">[{g}]</span> : null}
              </label>
            );
          })}
          {filtered.length === 0 ? (
            <div className="text-[13px] text-muted">No wallets.</div>
          ) : null}
        </div>
        <div className="border-t border-line px-4 py-2.5">
          <textarea
            value={extraText}
            onChange={(e) => setExtraText(e.target.value)}
            placeholder="Extra addresses (one per line) — not stored in the vault"
            spellCheck={false}
            rows={2}
            disabled={scanning}
            className={`${inputCls} w-full resize-y font-mono text-[12px] disabled:opacity-50`}
          />
        </div>
      </div>

      {/* progress */}
      {scanning ? (
        <div className="rounded-[14px] border border-line bg-card p-3 text-[13px] text-muted">
          <div className="mb-1.5 flex items-center justify-between">
            <span>
              {PHASE_LABEL[progress?.phase ?? ""] ?? progress?.phase ?? "…"}
            </span>
            <span className="font-mono text-[11px]">
              {progress && progress.total > 0
                ? `${progress.done}/${progress.total}`
                : ""}
            </span>
          </div>
          <div className="h-1.5 overflow-hidden rounded-full bg-line">
            <div
              className="h-full rounded-full bg-accent transition-all"
              style={{
                width:
                  progress && progress.total > 0
                    ? `${Math.min(100, (progress.done / progress.total) * 100)}%`
                    : "30%",
              }}
            />
          </div>
        </div>
      ) : null}

      {err ? <div className="text-[12px] text-danger">{err}</div> : null}

      {!result && !scanning ? (
        <div className="rounded-[14px] border border-line bg-card">
          <EmptyState
            icon={<ChartNoAxesCombined className="h-8 w-8" />}
            title="Collection PnL"
            description="Pick a collection contract + wallets, then run a scan — mint/buy/sell priced from Seaport orders and tx value."
          />
        </div>
      ) : null}

      {result ? (
        <>
          {/* header cards */}
          <div className="grid grid-cols-1 gap-4 md:grid-cols-4">
            <div className="rounded-[14px] border border-line bg-card p-4">
              <div className="text-[12px] text-muted">Floor price</div>
              <div className="mt-1 text-[20px] font-semibold">
                {result.floor_eth ? formatEth(result.floor_eth) : "n/a"}{" "}
                <span className="text-[13px] font-normal text-muted">
                  {result.floor_eth ? result.native_symbol : ""}
                </span>
              </div>
              <div className="mt-0.5 text-[11px] text-muted">
                {result.floor_source === "opensea"
                  ? `OpenSea · fee ${result.fee_bps / 100}% applied`
                  : result.floor_source === "window-sales"
                    ? "cheapest in-window sale"
                    : "unavailable"}
              </div>
            </div>
            <div className="rounded-[14px] border border-line bg-card p-4">
              <div className="text-[12px] text-muted">Net PnL</div>
              <div
                className={`mt-1 text-[20px] font-semibold ${
                  t?.net_eth ? flowClass(t.net_eth) : "text-muted"
                }`}
                title={t?.net_eth ?? undefined}
              >
                {t?.net_eth != null ? formatEth(t.net_eth) : "n/a"}{" "}
                <span className="text-[13px] font-normal text-muted">
                  {result.native_symbol}
                </span>
              </div>
              <div className="mt-0.5 text-[11px] text-muted">
                realized + unrealized − gas
              </div>
            </div>
            <div className="rounded-[14px] border border-line bg-card p-4">
              <div className="text-[12px] text-muted">ROI</div>
              <div
                className={`mt-1 text-[20px] font-semibold ${
                  t?.roi_pct == null
                    ? "text-muted"
                    : t.roi_pct >= 0
                      ? "text-ok"
                      : "text-danger"
                }`}
              >
                {t?.roi_pct != null
                  ? `${t.roi_pct >= 0 ? "+" : ""}${t.roi_pct.toFixed(1)}%`
                  : "n/a"}
              </div>
              <div className="mt-0.5 text-[11px] text-muted">
                net ÷ (spent + gas)
              </div>
            </div>
            <div className="rounded-[14px] border border-line bg-card p-4">
              <div className="text-[12px] text-muted">Holding</div>
              <div className="mt-1 text-[20px] font-semibold">
                {t?.balance ?? 0}{" "}
                <span className="text-[13px] font-normal text-muted">
                  tokens
                </span>
              </div>
              <div className="mt-0.5 text-[11px] text-muted">
                {t?.minted ?? 0} minted · {t?.bought ?? 0} bought ·{" "}
                {t?.sold ?? 0} sold
              </div>
            </div>
          </div>

          {/* money strip */}
          <div className="rounded-[14px] border border-line bg-card">
            <div className="grid grid-cols-2 gap-y-3 p-4 sm:grid-cols-3 lg:grid-cols-6">
              {[
                ["Spent", `${t ? formatEth(t.spent_eth) : "0"} ${result.native_symbol}`],
                ["Realized", t ? formatEth(t.realized_eth) : "0"],
                ["Unrealized", t?.unrealized_eth != null ? formatEth(t.unrealized_eth) : "n/a"],
                ["Gas", `${t ? formatEth(t.gas_eth) : "0"} ${result.native_symbol}`],
                ["Wallets", String(t?.wallets ?? 0)],
                [
                  "Unpriced sales",
                  t && t.unpriced_sales > 0
                    ? String(t.unpriced_sales)
                    : "0",
                ],
              ].map(([k, v]) => (
                <div key={k}>
                  <div className="text-[11px] text-muted">{k}</div>
                  <div
                    className={`text-[15px] font-semibold ${
                      k === "Realized" || k === "Unrealized"
                        ? flowClass(String(v))
                        : ""
                    }`}
                  >
                    {v}
                  </div>
                </div>
              ))}
            </div>
          </div>

          {/* warnings + model note */}
          {result.warnings.length > 0 ? (
            <div className="rounded-[14px] border border-warn/30 bg-warn/10 p-3 text-[12px] text-warn">
              <ul className="list-inside list-disc space-y-0.5">
                {result.warnings.map((w, i) => (
                  <li key={i}>{w}</li>
                ))}
              </ul>
            </div>
          ) : null}
          <div className="rounded-[14px] border border-line bg-card p-3 text-[12px] text-muted">
            Pricing from Seaport <code>OrderFulfilled</code> logs + tx
            value/gas (native ETH only, no explorer). Realized = proceeds −
            basis; unrealized = floor × in-window holdings − basis. Holdings
            that predate the window have no tracked basis and are excluded from
            unrealized (flagged per wallet). Blocks{" "}
            {result.from_block.toLocaleString()}–
            {result.to_block.toLocaleString()}.
          </div>

          {/* per-wallet table */}
          <div className="rounded-[14px] border border-line bg-card">
            <div className="border-b border-line px-4 py-2.5 text-[12px] font-semibold">
              Per-wallet ({result.rows.length})
            </div>
            <div className="overflow-x-auto">
              <table className="w-full text-[13px]">
                <thead>
                  <tr className="border-b border-line/60 text-left text-[11px] uppercase tracking-wide text-muted">
                    <th className="px-4 py-2">Wallet</th>
                    <th className="px-2 py-2 text-right">Mint</th>
                    <th className="px-2 py-2 text-right">Buy</th>
                    <th className="px-2 py-2 text-right">Sold</th>
                    <th className="px-2 py-2 text-right">Hold</th>
                    <th className="px-2 py-2 text-right">Spent</th>
                    <th className="px-2 py-2 text-right">Gas</th>
                    <th className="px-2 py-2 text-right">Realized</th>
                    <th className="px-2 py-2 text-right">Unrealized</th>
                    <th className="px-2 py-2 text-right">Net</th>
                    <th className="px-2 py-2 text-right">ROI %</th>
                    <th className="px-2 py-2 text-right">⚠</th>
                  </tr>
                </thead>
                <tbody>
                  {result.rows.map((r: CollectionPnlRow) => (
                    <tr
                      key={`${r.wallet_id ?? "x"}-${r.address}`}
                      className="border-b border-line/40 last:border-0"
                    >
                      <td className="px-4 py-2">
                        <div className="font-medium">{r.label}</div>
                        <div className="font-mono text-[10px] text-muted">
                          {shortAddress(r.address)}
                          {r.out_of_window > 0
                            ? ` · ${r.out_of_window} pre-window`
                            : ""}
                        </div>
                      </td>
                      <td className="px-2 py-2 text-right font-mono tabular-nums">{r.minted}</td>
                      <td className="px-2 py-2 text-right font-mono tabular-nums">{r.bought}</td>
                      <td className="px-2 py-2 text-right font-mono tabular-nums">{r.sold}</td>
                      <td className="px-2 py-2 text-right font-mono tabular-nums">
                        {r.balance_known
                          ? r.balance
                          : `~${r.holding}`}
                      </td>
                      <td className="px-2 py-2 text-right font-mono tabular-nums" title={r.spent_eth}>
                        {formatEth(r.spent_eth)}
                      </td>
                      <td className="px-2 py-2 text-right font-mono tabular-nums text-muted" title={r.gas_eth}>
                        {formatEth(r.gas_eth)}
                      </td>
                      <td
                        className={`px-2 py-2 text-right font-mono tabular-nums ${flowClass(r.realized_eth)}`}
                        title={r.realized_eth}
                      >
                        {formatEth(r.realized_eth)}
                      </td>
                      <td
                        className={`px-2 py-2 text-right font-mono tabular-nums ${
                          r.unrealized_eth == null
                            ? "text-muted"
                            : flowClass(r.unrealized_eth)
                        }`}
                        title={r.unrealized_eth ?? undefined}
                      >
                        {r.unrealized_eth != null ? formatEth(r.unrealized_eth) : "n/a"}
                      </td>
                      <td
                        className={`px-2 py-2 text-right font-mono font-semibold tabular-nums ${
                          r.net_eth == null ? "text-muted" : flowClass(r.net_eth)
                        }`}
                        title={r.net_eth ?? undefined}
                      >
                        {r.net_eth != null ? formatEth(r.net_eth) : "n/a"}
                      </td>
                      <td className="px-2 py-2 text-right">
                        {r.roi_pct != null
                          ? `${r.roi_pct >= 0 ? "+" : ""}${r.roi_pct.toFixed(1)}`
                          : "—"}
                      </td>
                      <td
                        className="px-2 py-2 text-right text-warn"
                        title={
                          r.basis_incomplete
                            ? "cost basis incomplete"
                            : r.unpriced_sales > 0
                              ? `${r.unpriced_sales} unpriced sale(s)`
                              : ""
                        }
                      >
                        {r.basis_incomplete || r.unpriced_sales > 0 ? "⚠" : ""}
                      </td>
                    </tr>
                  ))}
                  {/* totals */}
                  <tr className="border-t border-line bg-bg/50 font-semibold">
                    <td className="px-4 py-2 text-[12px]">Total</td>
                    <td className="px-2 py-2 text-right text-[12px]">
                      {t?.minted ?? 0}
                    </td>
                    <td className="px-2 py-2 text-right text-[12px]">
                      {t?.bought ?? 0}
                    </td>
                    <td className="px-2 py-2 text-right text-[12px]">
                      {t?.sold ?? 0}
                    </td>
                    <td className="px-2 py-2 text-right text-[12px]">
                      {t?.balance ?? 0}
                    </td>
                    <td className="px-2 py-2 text-right font-mono tabular-nums text-[12px]" title={t?.spent_eth}>
                      {t ? formatEth(t.spent_eth) : "0"}
                    </td>
                    <td className="px-2 py-2 text-right font-mono tabular-nums text-[12px] text-muted" title={t?.gas_eth}>
                      {t ? formatEth(t.gas_eth) : "0"}
                    </td>
                    <td
                      className={`px-2 py-2 text-right font-mono tabular-nums text-[12px] ${
                        t ? flowClass(t.realized_eth) : ""
                      }`}
                      title={t?.realized_eth}
                    >
                      {t ? formatEth(t.realized_eth) : "0"}
                    </td>
                    <td
                      className={`px-2 py-2 text-right font-mono tabular-nums text-[12px] ${
                        t?.unrealized_eth ? flowClass(t.unrealized_eth) : "text-muted"
                      }`}
                      title={t?.unrealized_eth ?? undefined}
                    >
                      {t?.unrealized_eth != null ? formatEth(t.unrealized_eth) : "n/a"}
                    </td>
                    <td
                      className={`px-2 py-2 text-right font-mono tabular-nums text-[12px] ${
                        t?.net_eth ? flowClass(t.net_eth) : "text-muted"
                      }`}
                      title={t?.net_eth ?? undefined}
                    >
                      {t?.net_eth != null ? formatEth(t.net_eth) : "n/a"}
                    </td>
                    <td className="px-2 py-2 text-right text-[12px]">
                      {t?.roi_pct != null
                        ? `${t.roi_pct >= 0 ? "+" : ""}${t.roi_pct.toFixed(1)}`
                        : "—"}
                    </td>
                    <td className="px-2 py-2 text-right text-[12px] text-warn">
                      {t?.basis_incomplete ? "⚠" : ""}
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </div>
        </>
      ) : null}
    </div>
  );
}

// ───────────────────────── Portfolio tab ─────────────────────────

function PortfolioTab() {
  const { wallets, groups, load: loadWallets } = useWalletStore();
  const [chains, setChains] = useState<ChainRow[]>([]);
  const [walletId, setWalletId] = useState("");
  const [chainId, setChainId] = useState("");
  const [result, setResult] = useState<PnlResult | null>(null);
  const [history, setHistory] = useState<PnlHistoryRow[]>([]);
  const [scanning, setScanning] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [walletGroupFilter, setWalletGroupFilter] = useState<GroupFilter>("all");
  const [windowBlocks, setWindowBlocks] = useState(() => {
    const saved = localStorage.getItem("pnl.windowBlocks");
    return saved ? Number(saved) || 50_000 : 50_000;
  });

  const groupNames = useMemo(() => groupNameMap(groups), [groups]);
  const formWallets = useMemo(
    () => filterWalletsByGroup(wallets, walletGroupFilter),
    [wallets, walletGroupFilter],
  );
  const enabledChains = useMemo(() => chains.filter((c) => c.enabled), [chains]);

  const loadHistory = useCallback(async (wid: string) => {
    if (!wid) {
      setHistory([]);
      return;
    }
    try {
      setHistory(
        await ipc<PnlHistoryRow[]>("pnl_history", {
          walletId: Number(wid),
          limit: 20,
        }),
      );
    } catch {
      setHistory([]);
    }
  }, []);

  const load = useCallback(async () => {
    try {
      const c = await ipc<ChainRow[]>("chain_list");
      setChains(c);
      await loadWallets();
      const enabled = c.filter((x) => x.enabled);
      setChainId((prev) => {
        if (prev && enabled.some((x) => String(x.chain_id) === prev)) return prev;
        return enabled[0] ? String(enabled[0].chain_id) : "";
      });
      setWalletId((prev) => {
        if (prev) return prev;
        const w = useWalletStore.getState().wallets;
        return w[0] ? String(w[0].id) : "";
      });
    } catch (e) {
      setErr(String(e));
    }
  }, [loadWallets]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    void loadHistory(walletId);
  }, [walletId, loadHistory]);

  function onWindowChange(v: number) {
    const n = Math.max(1_000, Math.min(5_000_000, v || 1_000));
    setWindowBlocks(n);
    localStorage.setItem("pnl.windowBlocks", String(n));
  }

  async function onScan() {
    if (!walletId || !chainId) return;
    setScanning(true);
    setErr(null);
    setResult(null);
    try {
      const r = await ipc<PnlResult>("pnl_scan", {
        walletId: Number(walletId),
        chainId: Number(chainId),
        windowBlocks,
      });
      setResult(r);
      await loadHistory(walletId);
    } catch (e) {
      setErr(String(e));
    } finally {
      setScanning(false);
    }
  }

  const sourceLabel =
    result?.source === "explorer"
      ? "Explorer API"
      : result?.source === "logs"
        ? "RPC log scan"
        : "Native only";

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap gap-2">
        <select
          value={walletGroupFilter}
          onChange={(e) => {
            const v = e.target.value;
            setWalletGroupFilter(v === "all" || v === "ungrouped" ? v : Number(v));
          }}
          className={selectCls}
        >
          <option value="all">All groups ({wallets.length})</option>
          <option value="ungrouped">
            Ungrouped ({wallets.filter((w) => w.group_id == null).length})
          </option>
          {groups.map((g) => (
            <option key={g.id} value={g.id}>
              {g.name} ({g.wallet_count})
            </option>
          ))}
        </select>
        <select
          value={walletId}
          onChange={(e) => setWalletId(e.target.value)}
          className={selectCls}
        >
          <option value="">Wallet…</option>
          {formWallets.map((w) => {
            const g = w.group_id != null ? groupNames.get(w.group_id) : null;
            return (
              <option key={w.id} value={w.id}>
                {g
                  ? `${w.label} [${g}] (${shortAddress(w.address)})`
                  : `${w.label} (${shortAddress(w.address)})`}
              </option>
            );
          })}
        </select>
        <select
          value={chainId}
          onChange={(e) => setChainId(e.target.value)}
          className={selectCls}
        >
          <option value="">Chain…</option>
          {enabledChains.map((c) => (
            <option key={c.id} value={c.chain_id}>
              {c.name}
            </option>
          ))}
        </select>
        <label className="flex items-center gap-1.5 text-[13px] text-muted">
          Window
          <input
            type="number"
            min={1_000}
            max={5_000_000}
            step={10_000}
            value={windowBlocks}
            disabled={scanning}
            onChange={(e) => onWindowChange(Number(e.target.value))}
            className="w-24 rounded-lg border border-line bg-bg px-2 py-2 text-[13px] outline-none focus:border-accent disabled:opacity-50"
          />
          <span className="text-[11px]">blocks</span>
        </label>
        <button
          onClick={onScan}
          disabled={scanning || !walletId || !chainId}
          className={runBtnCls}
        >
          <ChartNoAxesCombined className="h-4 w-4" />
          {scanning ? "Scanning…" : "Run PnL scan"}
        </button>
      </div>

      {scanning ? (
        <div className="rounded-[14px] border border-line bg-card p-3 text-[13px] text-muted">
          Scanning wallet… fetching balances and windowed flows.
        </div>
      ) : null}

      {err ? <div className="text-[12px] text-danger">{err}</div> : null}

      {!result && history.length === 0 ? (
        <div className="rounded-[14px] border border-line bg-card">
          <EmptyState
            icon={<ChartNoAxesCombined className="h-8 w-8" />}
            title="PnL data source not connected"
            description="Select a wallet and chain, then run a portfolio + net-flow scan."
          />
        </div>
      ) : (
        <div className="space-y-4">
          {result ? (
            <>
              <div className="grid grid-cols-1 gap-4 md:grid-cols-4">
                <div className="rounded-[14px] border border-line bg-card p-4">
                  <div className="text-[12px] text-muted">Source</div>
                  <div className="mt-1 text-[20px] font-semibold">{sourceLabel}</div>
                  <div className="mt-0.5 text-[11px] text-muted">
                    {result.status} · blocks {result.from_block}–{result.to_block}
                  </div>
                </div>
                <div className="rounded-[14px] border border-line bg-card p-4">
                  <div className="text-[12px] text-muted">Native balance</div>
                  <div className="mt-1 text-[20px] font-semibold">
                    {result.native_balance}{" "}
                    <span className="text-[13px] font-normal text-muted">
                      {result.native_symbol}
                    </span>
                  </div>
                </div>
                <div className="rounded-[14px] border border-line bg-card p-4">
                  <div className="text-[12px] text-muted">Net native flow (window)</div>
                  <div
                    className={`mt-1 text-[20px] font-semibold ${
                      result.net_native_flow
                        ? flowClass(result.net_native_flow)
                        : "text-muted"
                    }`}
                  >
                    {result.net_native_flow ?? "n/a"}
                  </div>
                  <div className="mt-0.5 text-[11px] text-muted">
                    {result.window_blocks.toLocaleString()} blocks
                  </div>
                </div>
                <div className="rounded-[14px] border border-line bg-card p-4">
                  <div className="text-[12px] text-muted">Realized PnL</div>
                  <div className="mt-1 text-[20px] font-semibold">
                    {result.realized_eth}
                  </div>
                  <div className="mt-0.5 text-[11px] text-muted">
                    cost basis not tracked
                  </div>
                </div>
              </div>

              <div className="rounded-[14px] border border-line bg-card">
                <div className="border-b border-line px-4 py-2.5 text-[12px] font-semibold">
                  Token positions ({result.tokens.length})
                </div>
                {result.tokens.length === 0 ? (
                  <div className="px-4 py-4 text-[13px] text-muted">
                    No ERC-20 tokens found in this window.
                  </div>
                ) : (
                  <>
                    <div className="grid grid-cols-12 border-b border-line/60 px-4 py-2 text-[11px] uppercase tracking-wide text-muted">
                      <div className="col-span-4">Token</div>
                      <div className="col-span-3">Balance</div>
                      <div className="col-span-3">Net flow (window)</div>
                      <div className="col-span-2 text-right">Contract</div>
                    </div>
                    {result.tokens.map((t) => (
                      <div
                        key={t.contract}
                        className="grid grid-cols-12 border-b border-line/60 px-4 py-2.5 text-[13px] last:border-0"
                      >
                        <div className="col-span-4 truncate font-medium">
                          {t.symbol}
                        </div>
                        <div className="col-span-3">{t.balance}</div>
                        <div className={`col-span-3 ${flowClass(t.net_flow)}`}>
                          {t.net_flow}
                        </div>
                        <div className="col-span-2 truncate text-right font-mono text-[11px] text-muted">
                          {shortAddress(t.contract, 4)}
                        </div>
                      </div>
                    ))}
                  </>
                )}
              </div>

              {result.note ? (
                <div className="rounded-[14px] border border-warn/30 bg-warn/10 p-3 text-[12px] text-warn">
                  {result.note}
                </div>
              ) : null}
            </>
          ) : null}

          <div className="rounded-[14px] border border-line bg-card">
            <div className="flex items-center gap-1.5 border-b border-line px-4 py-2.5 text-[12px] font-semibold">
              <History className="h-3.5 w-3.5" />
              Scan history ({history.length})
            </div>
            {history.length === 0 ? (
              <div className="px-4 py-4 text-[13px] text-muted">No scans yet.</div>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-[13px]">
                  <thead>
                    <tr className="border-b border-line/60 text-left text-[11px] uppercase tracking-wide text-muted">
                      <th className="px-4 py-2">When</th>
                      <th className="px-4 py-2">Status</th>
                      <th className="px-4 py-2">Source</th>
                      <th className="px-4 py-2">Native balance</th>
                      <th className="px-4 py-2">Net flow</th>
                      <th className="px-4 py-2">Tokens</th>
                      <th className="px-4 py-2">Window</th>
                    </tr>
                  </thead>
                  <tbody>
                    {history.map((h) => (
                      <tr key={h.id} className="border-b border-line/40 last:border-0">
                        <td className="whitespace-nowrap px-4 py-2 text-muted">
                          {fmtTime(h.scanned_at)}
                        </td>
                        <td className="px-4 py-2 capitalize">{h.status}</td>
                        <td className="px-4 py-2">
                          {h.summary?.source === "explorer"
                            ? "Explorer"
                            : h.summary?.source === "logs"
                              ? "Logs"
                              : (h.summary?.source ?? "—")}
                        </td>
                        <td className="px-4 py-2">
                          {h.summary?.native_balance ?? "—"}{" "}
                          <span className="text-muted">
                            {h.summary?.native_symbol}
                          </span>
                        </td>
                        <td
                          className={`px-4 py-2 ${
                            h.summary?.net_native_flow
                              ? flowClass(h.summary.net_native_flow)
                              : "text-muted"
                          }`}
                        >
                          {h.summary?.net_native_flow ?? "n/a"}
                        </td>
                        <td className="px-4 py-2">
                          {h.summary?.token_count ?? "—"}
                        </td>
                        <td className="px-4 py-2 text-muted">
                          {h.summary?.window_blocks
                            ? h.summary.window_blocks.toLocaleString()
                            : "—"}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

// ───────────────────────── page ─────────────────────────

export function PnlPage() {
  const [tab, setTab] = useState<PnlTab>(() => {
    const saved = localStorage.getItem("pnl.tab");
    return saved === "portfolio" ? "portfolio" : "collection";
  });

  function switchTab(t: PnlTab) {
    setTab(t);
    localStorage.setItem("pnl.tab", t);
  }

  const tabCls = (on: boolean) =>
    `rounded-lg px-3 py-1.5 text-[13px] font-semibold transition ${
      on ? "bg-fg text-bg" : "text-muted hover:text-fg"
    }`;

  return (
    <div className="p-6">
      <PageHeader
        suite="Portfolio"
        title="PnL"
        subtitle="Collection PnL (Tracecard-style, Seaport pricing) plus per-wallet portfolio net flow."
      />
      <div className="mb-4 flex w-fit gap-1 rounded-xl border border-line bg-card p-1">
        <button className={tabCls(tab === "collection")} onClick={() => switchTab("collection")}>
          Collection PnL
        </button>
        <button className={tabCls(tab === "portfolio")} onClick={() => switchTab("portfolio")}>
          Portfolio
        </button>
      </div>
      {tab === "collection" ? <CollectionTab /> : <PortfolioTab />}
    </div>
  );
}
