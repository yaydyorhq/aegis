import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { CheckCircle2, Clock, Copy, ExternalLink, Loader2, X, XCircle } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ipc } from "../../lib/ipc";
import type {
  ChainRow,
  FundJobRow,
  FundPreview,
  FundTxRow,
  FundsStartArgs,
} from "../../lib/types";
import { shortAddress } from "../../lib/utils";
import { pushToast, useDismissOnEscape } from "../../components/ui";
import { filterWalletsByGroup, groupNameMap, useWalletStore } from "../../store/app";

type Mode = "disperse" | "consolidate";
type AmountMode = "fixed" | "target";
type GroupFilter = number | "all" | "ungrouped";
type Step = "config" | "progress";

const STATUS_META: Record<
  string,
  { label: string; className: string; icon: typeof CheckCircle2 }
> = {
  pending: { label: "Pending", className: "text-muted", icon: Clock },
  signing: { label: "Signing", className: "text-warn", icon: Loader2 },
  broadcasting: { label: "Broadcasting", className: "text-warn", icon: Loader2 },
  confirming: { label: "Confirming", className: "text-warn", icon: Loader2 },
  confirmed: { label: "Success", className: "text-ok", icon: CheckCircle2 },
  failed: { label: "Failed", className: "text-danger", icon: XCircle },
  skipped: { label: "Skipped", className: "text-muted", icon: XCircle },
};

function formatUnits(wei: string, decimals: number): string {
  try {
    const neg = wei.startsWith("-");
    const digits = (neg ? wei.slice(1) : wei).replace(/^0x/, "");
    if (decimals === 0) return (neg ? "-" : "") + digits;
    const padded = digits.padStart(decimals + 1, "0");
    const whole = padded.slice(0, padded.length - decimals);
    let frac = padded.slice(padded.length - decimals).replace(/0+$/, "");
    if (frac.length > 8) frac = frac.slice(0, 8);
    const body = frac ? `${whole}.${frac}` : whole;
    return (neg ? "-" : "") + body;
  } catch {
    return wei;
  }
}

function parseUnitsToWei(input: string, decimals: number): string | null {
  const t = input.trim();
  if (!t || !/^\d+(\.\d+)?$/.test(t)) return null;
  const [whole, fracRaw = ""] = t.split(".");
  if (fracRaw.length > decimals) return null;
  const frac = fracRaw.padEnd(decimals, "0");
  const combined = `${whole}${frac}`.replace(/^0+(?=\d)/, "");
  return combined || "0";
}

