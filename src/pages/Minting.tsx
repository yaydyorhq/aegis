import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { ExternalLink, Link2, Loader2, Play, Plus, X, Zap } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ipc } from "../lib/ipc";
import type {
  AllowlistMatchRow,
  ChainRow,
  EnqueueBatchResult,
  MintTaskRow,
  OpenSeaMintPlan,
  SeaDropPlan,
} from "../lib/types";
import { EmptyState, PageHeader, StatusDot, pushToast } from "../components/ui";
import { shortAddress } from "../lib/utils";
import {
  filterWalletsByGroup,
  groupNameMap,
  useWalletStore,
} from "../store/app";

type GroupFilter = number | "all" | "ungrouped";
type Mode = "execute" | "simulate" | "spam" | "sweep";

const FUNCTION_PRESETS = [
  "mint()",
  "mint(uint256)",
  "mint(address,uint256)",
  "safeMint(address,uint256)",
  "publicMint(uint256)",
  "custom",
] as const;

const emptyForm = {
  chainId: "",
  contract: "",
  quantity: "1",
  valueEth: "0",
  functionPreset: "mint()",
  customFn: "",
  isHex: false,
  hexCalldata: "",
  parameters: "",
  flashbots: false,
  gasLimit: "auto",
  maxFee: "auto",
  priorityFee: "auto",
  nonce: "auto",
  timestamp: "",
  delayMs: "0",
  mode: "execute" as Mode,
  /** NFT collection address when using SeaDrop public-mint plan. */
  nftContract: "",
  /** GTD / allowlist JSON or plain addresses (optional). */
  allowlist: "",
};