export function ManageFundsModal({ onClose }: { onClose: () => void }) {
  const { wallets, groups, load: loadWallets } = useWalletStore();
  // Esc closes; a running job keeps going in the backend runner (the UI poll
  // just stops with the modal — reopening Wallets → Manage Funds reattaches).
  useDismissOnEscape(true, onClose);
  const [step, setStep] = useState<Step>("config");
  const [mode, setMode] = useState<Mode>("disperse");
  const [amountMode, setAmountMode] = useState<AmountMode>("fixed");
  const [chainId, setChainId] = useState<number | "">("");
  const [chains, setChains] = useState<ChainRow[]>([]);
  const [tokenAddress, setTokenAddress] = useState("");
  const [anchorWalletId, setAnchorWalletId] = useState<number | "">("");
  const [peerIds, setPeerIds] = useState<number[]>([]);
  const [amount, setAmount] = useState("");
  const [groupFilter, setGroupFilter] = useState<GroupFilter>("all");
  const [preview, setPreview] = useState<FundPreview | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [starting, setStarting] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const [job, setJob] = useState<FundJobRow | null>(null);
  const [txs, setTxs] = useState<FundTxRow[]>([]);
  const [jobDecimals, setJobDecimals] = useState<number>(18);
  const [copied, setCopied] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const groupNames = useMemo(() => groupNameMap(groups), [groups]);
  const enabledChains = useMemo(() => chains.filter((c) => c.enabled !== 0), [chains]);
  const peerWallets = useMemo(() => {
    const base = filterWalletsByGroup(wallets, groupFilter);
    return base.filter((w) => w.id !== anchorWalletId);
  }, [wallets, groupFilter, anchorWalletId]);
  const chain = useMemo(
    () => enabledChains.find((c) => c.chain_id === chainId) ?? null,
    [enabledChains, chainId],
  );
  const isNative = !tokenAddress.trim();

  const explorer = useMemo(() => {
    if (!chain?.explorer) return null;
    return chain.explorer.trim().replace(/\/+$/, "");
  }, [chain]);

  const counts = useMemo(() => {
    const c = { confirmed: 0, confirming: 0, pending: 0, failed: 0, other: 0 };
    for (const t of txs) {
      if (t.status === "confirmed") c.confirmed++;
      else if (t.status === "confirming" || t.status === "broadcasting" || t.status === "signing")
        c.confirming++;
      else if (t.status === "pending") c.pending++;
      else if (t.status === "failed") c.failed++;
      else c.other++;
    }
    return c;
  }, [txs]);

  const jobRunning = job?.status === "running";

  const resetPreview = useCallback(() => setPreview(null), []);

  // Load chains + wallets + resume active job
  useEffect(() => {
    void (async () => {
      try {
        const [c] = await Promise.all([
          ipc<ChainRow[]>("chain_list"),
          loadWallets(),
        ]);
        setChains(c);
        const first = c.find((x) => x.enabled !== 0);
        if (first) setChainId(first.chain_id);
      } catch (e) {
        setErr(String(e));
      }
      try {
        const active = await ipc<FundJobRow | null>("funds_active");
        if (active) {
          setJob(active);
          setStep("progress");
          const [rows, meta] = await Promise.all([
            ipc<FundTxRow[]>("funds_job_txs", { jobId: active.id }),
            active.asset === "native"
              ? Promise.resolve<[number, string]>([18, ""])
              : ipc<[number, string]>("funds_asset_meta", {
                  chainId: active.chain_id,
                  asset: active.asset,
                }),
          ]);
          setJobDecimals(meta[0]);
          setTxs(rows);
        }
      } catch {
        /* no active job */
      }
    })();
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [loadWallets]);

  // Poll while progress step + job running. `stopped` still settles in-flight
  // rows in the background, so keep watching; `done` is terminal.
  useEffect(() => {
    if (step !== "progress" || !job) return;
    if (job.status !== "running" && job.status !== "stopped") return;
    const tick = async () => {
      try {
        const [j, rows] = await Promise.all([
          ipc<FundJobRow>("funds_job", { id: job.id }),
          ipc<FundTxRow[]>("funds_job_txs", { jobId: job.id }),
        ]);
        setJob(j);
        setTxs(rows);
      } catch {
        /* transient */
      }
    };
    void tick();
    pollRef.current = setInterval(() => void tick(), 1500);
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [step, job?.id, job?.status]);

  // Debounced preview
  useEffect(() => {
    if (step !== "config") return;
    if (!chainId || anchorWalletId === "" || peerIds.length === 0 || !amount.trim()) {
      setPreview(null);
      return;
    }
    const t = setTimeout(() => void runPreview(true), 400);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [step, chainId, anchorWalletId, peerIds, amount, amountMode, mode, tokenAddress]);

  async function runPreview(silent = false) {
    if (chainId === "" || anchorWalletId === "") return null;
    // First pass for an ERC-20 has no cached decimals — resolve them BEFORE
    // parsing the amount, or a 6-decimal token gets read as 18 and the
    // preview shows the wrong scale (Start re-resolves; the display misled).
    let decimals = isNative ? 18 : (preview?.decimals ?? -1);
    if (!isNative && decimals < 0) {
      try {
        const meta = await ipc<[number, string]>("funds_asset_meta", {
          chainId,
          asset: tokenAddress.trim(),
        });
        decimals = meta[0];
      } catch {
        if (!silent) {
          setErr("Cannot read token decimals — check the contract address and chain");
        }
        return null;
      }
    }
    const wei = parseUnitsToWei(amount, decimals);
    if (!wei) {
      if (!silent) setErr("Invalid amount");
      return null;
    }
    if (!silent) {
      setPreviewing(true);
      setErr(null);
    }
    try {
      const args: FundsStartArgs = {
        mode,
        amount_mode: amountMode,
        asset: isNative ? "native" : tokenAddress.trim(),
        chain_id: chainId,
        amount_wei: wei,
        anchor_wallet_id: anchorWalletId,
        peer_wallet_ids: peerIds,
      };
      const p = await ipc<FundPreview>("funds_preview", { args });
      setPreview(p);
      if (!silent) setErr(null);
      return p;
    } catch (e) {
      if (!silent) setErr(String(e));
      else setPreview(null);
      return null;
    } finally {
      if (!silent) setPreviewing(false);
    }
  }

  async function onStart() {
    setStarting(true);
    setErr(null);
    try {
      const p = await runPreview(true);
      if (!p) {
        setErr("Preview failed — check amount, chain, and balances");
        return;
      }
      if (p.rows.every((r) => r.skip_reason)) {
        setErr("Every transfer would be skipped — adjust amount/target");
        return;
      }
      const wei = parseUnitsToWei(amount, p.decimals);
      if (!wei) {
        setErr("Invalid amount");
        return;
      }
      const args: FundsStartArgs = {
        mode,
        amount_mode: amountMode,
        asset: isNative ? "native" : tokenAddress.trim(),
        chain_id: chainId as number,
        amount_wei: wei,
        anchor_wallet_id: anchorWalletId as number,
        peer_wallet_ids: peerIds,
      };
      const j = await ipc<FundJobRow>("funds_start", { args });
      setJob(j);
      const rows = await ipc<FundTxRow[]>("funds_job_txs", { jobId: j.id });
      setTxs(rows);
      setStep("progress");
      pushToast("Fund job started", "ok", `${j.total_count} transfer(s) queued`);
    } catch (e) {
      setErr(String(e));
    } finally {
      setStarting(false);
    }
  }

  async function onStop() {
    if (!job) return;
    try {
      const j = await ipc<FundJobRow>("funds_stop", { jobId: job.id });
      setJob(j);
      const rows = await ipc<FundTxRow[]>("funds_job_txs", { jobId: job.id });
      setTxs(rows);
      pushToast("Fund job stopped", "warn");
    } catch (e) {
      setErr(String(e));
    }
  }

  async function copyTx(hash: string) {
    try {
      await navigator.clipboard.writeText(hash);
      setCopied(hash);
      setTimeout(() => setCopied(null), 1500);
    } catch {
      /* ignore */
    }
  }

  function togglePeer(id: number) {
    setPeerIds((ids) => (ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id]));
  }

  const selectVisible = () => setPeerIds(peerWallets.map((w) => w.id));
  const clearPeers = () => setPeerIds([]);

  const totalDisplay = preview
    ? `${formatUnits(preview.total_send_wei, preview.decimals)} ${preview.symbol}`
    : "—";
  const unitSuffix = preview?.symbol ?? (isNative ? (chain?.symbol ?? "ETH") : "TOKEN");

  const amountLabel =
    amountMode === "fixed" ? "Amount per wallet" : "Target balance per wallet";

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/60 p-4">
      <div className="flex max-h-[90vh] w-full max-w-3xl flex-col overflow-hidden rounded-[16px] border border-line bg-panel shadow-2xl">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-line px-5 py-4">
          <div className="flex items-center gap-3">
            <h2 className="text-[15px] font-semibold text-fg">Manage Funds</h2>
            {step === "progress" && job ? (
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="rounded-full border border-line bg-card px-2 py-0.5 text-[11px] text-muted">
                  {job.mode}
                </span>
                <span className="rounded-full border border-line bg-card px-2 py-0.5 text-[11px] text-muted">
                  {job.asset === "native" ? "native" : shortAddress(job.asset, 4)}
                </span>
                <span className="rounded-full border border-line bg-card px-2 py-0.5 text-[11px] text-muted">
                  {chains.find((c) => c.chain_id === job.chain_id)?.name ?? job.chain_id}
                </span>
                <span className="rounded-full border border-line bg-card px-2 py-0.5 text-[11px] text-muted">
                  direct
                </span>
              </div>
            ) : null}
          </div>
          <div className="flex items-center gap-3">
            {step === "progress" && job ? (
              <div className="flex items-center gap-2 text-[12px]">
                <span className="text-ok">{counts.confirmed} Success</span>
                <span className="text-warn">·</span>
                <span className="text-warn">{counts.confirming} Confirming</span>
                <span className="text-warn">·</span>
                <span className="text-muted">{counts.pending} Pending</span>
                {counts.failed > 0 ? (
                  <>
                    <span className="text-danger">·</span>
                    <span className="text-danger">{counts.failed} Failed</span>
                  </>
                ) : null}
              </div>
            ) : null}
            <button
              onClick={onClose}
              className="rounded-lg border border-line bg-card px-3 py-1.5 text-[12px] text-fg hover:border-muted/40"
            >
              {jobRunning ? "Close (Still Running)" : "Close"}
            </button>
          </div>
        </div>

        {err ? (
          <div className="border-b border-danger/30 bg-danger/10 px-5 py-2 text-[12px] text-danger">
            {err}
          </div>
        ) : null}

        {/* Body */}
        <div className="min-h-0 flex-1 overflow-y-auto p-5">
          {step === "config" ? (
            <div className="space-y-4">
              {/* ERC-20 + mode + chain */}
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <label className="block">
                  <span className="mb-1 block text-[12px] text-muted">
                    ERC-20 Contract Address
                  </span>
                  <input
                    value={tokenAddress}
                    onChange={(e) => {
                      setTokenAddress(e.target.value);
                      resetPreview();
                    }}
                    placeholder="Leave blank for native token"
                    spellCheck={false}
                    className="w-full rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[12px] outline-none focus:border-accent"
                  />
                </label>
                <label className="block">
                  <span className="mb-1 block text-[12px] text-muted">Chain</span>
                  <select
                    value={chainId}
                    onChange={(e) => {
                      setChainId(e.target.value === "" ? "" : Number(e.target.value));
                      // Token contracts are chain-specific — a pasted address
                      // from another chain only produces confusing previews.
                      setTokenAddress("");
                      resetPreview();
                    }}
                    className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
                  >
                    <option value="">Select chain</option>
                    {enabledChains.map((c) => (
                      <option key={c.id} value={c.chain_id}>
                        {c.name} ({c.chain_id})
                      </option>
                    ))}
                  </select>
                </label>
              </div>

              {/* Mode toggle */}
              <div className="grid grid-cols-2 gap-2">
                {(
                  [
                    ["disperse", "DISPERSE", "ONE TO MANY"],
                    ["consolidate", "CONSOLIDATE", "MANY TO ONE"],
                  ] as const
                ).map(([m, title, sub]) => (
                  <button
                    key={m}
                    type="button"
                    onClick={() => {
                      setMode(m);
                      // flip anchor/peers when switching mode if both sides set
                      if (anchorWalletId !== "" && peerIds.length === 1) {
                        setAnchorWalletId(peerIds[0]);
                        setPeerIds([anchorWalletId as number]);
                      }
                      resetPreview();
                    }}
                    className={`rounded-xl border px-4 py-3 text-left transition ${
                      mode === m
                        ? "border-accent bg-accent/10"
                        : "border-line bg-card hover:border-muted/40"
                    }`}
                  >
                    <div
                      className={`text-[13px] font-semibold ${
                        mode === m ? "text-accent" : "text-fg"
                      }`}
                    >
                      {title}
                    </div>
                    <div className="text-[11px] text-muted">{sub}</div>
                  </button>
                ))}
              </div>

              {/* Source / destinations */}
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <label className="block">
                  <span className="mb-1 block text-[12px] text-muted">
                    {mode === "disperse" ? "Source" : "Destination"}
                  </span>
                  <select
                    value={anchorWalletId}
                    onChange={(e) => {
                      setAnchorWalletId(e.target.value === "" ? "" : Number(e.target.value));
                      setPeerIds((ids) =>
                        ids.filter((id) => id !== Number(e.target.value)),
                      );
                      resetPreview();
                    }}
                    className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
                  >
                    <option value="">Select wallet</option>
                    {wallets.map((w) => (
                      <option key={w.id} value={w.id}>
                        {w.label} · {shortAddress(w.address, 4)}
                      </option>
                    ))}
                  </select>
                </label>
                <div>
                  <span className="mb-1 block text-[12px] text-muted">
                    {mode === "disperse" ? "Destinations" : "Sources"}{" "}
                    <span className="text-fg">({peerIds.length} Selected)</span>
                  </span>
                  <div className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] text-muted">
                    {mode === "disperse"
                      ? "Pick wallets below →"
                      : "Pick source wallets below →"}
                  </div>
                </div>
              </div>

              {/* Amount mode toggle */}
              <div className="grid grid-cols-2 gap-2">
                {(
                  [
                    ["fixed", "AMOUNT PER WALLET"],
                    ["target", "TARGET BALANCE PER WALLET"],
                  ] as const
                ).map(([m, label]) => (
                  <button
                    key={m}
                    type="button"
                    onClick={() => {
                      setAmountMode(m);
                      resetPreview();
                    }}
                    className={`rounded-lg border px-3 py-2 text-[12px] font-medium transition ${
                      amountMode === m
                        ? "border-accent bg-accent/10 text-accent"
                        : "border-line bg-card text-muted hover:text-fg"
                    }`}
                  >
                    {label}
                  </button>
                ))}
              </div>

              {/* Amount */}
              <label className="block">
                <span className="mb-1 block text-[12px] text-muted">{amountLabel}</span>
                <div className="flex items-center gap-2">
                  <input
                    value={amount}
                    onChange={(e) => {
                      setAmount(e.target.value);
                      resetPreview();
                    }}
                    placeholder="0.0"
                    inputMode="decimal"
                    className="flex-1 rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
                  />
                  <span className="rounded-lg border border-line bg-card px-3 py-2 text-[12px] text-muted">
                    {unitSuffix}
                  </span>
                </div>
              </label>

              <div className="flex items-center justify-between rounded-lg border border-line bg-bg px-3 py-2 text-[12px]">
                <span className="text-muted">total</span>
                <span className="font-medium text-fg">
                  {totalDisplay} across {peerIds.length} wallet
                  {peerIds.length === 1 ? "" : "s"}
                </span>
              </div>

              {preview?.warnings?.length ? (
                <div className="rounded-lg border border-warn/40 bg-warn/10 px-3 py-2 text-[12px] text-warn">
                  {preview.warnings.join(" · ")}
                </div>
              ) : null}

              {preview?.rows?.length ? (
                <div className="rounded-lg border border-line bg-bg">
                  <div className="grid grid-cols-[1fr_1fr_110px] border-b border-line px-3 py-2 text-[11px] uppercase tracking-wide text-muted">
                    <div>From</div>
                    <div>To</div>
                    <div className="text-right">Send</div>
                  </div>
                  <div className="max-h-40 overflow-y-auto">
                    {preview.rows.map((r, i) => (
                      <div
                        key={`${r.from_address}-${r.to_address}-${i}`}
                        className="grid grid-cols-[1fr_1fr_110px] items-center border-b border-line/50 px-3 py-1.5 text-[12px] last:border-0"
                      >
                        <div className="truncate font-mono text-muted">
                          {shortAddress(r.from_address, 4)}
                        </div>
                        <div className="truncate font-mono text-muted">
                          {shortAddress(r.to_address, 4)}
                        </div>
                        <div className="text-right">
                          {r.skip_reason ? (
                            <span className="text-warn">{r.skip_reason}</span>
                          ) : (
                            <span className="text-fg">
                              {formatUnits(r.send_wei, preview.decimals)}
                            </span>
                          )}
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              ) : null}

              {/* Wallet multi-select */}
              <div className="rounded-lg border border-line bg-bg p-3">
                <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
                  <div className="text-[11px] text-muted">
                    {mode === "disperse" ? "Destination" : "Source"} wallets{" "}
                    <span className="text-fg">({peerIds.length} selected)</span>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    <select
                      value={groupFilter}
                      onChange={(e) => {
                        const v = e.target.value;
                        setGroupFilter(
                          v === "all" || v === "ungrouped" ? v : Number(v),
                        );
                      }}
                      className="rounded-lg border border-line bg-card px-2 py-1 text-[12px] outline-none focus:border-accent"
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
                    <button
                      type="button"
                      onClick={selectVisible}
                      className="rounded border border-line px-2 py-1 text-[12px] text-fg hover:bg-line"
                    >
                      Select visible
                    </button>
                    <button
                      type="button"
                      onClick={clearPeers}
                      className="rounded border border-line px-2 py-1 text-[12px] text-muted hover:bg-line"
                    >
                      Clear
                    </button>
                  </div>
                </div>
                <div className="flex max-h-40 flex-wrap gap-1.5 overflow-y-auto">
                  {peerWallets.length === 0 ? (
                    <div className="text-[12px] text-muted">No wallets in this filter.</div>
                  ) : (
                    peerWallets.map((w) => {
                      const selected = peerIds.includes(w.id);
                      const g = w.group_id != null ? groupNames.get(w.group_id) : null;
                      return (
                        <button
                          key={w.id}
                          type="button"
                          onClick={() => {
                            togglePeer(w.id);
                            resetPreview();
                          }}
                          title={w.address}
                          className={`flex items-center gap-1.5 rounded-lg border px-2.5 py-1.5 text-[12px] transition ${
                            selected
                              ? "border-accent bg-accent/15 text-fg"
                              : "border-line bg-card text-muted hover:border-muted/40"
                          }`}
                        >
                          <span className="font-medium">{w.label}</span>
                          {g ? (
                            <span className="rounded bg-line px-1 text-[10px] text-muted">
                              {g}
                            </span>
                          ) : null}
                          <span className="font-mono text-[10px] opacity-70">
                            {shortAddress(w.address, 4)}
                          </span>
                        </button>
                      );
                    })
                  )}
                </div>
              </div>
            </div>
          ) : (
            /* ── Progress ── */
            <div className="space-y-3">
              {job ? (
                <div className="flex items-center justify-between text-[12px]">
                  <span className="text-muted">
                    Job #{job.id} · {job.status} · {job.total_count} tx
                  </span>
                  {jobRunning ? (
                    <button
                      onClick={onStop}
                      className="rounded-lg border border-danger/50 bg-danger/10 px-3 py-1.5 text-[12px] text-danger hover:bg-danger/20"
                    >
                      Stop remaining
                    </button>
                  ) : null}
                </div>
              ) : null}

              <div className="overflow-hidden rounded-[14px] border border-line bg-card">
                <div className="grid grid-cols-[36px_80px_1fr_1fr_110px_150px] border-b border-line px-3 py-2.5 text-[11px] uppercase tracking-wide text-muted">
                  <div>#</div>
                  <div>Type</div>
                  <div>From</div>
                  <div>To</div>
                  <div className="text-right">Amount</div>
                  <div>Tx / Error</div>
                </div>
                {txs.length === 0 ? (
                  <div className="px-4 py-8 text-center text-[13px] text-muted">
                    No transfers yet.
                  </div>
                ) : (
                  txs.map((t) => {
                    const meta = STATUS_META[t.status] ?? STATUS_META.pending;
                    const Icon = meta.icon;
                    return (
                      <div
                        key={t.id}
                        className="grid grid-cols-[36px_80px_1fr_1fr_110px_150px] items-center border-b border-line/60 px-3 py-2 text-[12px] last:border-0 hover:bg-line/30"
                      >
                        <div className="text-muted">{t.seq}</div>
                        <div className="text-muted">{t.kind}</div>
                        <div className="truncate font-mono text-muted" title={t.from_address}>
                          {shortAddress(t.from_address, 4)}
                        </div>
                        <div className="truncate font-mono text-muted" title={t.to_address}>
                          {shortAddress(t.to_address, 4)}
                        </div>
                        <div className="text-right font-medium text-fg">
                          {t.amount_wei && t.amount_wei !== "0"
                            ? formatUnits(t.amount_wei, jobDecimals)
                            : "—"}
                        </div>
                        <div className="flex items-center gap-1.5">
                          <span className={`flex items-center gap-1 ${meta.className}`}>
                            <Icon
                              className={`h-3.5 w-3.5 ${
                                t.status === "signing" ||
                                t.status === "broadcasting" ||
                                t.status === "confirming"
                                  ? "animate-spin"
                                  : ""
                              }`}
                            />
                            {meta.label}
                          </span>
                          {t.tx_hash ? (
                            <>
                              {explorer ? (
                                <button
                                  onClick={() =>
                                    void openUrl(`${explorer}/tx/${t.tx_hash}`).catch(
                                      () => {},
                                    )
                                  }
                                  title="Open in explorer"
                                  className="rounded p-1 text-muted hover:text-fg"
                                >
                                  <ExternalLink className="h-3.5 w-3.5" />
                                </button>
                              ) : null}
                              <button
                                onClick={() => void copyTx(t.tx_hash!)}
                                title="Copy tx hash"
                                className="rounded p-1 text-muted hover:text-fg"
                              >
                                {copied === t.tx_hash ? (
                                  <CheckCircle2 className="h-3.5 w-3.5 text-ok" />
                                ) : (
                                  <Copy className="h-3.5 w-3.5" />
                                )}
                              </button>
                            </>
                          ) : null}
                          {t.error ? (
                            <span
                              className="truncate text-[11px] text-danger"
                              title={t.error}
                            >
                              {t.error}
                            </span>
                          ) : null}
                        </div>
                      </div>
                    );
                  })
                )}
              </div>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between border-t border-line px-5 py-3">
          <span className="text-[12px] text-muted">
            {step === "config"
              ? "Direct EOA transfers · sequential · receipt-wait per tx"
              : jobRunning
                ? "Job continues in background if you close"
                : job?.status === "done"
                  ? "All transfers finished"
                  : `Job ${job?.status ?? ""}`}
          </span>
          <div className="flex gap-2">
            {step === "config" ? (
              <>
                <button
                  onClick={onClose}
                  className="rounded-lg border border-line bg-card px-4 py-2 text-[13px] text-fg hover:border-muted/40"
                >
                  <X className="mr-1 inline h-3.5 w-3.5" />
                  Cancel
                </button>
                <button
                  onClick={() => void onStart()}
                  disabled={
                    starting ||
                    previewing ||
                    chainId === "" ||
                    anchorWalletId === "" ||
                    peerIds.length === 0 ||
                    !amount.trim()
                  }
                  className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
                >
                  {starting ? "Starting…" : mode === "disperse" ? "Disperse" : "Consolidate"}
                </button>
              </>
            ) : (
              <button
                onClick={onClose}
                className="rounded-lg border border-line bg-card px-4 py-2 text-[13px] text-fg hover:border-muted/40"
              >
                {jobRunning ? "Close (Still Running)" : "Close"}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