export function MintingPage() {
  const { wallets, groups, load: loadWallets } = useWalletStore();
  const [tasks, setTasks] = useState<MintTaskRow[]>([]);
  const [chains, setChains] = useState<ChainRow[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [running, setRunning] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [walletGroupFilter, setWalletGroupFilter] = useState<GroupFilter>("all");
  const [selectedWalletIds, setSelectedWalletIds] = useState<number[]>([]);
  const [selectedRpcUrls, setSelectedRpcUrls] = useState<string[]>([]);
  const [form, setForm] = useState(emptyForm);
  const [seaDrop, setSeaDrop] = useState<SeaDropPlan | null>(null);
  const [fetchingDrop, setFetchingDrop] = useState(false);
  const [dropErr, setDropErr] = useState<string | null>(null);
  const [allowlistMatch, setAllowlistMatch] = useState<AllowlistMatchRow[] | null>(null);
  const [allowlistErr, setAllowlistErr] = useState<string | null>(null);
  const [openSeaPlan, setOpenSeaPlan] = useState<OpenSeaMintPlan | null>(null);
  const [fetchingOpenSea, setFetchingOpenSea] = useState(false);
  const [openSeaErr, setOpenSeaErr] = useState<string | null>(null);
  const [enqueueingOpenSea, setEnqueueingOpenSea] = useState(false);
  const seenTaskIds = useRef<Set<number>>(new Set());
  const seenStatuses = useRef<Map<number, string>>(new Map());
  const primedTasks = useRef(false);
  /** After local enqueue we already toast — suppress the "new task" duplicate. */
  const suppressNewTaskToast = useRef(false);

  const groupNames = useMemo(() => groupNameMap(groups), [groups]);
  const formWallets = useMemo(
    () => filterWalletsByGroup(wallets, walletGroupFilter),
    [wallets, walletGroupFilter],
  );

  const enabledChains = useMemo(
    () => chains.filter((c) => c.enabled !== 0),
    [chains],
  );

  const explorerByChain = useMemo(() => {
    const m = new Map<number, string>();
    for (const c of chains) {
      const ex = (c.explorer ?? "").trim().replace(/\/+$/, "");
      if (ex) m.set(c.chain_id, ex);
    }
    return m;
  }, [chains]);

  // RPC list is restricted to the SELECTED chain — mixing endpoints from
  // other chains would sign/estimate/nonce against the wrong network.
  const rpcOptions = useMemo(() => {
    const list: { url: string; label: string }[] = [];
    const seen = new Set<string>();
    for (const c of enabledChains) {
      if (!form.chainId || String(c.chain_id) !== form.chainId) continue;
      const url = c.rpc_url.trim();
      if (!url || seen.has(url)) continue;
      seen.add(url);
      list.push({ url, label: `${c.name} · ${shortAddress(url.replace(/^https?:\/\//, ""), 14)}` });
    }
    return list;
  }, [enabledChains, form.chainId]);

  // Drop stale RPC selections when they no longer belong to the chosen chain.
  useEffect(() => {
    setSelectedRpcUrls((prev) => {
      const kept = prev.filter((u) => rpcOptions.some((r) => r.url === u));
      if (kept.length > 0) return kept;
      const c = enabledChains.find((x) => String(x.chain_id) === form.chainId);
      return c?.rpc_url ? [c.rpc_url] : [];
    });
  }, [rpcOptions, form.chainId, enabledChains]);

  function applyTaskNotifications(next: MintTaskRow[]) {
    const newIds: number[] = [];
    const statusHits: {
      title: string;
      detail: string;
      tone: "ok" | "warn" | "error";
    }[] = [];
    for (const t of next) {
      if (!primedTasks.current) {
        seenTaskIds.current.add(t.id);
        seenStatuses.current.set(t.id, t.status);
        continue;
      }
      if (!seenTaskIds.current.has(t.id)) {
        seenTaskIds.current.add(t.id);
        seenStatuses.current.set(t.id, t.status);
        if (!suppressNewTaskToast.current) newIds.push(t.id);
      } else {
        const prev = seenStatuses.current.get(t.id);
        if (prev && prev !== t.status) {
          seenStatuses.current.set(t.id, t.status);
          if (t.status === "confirmed" || t.status === "simulated") {
            statusHits.push({
              title: `Mint #${t.id} ${t.status}`,
              detail: t.tx_hash ? shortAddress(t.tx_hash, 8) : shortAddress(t.contract, 6),
              tone: "ok",
            });
          } else if (
            t.status === "failed" ||
            t.status === "canceled" ||
            t.status === "cancelled"
          ) {
            statusHits.push({
              title: `Mint #${t.id} ${t.status}`,
              detail: (t.error || t.status).slice(0, 140),
              tone: "error",
            });
          }
        }
      }
    }
    primedTasks.current = true;
    if (newIds.length > 0) {
      const list = newIds.map((id) => `#${id}`).join(", ");
      pushToast(
        newIds.length === 1 ? `New mint task ${list}` : `New mint tasks ${list}`,
        "info",
        next
          .filter((t) => newIds.includes(t.id))
          .map((t) => shortAddress(t.contract, 6))
          .join(", "),
      );
    }
    for (const hit of statusHits) {
      pushToast(hit.title, hit.tone, hit.detail);
    }
  }

  const load = useCallback(async () => {
    try {
      const [t, c] = await Promise.all([
        ipc<MintTaskRow[]>("mint_list"),
        ipc<ChainRow[]>("chain_list"),
      ]);
      applyTaskNotifications(t);
      setTasks(t);
      setChains(c);
      await loadWallets();
    } catch (e) {
      setErr(String(e));
    }
    // applyTaskNotifications uses only refs + setState
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadWallets]);

  useEffect(() => {
    void load();
  }, [load]);

  function toggleWallet(id: number) {
    setSelectedWalletIds((ids) =>
      ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id],
    );
  }

  function selectVisibleWallets() {
    setSelectedWalletIds(formWallets.map((w) => w.id));
  }

  function deselectAllWallets() {
    setSelectedWalletIds([]);
  }

  function toggleRpc(url: string) {
    setSelectedRpcUrls((urls) =>
      urls.includes(url) ? urls.filter((x) => x !== url) : [...urls, url],
    );
  }

  function selectAllRpcs() {
    setSelectedRpcUrls(rpcOptions.map((r) => r.url));
  }

  // Preview allowlist × selected wallets (debounced).
  useEffect(() => {
    const text = form.allowlist.trim();
    if (!text || selectedWalletIds.length === 0) {
      setAllowlistMatch(null);
      setAllowlistErr(null);
      return;
    }
    setAllowlistErr(null);
    const handle = window.setTimeout(() => {
      void ipc<AllowlistMatchRow[]>("mint_allowlist_match", {
        walletIds: selectedWalletIds,
        allowlist: text,
      })
        .then((rows) => setAllowlistMatch(rows))
        .catch((e) => {
          setAllowlistMatch(null);
          setAllowlistErr(String(e));
        });
    }, 350);
    return () => window.clearTimeout(handle);
  }, [form.allowlist, selectedWalletIds]);

  async function onEnqueue(e: FormEvent) {
    e.preventDefault();
    setErr(null);
    try {
      const contract = form.contract.trim();
      if (!/^0x[0-9a-fA-F]{40}$/.test(contract)) {
        setErr("Contract must be a 0x + 40 hex address");
        return;
      }
      if (!form.chainId) {
        setErr("Select a chain");
        return;
      }
      if (selectedWalletIds.length === 0) {
        setErr("Select at least one wallet");
        return;
      }
      const allowlistText = form.allowlist.trim() || null;
      if (allowlistText && allowlistErr) {
        setErr(allowlistErr);
        return;
      }
      if (
        allowlistMatch &&
        allowlistMatch.length > 0 &&
        !allowlistMatch.some((m) => m.matched)
      ) {
        setErr("No selected wallets are on this allowlist");
        return;
      }
      const qty = Math.max(1, Math.floor(Number(form.quantity) || 1));
      // Plans bake quantity into calldata + value — editing qty afterwards
      // would silently mint the old amount with the new UI number.
      if (seaDrop && qty !== seaDrop.quantity) {
        setErr(
          `Quantity changed since SeaDrop fetch (built for ${seaDrop.quantity}) — re-fetch the public drop`,
        );
        return;
      }
      if (openSeaPlan && qty !== openSeaPlan.quantity) {
        setErr(
          `Quantity changed since OpenSea plan (built for ${openSeaPlan.quantity}) — re-fetch the stage or use "Enqueue stage"`,
        );
        return;
      }
      const valueWei = ethToWei(form.valueEth);
      if (valueWei === null) {
        setErr("Value must be a plain ETH number (e.g. 0.01)");
        return;
      }

      let functionName: string | null = null;
      let calldata: string | null = null;
      if (form.isHex) {
        const cd = form.hexCalldata.trim();
        const probe = cd
          .replace(/\{address\}/g, "aa")
          .replace(/\{signature\}/g, "aa")
          .replace(/\{proof\}/g, "aa");
        if (!/^0x[0-9a-fA-F]*$/.test(probe) || cd.length < 2) {
          setErr("Hex calldata must start with 0x (may include {address}/{signature})");
          return;
        }
        calldata = cd;
      } else if (form.functionPreset === "custom") {
        functionName = form.customFn.trim();
        if (!functionName || !functionName.includes("(")) {
          setErr("Custom function must look like name(types)");
          return;
        }
      } else {
        functionName = form.functionPreset;
      }

      let scheduledAt: number | null = null;
      if (form.timestamp.trim()) {
        const parsed = Date.parse(form.timestamp);
        if (Number.isNaN(parsed)) {
          setErr("Invalid timestamp");
          return;
        }
        scheduledAt = parsed;
      }

      const delayMs = Math.max(0, Math.floor(Number(form.delayMs) || 0));

      const result = await ipc<EnqueueBatchResult>("mint_enqueue", {
        args: {
          wallet_ids: selectedWalletIds,
          chain_id: Number(form.chainId),
          contract,
          quantity: qty,
          value_wei: valueWei,
          function_name: functionName,
          is_hex: form.isHex,
          parameters: form.parameters.trim() || null,
          calldata,
          rpc_endpoints: selectedRpcUrls.length ? selectedRpcUrls : null,
          flashbots: form.flashbots,
          gas_limit: form.gasLimit.trim() || null,
          max_fee_gwei: form.maxFee.trim() || null,
          priority_fee_gwei: form.priorityFee.trim() || null,
          nonce_override: form.nonce.trim() || null,
          scheduled_at: scheduledAt,
          delay_ms: delayMs,
          mode: form.mode,
          allowlist: allowlistText,
        },
      });
      const queued = result.tasks.length;
      const skipped = result.skipped.length;
      if (queued === 0) {
        setErr(
          skipped > 0
            ? `All ${skipped} selected wallets skipped — none on allowlist`
            : "Nothing enqueued",
        );
        return;
      }
      pushToast(
        "Mint task queued",
        "ok",
        skipped > 0
          ? `${queued} queued · ${skipped} skipped · ${form.mode}`
          : `${queued} wallet(s) · ${form.mode} · qty ${qty}`,
      );
      suppressNewTaskToast.current = true;
      setShowForm(false);
      setForm(emptyForm);
      setSeaDrop(null);
      setDropErr(null);
      setOpenSeaPlan(null);
      setOpenSeaErr(null);
      setAllowlistMatch(null);
      setAllowlistErr(null);
      setSelectedWalletIds([]);
      setSelectedRpcUrls([]);
      await load();
      suppressNewTaskToast.current = false;
    } catch (e2) {
      setErr(String(e2));
    }
  }

  async function onFetchDrop() {
    setDropErr(null);
    if (!form.chainId) {
      setDropErr("Select a chain first");
      return;
    }
    const nft = form.nftContract.trim();
    if (!/^0x[0-9a-fA-F]{40}$/.test(nft)) {
      setDropErr("NFT contract must be 0x + 40 hex");
      return;
    }
    const qty = Math.max(1, Math.floor(Number(form.quantity) || 1));
    setFetchingDrop(true);
    try {
      const plan = await ipc<SeaDropPlan>("mint_seadrop_plan", {
        chainId: Number(form.chainId),
        nftContract: nft,
        quantity: qty,
      });
      setSeaDrop(plan);
      setForm((f) => ({
        ...f,
        contract: plan.to,
        isHex: true,
        hexCalldata: plan.calldata,
        valueEth: plan.value_eth,
        parameters: "",
        functionPreset: "custom",
        customFn: "",
        quantity: String(qty),
      }));
      pushToast(
        plan.live ? "SeaDrop plan ready" : "SeaDrop drop not live",
        plan.live ? "ok" : "warn",
        `price ${plan.value_eth} ETH × ${plan.quantity} · fee ${shortAddress(plan.fee_recipient, 4)}`,
      );
    } catch (e) {
      const msg = String(e);
      setDropErr(msg);
      pushToast("SeaDrop fetch failed", "error", msg.slice(0, 160));
    } finally {
      setFetchingDrop(false);
    }
  }

  async function onFetchOpenSeaStage() {
    setErr(null);
    setOpenSeaErr(null);
    setOpenSeaPlan(null);
    if (!form.chainId) {
      setOpenSeaErr("Select a chain first");
      return;
    }
    const nft = form.nftContract.trim() || form.contract.trim();
    if (!/^0x[0-9a-fA-F]{40}$/.test(nft)) {
      setOpenSeaErr("NFT contract must be 0x + 40 hex");
      return;
    }
    const walletId = selectedWalletIds[0];
    if (walletId == null) {
      setOpenSeaErr("Select at least one wallet (for SIWE session)");
      return;
    }
    const qty = Math.max(1, Math.floor(Number(form.quantity) || 1));
    setFetchingOpenSea(true);
    try {
      const plan = await ipc<OpenSeaMintPlan>("mint_opensea_plan", {
        chainId: Number(form.chainId),
        walletId,
        collection: nft,
        quantity: qty,
        tokenId: "0",
      });
      setOpenSeaPlan(plan);
      setForm((f) => ({
        ...f,
        contract: plan.to,
        isHex: true,
        hexCalldata: plan.calldata,
        valueEth: plan.value_eth,
        parameters: "",
        functionPreset: "custom",
        customFn: "",
        quantity: String(plan.quantity),
        nftContract: plan.nft_contract,
      }));
      pushToast(
        `OpenSea ${plan.stage_type} plan ready`,
        "ok",
        `${plan.slug} · value ${plan.value_eth} ETH · qty ${plan.quantity}`,
      );
    } catch (e) {
      const msg = String(e);
      setOpenSeaErr(msg);
      pushToast("OpenSea stage fetch failed", "error", msg.slice(0, 160));
    } finally {
      setFetchingOpenSea(false);
    }
  }

  async function onEnqueueOpenSeaStage() {
    setErr(null);
    setOpenSeaErr(null);
    if (!form.chainId) {
      setOpenSeaErr("Select a chain first");
      return;
    }
    const nft = form.nftContract.trim() || form.contract.trim();
    if (!/^0x[0-9a-fA-F]{40}$/.test(nft)) {
      setOpenSeaErr("NFT contract must be 0x + 40 hex");
      return;
    }
    if (selectedWalletIds.length === 0) {
      setOpenSeaErr("Select at least one wallet");
      return;
    }
    const qty = Math.max(1, Math.floor(Number(form.quantity) || 1));
    let scheduledAt: number | null = null;
    if (form.timestamp.trim()) {
      const parsed = Date.parse(form.timestamp);
      if (Number.isNaN(parsed)) {
        setOpenSeaErr("Invalid timestamp");
        return;
      }
      scheduledAt = parsed;
    }
    const delayMs = Math.max(0, Math.floor(Number(form.delayMs) || 0));
    setEnqueueingOpenSea(true);
    try {
      const result = await ipc<EnqueueBatchResult>("mint_opensea_enqueue", {
        args: {
          wallet_ids: selectedWalletIds,
          chain_id: Number(form.chainId),
          collection: nft,
          quantity: qty,
          token_id: "0",
          rpc_endpoints: selectedRpcUrls.length ? selectedRpcUrls : null,
          flashbots: form.flashbots,
          gas_limit: form.gasLimit.trim() || null,
          max_fee_gwei: form.maxFee.trim() || null,
          priority_fee_gwei: form.priorityFee.trim() || null,
          scheduled_at: scheduledAt,
          delay_ms: delayMs,
          mode: form.mode,
        },
      });
      const queued = result.tasks.length;
      const skipped = result.skipped.length;
      if (queued === 0) {
        setOpenSeaErr(
          result.skipped
            .map((s) => `#${s.wallet_id}: ${s.reason}`)
            .join("; ")
            .slice(0, 400) || "Nothing enqueued",
        );
        pushToast("OpenSea mint failed", "error", `${skipped} skipped`);
        return;
      }
      pushToast(
        "OpenSea stage mint queued",
        "ok",
        skipped > 0
          ? `${queued} queued · ${skipped} skipped · qty ${qty}`
          : `${queued} wallet(s) · qty ${qty}`,
      );
      if (skipped > 0) {
        setOpenSeaErr(
          result.skipped
            .map((s) => `#${s.wallet_id}: ${s.reason}`)
            .join("; ")
            .slice(0, 400),
        );
      }
      suppressNewTaskToast.current = true;
      setShowForm(false);
      setForm(emptyForm);
      setSeaDrop(null);
      setDropErr(null);
      setOpenSeaPlan(null);
      setOpenSeaErr(null);
      setAllowlistMatch(null);
      setAllowlistErr(null);
      setSelectedWalletIds([]);
      setSelectedRpcUrls([]);
      await load();
      suppressNewTaskToast.current = false;
    } catch (e) {
      setOpenSeaErr(String(e));
      pushToast("OpenSea mint failed", "error", String(e).slice(0, 160));
    } finally {
      setEnqueueingOpenSea(false);
    }
  }

  async function onRun() {
    setRunning(true);
    setErr(null);
    try {
      await ipc("mint_run");
      await load();
    } catch (e) {
      setErr(String(e));
    } finally {
      setRunning(false);
    }
  }

  async function onCancel(id: number) {
    try {
      await ipc("mint_cancel", { id });
      await load();
    } catch (e) {
      setErr(String(e));
    }
  }

  const hasPending = tasks.some((t) => t.status === "pending");
  const matchedAllowlist =
    allowlistMatch?.filter((m) => m.matched).length ?? selectedWalletIds.length;
  const canSubmit =
    form.chainId &&
    form.contract.trim() &&
    selectedWalletIds.length > 0 &&
    (!form.isHex || form.hexCalldata.trim().length >= 2) &&
    (!form.allowlist.trim() || matchedAllowlist > 0);

  // Poll task list so toasts fire when run_pending finishes in the background.
  useEffect(() => {
    const iv = window.setInterval(() => {
      void ipc<MintTaskRow[]>("mint_list")
        .then((t) => {
          applyTaskNotifications(t);
          setTasks(t);
        })
        .catch(() => {
          /* keep last known list */
        });
    }, 4000);
    return () => window.clearInterval(iv);
  }, []);

  return (
    <div className="p-6">
      <PageHeader
        suite="Operations"
        title="Minting"
        subtitle="Queue mint tasks — signed and broadcast from local vault"
        action={
          <div className="flex gap-2">
            <button
              onClick={onRun}
              disabled={running || !hasPending}
              className="flex items-center gap-1.5 rounded-lg border border-line bg-card px-3 py-2 text-[13px] text-fg hover:border-muted/40 disabled:opacity-40"
            >
              <Play className="h-4 w-4" /> {running ? "Running…" : "Run queue"}
            </button>
            <button
              onClick={() => {
                setErr(null);
                setSeaDrop(null);
                setDropErr(null);
                setOpenSeaPlan(null);
                setOpenSeaErr(null);
                setShowForm(true);
              }}
              className="flex items-center gap-1.5 rounded-lg bg-fg px-3 py-2 text-[13px] font-semibold text-bg hover:opacity-90"
            >
              <Plus className="h-4 w-4" /> New task
            </button>
          </div>
        }
      />

      {showForm ? (
        <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/60 p-4">
          <form
            onSubmit={onEnqueue}
            className="flex max-h-[90vh] w-full max-w-3xl flex-col overflow-hidden rounded-[16px] border border-line bg-panel shadow-2xl"
          >
            <div className="flex items-center justify-between border-b border-line px-5 py-4">
              <h2 className="text-[15px] font-semibold text-fg">Create Task</h2>
              <button
                type="button"
                onClick={() => setShowForm(false)}
                className="rounded-lg border border-line bg-card px-3 py-1.5 text-[12px] text-fg hover:border-muted/40"
              >
                Close
              </button>
            </div>
            {err ? (
              <div className="border-b border-danger/30 bg-danger/10 px-5 py-2 text-[12px] text-danger">
                {err}
              </div>
            ) : null}
            <div className="min-h-0 flex-1 overflow-y-auto p-5">
          {/* SeaDrop public mint: fetch plan from NFT collection address */}
          <div className="mb-3 rounded-lg border border-line bg-bg p-3">
            <div className="mb-2 flex flex-wrap items-end justify-between gap-2">
              <div className="min-w-0 flex-1">
                <label className="mb-1 block text-[11px] text-muted">
                  NFT Contract (SeaDrop public / OpenSea stage)
                </label>
                <input
                  placeholder="0x… collection address"
                  value={form.nftContract}
                  onChange={(e) => setForm({ ...form, nftContract: e.target.value })}
                  className="w-full rounded-lg border border-line bg-card px-3 py-2 font-mono text-[13px] outline-none focus:border-accent"
                />
              </div>
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={() => void onFetchDrop()}
                  disabled={fetchingDrop || !form.chainId}
                  className="flex h-[38px] items-center gap-1.5 rounded-lg border border-accent/60 bg-accent/15 px-3 text-[13px] font-medium text-fg hover:bg-accent/25 disabled:opacity-40"
                >
                  {fetchingDrop ? (
                    <Loader2 className="h-4 w-4 animate-spin" />
                  ) : (
                    <Link2 className="h-4 w-4" />
                  )}
                  Fetch public drop
                </button>
                <button
                  type="button"
                  onClick={() => void onFetchOpenSeaStage()}
                  disabled={
                    fetchingOpenSea ||
                    enqueueingOpenSea ||
                    !form.chainId ||
                    selectedWalletIds.length === 0
                  }
                  className="flex h-[38px] items-center gap-1.5 rounded-lg border border-accent/60 bg-accent/15 px-3 text-[13px] font-medium text-fg hover:bg-accent/25 disabled:opacity-40"
                >
                  {fetchingOpenSea ? (
                    <Loader2 className="h-4 w-4 animate-spin" />
                  ) : (
                    <Zap className="h-4 w-4" />
                  )}
                  OpenSea stage
                </button>
                <button
                  type="button"
                  onClick={() => void onEnqueueOpenSeaStage()}
                  disabled={
                    fetchingOpenSea ||
                    enqueueingOpenSea ||
                    !form.chainId ||
                    selectedWalletIds.length === 0
                  }
                  className="flex h-[38px] items-center gap-1.5 rounded-lg bg-accent px-3 text-[13px] font-semibold text-white disabled:opacity-40"
                >
                  {enqueueingOpenSea ? (
                    <Loader2 className="h-4 w-4 animate-spin" />
                  ) : (
                    <Zap className="h-4 w-4" />
                  )}
                  Enqueue stage
                </button>
              </div>
            </div>
            {dropErr ? <div className="text-[12px] text-danger">{dropErr}</div> : null}
            {openSeaErr ? (
              <div className="text-[12px] text-danger">{openSeaErr}</div>
            ) : null}
            {openSeaPlan ? (
              <div className="mt-1 flex flex-wrap gap-x-4 gap-y-1 font-mono text-[11px] text-muted">
                <span>
                  stage=<span className="text-fg">{openSeaPlan.stage_type}</span>
                  <span className="opacity-60">#{openSeaPlan.stage_index}</span>
                </span>
                <span>
                  slug=<span className="text-fg">{openSeaPlan.slug}</span>
                </span>
                <span>
                  to=<span className="text-fg">{shortAddress(openSeaPlan.to, 6)}</span>
                </span>
                <span>
                  value=<span className="text-fg">{openSeaPlan.value_eth}</span> ETH
                </span>
                <span>
                  qty=<span className="text-fg">{openSeaPlan.quantity}</span>
                </span>
                <span className="text-ok">MintAction validated</span>
              </div>
            ) : null}
            {seaDrop ? (
              <div className="mt-1 flex flex-wrap gap-x-4 gap-y-1 font-mono text-[11px] text-muted">
                <span>
                  to=<span className="text-fg">{shortAddress(seaDrop.to, 6)}</span>
                </span>
                <span>
                  price=<span className="text-fg">{seaDrop.drop.mint_price_wei}</span> wei
                </span>
                <span>
                  max/wallet=<span className="text-fg">{seaDrop.drop.max_per_wallet}</span>
                </span>
                <span>
                  window=
                  <span className="text-fg">
                    {seaDrop.drop.start_time} → {seaDrop.drop.end_time}
                  </span>
                </span>
                <span>
                  fee=
                  <span className="text-fg">{shortAddress(seaDrop.fee_recipient, 4)}</span>
                  <span className="opacity-60"> ({seaDrop.fee_source})</span>
                </span>
                <span className={seaDrop.live ? "text-ok" : "text-warn"}>
                  {seaDrop.live ? "live" : "not live"}
                </span>
              </div>
            ) : null}
          </div>

          {/* Row: Contract + Chain */}
          <div className="mb-3 grid grid-cols-2 gap-2">
            <div>
              <label className="mb-1 block text-[11px] text-muted">Contract Address</label>
              <input
                placeholder="0x…"
                value={form.contract}
                onChange={(e) => setForm({ ...form, contract: e.target.value })}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[13px] outline-none focus:border-accent"
              />
            </div>
            <div>
              <label className="mb-1 block text-[11px] text-muted">Chain</label>
              <select
                value={form.chainId}
                onChange={(e) => {
                  const cid = e.target.value;
                  const c = enabledChains.find((x) => String(x.chain_id) === cid);
                  setForm({ ...form, chainId: cid });
                  if (c?.rpc_url) setSelectedRpcUrls([c.rpc_url]);
                }}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              >
                <option value="">Select chain…</option>
                {enabledChains.map((c) => (
                  <option key={c.id} value={c.chain_id}>
                    {c.name} ({c.chain_id})
                  </option>
                ))}
              </select>
            </div>
          </div>

          {/* Function + HEX checkbox */}
          <div className="mb-3 grid grid-cols-[1fr_auto] gap-2">
            <div>
              <label className="mb-1 block text-[11px] text-muted">Function</label>
              {form.isHex ? (
                <input
                  placeholder="0xcalldata… ({address} allowed)"
                  value={form.hexCalldata}
                  onChange={(e) => setForm({ ...form, hexCalldata: e.target.value })}
                  className="w-full rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[13px] outline-none focus:border-accent"
                />
              ) : form.functionPreset === "custom" ? (
                <input
                  placeholder="transfer(address,uint256)"
                  value={form.customFn}
                  onChange={(e) => setForm({ ...form, customFn: e.target.value })}
                  className="w-full rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[13px] outline-none focus:border-accent"
                />
              ) : (
                <select
                  value={form.functionPreset}
                  onChange={(e) => setForm({ ...form, functionPreset: e.target.value })}
                  className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
                >
                  {FUNCTION_PRESETS.map((p) => (
                    <option key={p} value={p}>
                      {p}
                    </option>
                  ))}
                </select>
              )}
            </div>
            <div className="flex items-end pb-1">
              <label className="flex cursor-pointer select-none items-center gap-1.5 rounded-lg border border-line bg-bg px-3 py-2 text-[13px] text-fg">
                <input
                  type="checkbox"
                  checked={form.isHex}
                  onChange={(e) => setForm({ ...form, isHex: e.target.checked })}
                  className="h-3.5 w-3.5 accent-[var(--accent,#6d5efc)]"
                />
                HEX
              </label>
            </div>
          </div>

          {/* Parameters + Value */}
          <div className="mb-3 grid grid-cols-2 gap-2">
            <div>
              <label className="mb-1 block text-[11px] text-muted">
                Parameters{" "}
                <span className="opacity-60">
                  — to {"{address}"}; qty; {"{proof}"} / {"{signature}"}
                </span>
              </label>
              <input
                placeholder={'{address}; 1'}
                value={form.parameters}
                onChange={(e) => setForm({ ...form, parameters: e.target.value })}
                disabled={form.isHex}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[13px] outline-none focus:border-accent disabled:opacity-40"
              />
            </div>
            <div>
              <label className="mb-1 block text-[11px] text-muted">Value (ETH)</label>
              <input
                placeholder="0"
                value={form.valueEth}
                onChange={(e) => setForm({ ...form, valueEth: e.target.value })}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              />
            </div>
          </div>

          {/* GTD / Allowlist builder */}
          <div className="mb-3 rounded-lg border border-line bg-bg p-3">
            <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
              <div className="text-[11px] text-muted">
                GTD / Allowlist{" "}
                <span className="opacity-70">(optional — filter wallets + proof/sig)</span>
              </div>
              {allowlistMatch ? (
                <div className="font-mono text-[11px]">
                  <span className="text-ok">
                    {allowlistMatch.filter((m) => m.matched).length} matched
                  </span>
                  <span className="text-muted">
                    {" "}
                    / {allowlistMatch.length} selected ·{" "}
                    {allowlistMatch.filter((m) => m.has_proof).length} proof ·{" "}
                    {allowlistMatch.filter((m) => m.has_signature).length} sig
                  </span>
                </div>
              ) : null}
            </div>
            <textarea
              rows={3}
              placeholder={
                '["0xabc…"]  or  {"0xabc…": ["0xproof32b…"]}  or  newline addresses  ·  objects may use proof / signature / calldata'
              }
              value={form.allowlist}
              onChange={(e) => setForm({ ...form, allowlist: e.target.value })}
              spellCheck={false}
              className="w-full resize-y rounded-lg border border-line bg-card px-3 py-2 font-mono text-[12px] outline-none focus:border-accent"
            />
            {allowlistErr ? (
              <div className="mt-1 text-[12px] text-danger">{allowlistErr}</div>
            ) : null}
            {allowlistMatch && allowlistMatch.some((m) => !m.matched) ? (
              <div className="mt-1 text-[11px] text-warn">
                {allowlistMatch.filter((m) => !m.matched).length} selected wallet(s) will be
                skipped (not on allowlist).
              </div>
            ) : null}
          </div>

          {/* Wallets multi-select with group filter */}
          <div className="mb-3 rounded-lg border border-line bg-bg p-3">
            <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
              <div className="text-[11px] text-muted">
                Wallets <span className="text-fg">({selectedWalletIds.length} selected)</span>
              </div>
              <div className="flex flex-wrap gap-2">
                <select
                  value={walletGroupFilter}
                  onChange={(e) => {
                    const v = e.target.value;
                    setWalletGroupFilter(v === "all" || v === "ungrouped" ? v : Number(v));
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
                  onClick={selectVisibleWallets}
                  className="rounded border border-line px-2 py-1 text-[12px] text-fg hover:bg-line"
                >
                  Select visible
                </button>
                <button
                  type="button"
                  onClick={deselectAllWallets}
                  className="rounded border border-line px-2 py-1 text-[12px] text-muted hover:bg-line"
                >
                  Clear
                </button>
              </div>
            </div>
            <div className="flex max-h-40 flex-wrap gap-1.5 overflow-y-auto">
              {formWallets.length === 0 ? (
                <div className="text-[12px] text-muted">No wallets in this filter.</div>
              ) : (
                formWallets.map((w) => {
                  const selected = selectedWalletIds.includes(w.id);
                  const g = w.group_id != null ? groupNames.get(w.group_id) : null;
                  return (
                    <button
                      key={w.id}
                      type="button"
                      onClick={() => toggleWallet(w.id)}
                      title={w.address}
                      className={`flex items-center gap-1.5 rounded-lg border px-2.5 py-1.5 text-[12px] transition ${
                        selected
                          ? "border-accent bg-accent/15 text-fg"
                          : "border-line bg-card text-muted hover:border-muted/40"
                      }`}
                    >
                      <span className="font-medium">{w.label}</span>
                      {g ? (
                        <span className="rounded bg-line px-1 text-[10px] text-muted">{g}</span>
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

          {/* RPC endpoints + Flashbots */}
          <div className="mb-3 rounded-lg border border-line bg-bg p-3">
            <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
              <div className="text-[11px] text-muted">
                RPC Endpoints{" "}
                <span className="text-fg">({selectedRpcUrls.length} selected)</span>
                {!form.chainId ? (
                  <span className="ml-1 opacity-70">— select a chain first</span>
                ) : null}
              </div>
              <div className="flex flex-wrap gap-2">
                <button
                  type="button"
                  onClick={selectAllRpcs}
                  className="rounded border border-line px-2 py-1 text-[12px] text-fg hover:bg-line"
                >
                  All ({rpcOptions.length})
                </button>
                <label className="flex cursor-pointer select-none items-center gap-1.5 rounded border border-line px-2 py-1 text-[12px] text-fg">
                  <input
                    type="checkbox"
                    checked={form.flashbots}
                    onChange={(e) => setForm({ ...form, flashbots: e.target.checked })}
                    className="h-3.5 w-3.5 accent-[var(--accent,#6d5efc)]"
                  />
                  Flashbots
                </label>
              </div>
            </div>
            <div className="flex max-h-32 flex-wrap gap-1.5 overflow-y-auto">
              {rpcOptions.length === 0 ? (
                <div className="text-[12px] text-muted">No RPC endpoints configured.</div>
              ) : (
                rpcOptions.map((r) => {
                  const selected = selectedRpcUrls.includes(r.url);
                  return (
                    <button
                      key={r.url}
                      type="button"
                      onClick={() => toggleRpc(r.url)}
                      title={r.url}
                      className={`rounded-lg border px-2.5 py-1.5 text-[12px] transition ${
                        selected
                          ? "border-accent bg-accent/15 text-fg"
                          : "border-line bg-card text-muted hover:border-muted/40"
                      }`}
                    >
                      {r.label}
                    </button>
                  );
                })
              )}
            </div>
          </div>

          {/* Gas + nonce + schedule */}
          <div className="mb-3 grid grid-cols-3 gap-2">
            <div>
              <label className="mb-1 block text-[11px] text-muted">Gas Limit</label>
              <input
                placeholder="auto"
                value={form.gasLimit}
                onChange={(e) => setForm({ ...form, gasLimit: e.target.value })}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              />
            </div>
            <div>
              <label className="mb-1 block text-[11px] text-muted">Max Fee (gwei)</label>
              <input
                placeholder="auto"
                value={form.maxFee}
                onChange={(e) => setForm({ ...form, maxFee: e.target.value })}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              />
            </div>
            <div>
              <label className="mb-1 block text-[11px] text-muted">Priority Fee (gwei)</label>
              <input
                placeholder="auto"
                value={form.priorityFee}
                onChange={(e) => setForm({ ...form, priorityFee: e.target.value })}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              />
            </div>
            <div>
              <label className="mb-1 block text-[11px] text-muted">Nonce</label>
              <input
                placeholder="auto"
                value={form.nonce}
                onChange={(e) => setForm({ ...form, nonce: e.target.value })}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              />
            </div>
            <div>
              <label className="mb-1 block text-[11px] text-muted">Timestamp</label>
              <input
                type="datetime-local"
                value={form.timestamp}
                onChange={(e) => setForm({ ...form, timestamp: e.target.value })}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              />
            </div>
            <div>
              <label className="mb-1 block text-[11px] text-muted">Delay (ms)</label>
              <input
                placeholder="0"
                value={form.delayMs}
                onChange={(e) => setForm({ ...form, delayMs: e.target.value })}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              />
            </div>
          </div>

          {/* Quantity + mode toggles + submit */}
          <div className="flex flex-wrap items-end gap-3">
            <div className="w-24">
              <label className="mb-1 block text-[11px] text-muted">Qty</label>
              <input
                value={form.quantity}
                onChange={(e) => setForm({ ...form, quantity: e.target.value })}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
              />
            </div>

            <div className="flex flex-wrap gap-1.5">
              {(
                [
                  ["execute", "Execute"],
                  ["simulate", "Simulate"],
                  ["spam", "Spam"],
                  ["sweep", "Sweep"],
                ] as const
              ).map(([mode, label]) => {
                const active = form.mode === mode;
                return (
                  <button
                    key={mode}
                    type="button"
                    onClick={() => setForm({ ...form, mode })}
                    className={`flex items-center gap-1 rounded-lg border px-3 py-2 text-[13px] font-medium transition ${
                      active
                        ? mode === "simulate"
                          ? "border-sky-500 bg-sky-500/15 text-sky-300"
                          : mode === "spam"
                            ? "border-amber-500 bg-amber-500/15 text-amber-300"
                            : mode === "sweep"
                              ? "border-rose-500 bg-rose-500/15 text-rose-300"
                              : "border-accent bg-accent/20 text-fg"
                        : "border-line bg-bg text-muted hover:border-muted/40"
                    }`}
                  >
                    <Zap className="h-3.5 w-3.5" />
                    {label}
                  </button>
                );
              })}
            </div>

            <div className="ml-auto flex gap-2">
              <button
                type="button"
                className="rounded-lg border border-line px-3 py-2 text-[13px] text-muted hover:bg-line"
                onClick={() => {
                  setShowForm(false);
                  setSeaDrop(null);
                  setDropErr(null);
                  setOpenSeaPlan(null);
                  setOpenSeaErr(null);
                }}
              >
                Cancel
              </button>
              <button
                type="submit"
                disabled={!canSubmit}
                className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
              >
                Enqueue ({form.allowlist.trim() ? matchedAllowlist : selectedWalletIds.length})
              </button>
            </div>
          </div>
            </div>
          </form>
        </div>
      ) : null}

      {err ? <div className="mb-3 text-[12px] text-danger">{err}</div> : null}

      <div className="overflow-x-auto rounded-[14px] border border-line bg-card">
        <div className="grid min-w-[900px] grid-cols-[50px_1fr_100px_90px_90px_110px_180px_60px] gap-2 border-b border-line px-4 py-2.5 text-[11px] uppercase tracking-wide text-muted">
          <div>ID</div>
          <div>Contract</div>
          <div>Gas Fee</div>
          <div>Value</div>
          <div>Mode</div>
          <div>Chain</div>
          <div>Status</div>
          <div />
        </div>
        {tasks.length === 0 ? (
          <EmptyState
            title="No mint tasks"
            description="Enqueue a task targeting any contract you control or a public mint."
          />
        ) : (
          tasks.map((t) => (
            <div
              key={t.id}
              className="grid min-w-[900px] grid-cols-[50px_1fr_100px_90px_90px_110px_180px_60px] items-center gap-2 border-b border-line/60 px-4 py-2.5 text-[13px] last:border-0 hover:bg-line/30"
            >
              <div className="text-muted">#{t.id}</div>
              <div className="truncate font-mono text-[12px]" title={t.contract}>
                {shortAddress(t.contract, 6)}
                {t.function_name ? (
                  <span className="ml-1.5 text-muted">· {t.function_name}</span>
                ) : null}
                {t.is_hex && t.calldata ? (
                  <span className="ml-1.5 text-muted opacity-70">· hex</span>
                ) : null}
              </div>
              <div className="truncate text-[12px] text-muted" title={t.gas_limit || ""}>
                {t.gas_limit
                  ? `${t.gas_limit} g`
                  : t.max_fee_gwei
                    ? `${t.max_fee_gwei} gwei`
                    : "auto"}
              </div>
              <div className="truncate text-[12px] text-muted" title={t.value_wei || ""}>
                {formatValue(t.value_wei)}
              </div>
              <div>
                <ModeBadge mode={t.mode || "execute"} />
              </div>
              <div className="text-muted">{t.chain_id}</div>
              <div className="flex items-center gap-1.5 text-[12px] capitalize">
                <StatusDot
                  ok={t.status === "confirmed" || t.status === "simulated"}
                />
                {t.status === "confirmed" && t.tx_hash
                  ? `minted · ${shortAddress(t.tx_hash, 4)}`
                  : t.status}
                {t.error && t.status !== "confirmed" ? (
                  <span className="ml-1 truncate text-[10px] text-danger/80" title={t.error}>
                    ·
                  </span>
                ) : null}
                {t.tx_hash ? (
                  <button
                    type="button"
                    title={t.tx_hash}
                    onClick={() => {
                      const base = explorerByChain.get(t.chain_id);
                      if (!base || !t.tx_hash) return;
                      void openUrl(`${base}/tx/${t.tx_hash}`);
                    }}
                    className="inline-flex shrink-0 items-center text-muted transition hover:text-fg"
                  >
                    <ExternalLink className="h-3.5 w-3.5" />
                  </button>
                ) : null}
              </div>
              <div className="text-right">
                {t.status === "pending" ? (
                  <button
                    onClick={() => onCancel(t.id)}
                    className="rounded p-1 text-muted hover:bg-line hover:text-danger"
                    title="Cancel"
                  >
                    <X className="h-4 w-4" />
                  </button>
                ) : null}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

function ModeBadge({ mode }: { mode: string }) {
  const cls =
    mode === "simulate"
      ? "border-sky-500/50 text-sky-300"
      : mode === "spam"
        ? "border-amber-500/50 text-amber-300"
        : mode === "sweep"
          ? "border-rose-500/50 text-rose-300"
          : "border-line text-muted";
  return (
    <span className={`inline-flex rounded border px-1.5 py-0.5 text-[11px] capitalize ${cls}`}>
      {mode}
    </span>
  );
}

function formatValue(valueWei: string | null | undefined): string {
  if (!valueWei || valueWei === "0" || valueWei === "0x0") return "0";
  try {
    const wei = BigInt(valueWei);
    if (wei <= 0n) return "0";
    // 4 decimal places in wei units (0.0001 ETH = 1e14 wei) — exact at any
    // magnitude; Number() only used for the sub-0.0001 exponential display.
    const scaled = wei / 10n ** 14n;
    if (scaled === 0n) return (Number(wei) / 1e18).toExponential(1);
    const whole = scaled / 10000n;
    const frac = (scaled % 10000n).toString().padStart(4, "0").replace(/0+$/, "");
    return frac === "" ? whole.toString() : `${whole}.${frac}`;
  } catch {
    return valueWei;
  }
}

function ethToWei(eth: string): string | null {
  const s = eth.trim();
  if (!s) return "0";
  if (!/^\d+(\.\d+)?$/.test(s)) return null;
  const [whole, frac = ""] = s.split(".");
  if (frac.length > 18) return null;
  const fracPadded = (frac + "0".repeat(18)).slice(0, 18);
  const wei = BigInt(whole || "0") * 10n ** 18n + BigInt(fracPadded || "0");
  return wei.toString();
}
