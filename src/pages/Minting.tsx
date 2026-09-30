import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { ExternalLink, Link2, Loader2, Play, Plus, X, Zap } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ipc } from "../lib/ipc";
import {
  fnDefaults,
  fnHint,
  fnSignature,
  isAddress,
  loadAbi,
  type AbiFn,
  type AbiInfo,
} from "../lib/abi";
import type {
  AllowlistMatchRow,
  ChainRow,
  EnqueueBatchResult,
  MintTaskRow,
  OpenSeaMintPlan,
  SeaDropPlan,
} from "../lib/types";
import { EmptyState, MultiSelectDropdown, PageHeader, StatusDot, pushToast, ConfirmDialog, useDismissOnEscape, type DropdownGroup, type StatusTone } from "../components/ui";
import { shortAddress } from "../lib/utils";
import { useWalletStore } from "../store/app";
import { suppressNextNewTaskToast, useMintTaskFeed } from "../store/tasks";

type Mode = "execute" | "simulate" | "spam" | "sweep";

const FUNCTION_PRESETS = [
  "mint()",
  "mint(uint256)",
  "mint(address,uint256)",
  "safeMint(address,uint256)",
  "publicMint(uint256)",
  "custom",
] as const;

/** Unix seconds → compact local stamp for the drop window readout. */
function fmtTsLocal(sec: number): string {
  return new Date(sec * 1000).toLocaleString(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}

/** Value for <input type="datetime-local">; the browser parses it as local time. */
function toDatetimeLocal(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

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
  // Task list + status toasts come from the app-wide feed (polled in AppShell)
  // so confirm/fail notifications fire on any page, not just this one.
  const tasks = useMintTaskFeed((s) => s.tasks);
  const refreshTasks = useMintTaskFeed((s) => s.refresh);
  const [chains, setChains] = useState<ChainRow[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [running, setRunning] = useState(false);
  /** In-flight guard for Queue: a double-click/Enter must not mint twice. */
  const [enqueueing, setEnqueueing] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [selectedWalletIds, setSelectedWalletIds] = useState<number[]>([]);
  const [selectedRpcUrls, setSelectedRpcUrls] = useState<string[]>([]);
  /** Extra endpoints pasted in for the current chain (probed before adding). */
  const [customRpcUrls, setCustomRpcUrls] = useState<string[]>([]);
  const [newRpcUrl, setNewRpcUrl] = useState("");
  const [probingRpc, setProbingRpc] = useState(false);
  const [rpcAddErr, setRpcAddErr] = useState<string | null>(null);
  const [abiInfo, setAbiInfo] = useState<AbiInfo | null>(null);
  const [abiLoading, setAbiLoading] = useState(false);
  const [abiErr, setAbiErr] = useState<string | null>(null);
  const [form, setForm] = useState(emptyForm);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [seaDrop, setSeaDrop] = useState<SeaDropPlan | null>(null);
  const [fetchingDrop, setFetchingDrop] = useState(false);
  const [dropErr, setDropErr] = useState<string | null>(null);
  /** Broadcast lead: fire the tx this many ms before the phase opens. */
  const [bcLead, setBcLead] = useState("3000");
  const [allowlistMatch, setAllowlistMatch] = useState<AllowlistMatchRow[] | null>(null);
  const [allowlistErr, setAllowlistErr] = useState<string | null>(null);
  const [openSeaPlan, setOpenSeaPlan] = useState<OpenSeaMintPlan | null>(null);
  const [fetchingOpenSea, setFetchingOpenSea] = useState(false);
  const [openSeaErr, setOpenSeaErr] = useState<string | null>(null);
  const [enqueueingOpenSea, setEnqueueingOpenSea] = useState(false);
  const [encodedPreview, setEncodedPreview] = useState<string | null>(null);
  const [encoding, setEncoding] = useState(false);
  const [encodeErr, setEncodeErr] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [statusFilter, setStatusFilter] = useState<"all" | "draft" | "pending" | "done" | "failed">("all");
  /** Cancel is destructive and irreversible — needs an explicit confirm. */
  const [cancelTarget, setCancelTarget] = useState<MintTaskRow | null>(null);
  const [cancelling, setCancelling] = useState(false);
  /** RPC selection is initialised exactly once per selected chain. */
  const rpcInitKey = useRef<string | null>(null);

  type Wallet = (typeof wallets)[number];
  // Wallet picker groups: every store group with wallets + an "Ungrouped" bucket.
  const walletGroups = useMemo(() => {
    const out: { key: string; title: string; items: Wallet[] }[] = [];
    for (const g of groups) {
      const items = wallets.filter((w) => w.group_id === g.id);
      if (items.length) out.push({ key: `g${g.id}`, title: g.name, items });
    }
    const loose = wallets.filter((w) => w.group_id == null);
    if (loose.length) out.push({ key: "ungrouped", title: "Ungrouped", items: loose });
    return out;
  }, [wallets, groups]);

  // Function currently selected in the ABI-derived list (null for presets/custom).
  const selectedFn = useMemo<AbiFn | null>(() => {
    if (!abiInfo || form.functionPreset === "custom") return null;
    return abiInfo.fns.find((f) => fnSignature(f) === form.functionPreset) ?? null;
  }, [abiInfo, form.functionPreset]);

  const abiSigSet = useMemo(
    () => new Set((abiInfo?.fns ?? []).map(fnSignature)),
    [abiInfo],
  );
  // Values the <option> list can show — anything else gets an escape-hatch option.
  const knownFnValues = useMemo(
    () => new Set<string>([...abiSigSet, ...FUNCTION_PRESETS]),
    [abiSigSet],
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
  // Extra endpoints pasted for this chain are appended as a "custom" group.
  const rpcOptions = useMemo(() => {
    const list: { url: string; label: string }[] = [];
    const seen = new Set<string>();
    const push = (raw: string, label: string) => {
      const url = raw.trim();
      if (!url || seen.has(url)) return;
      seen.add(url);
      list.push({ url, label });
    };
    for (const c of enabledChains) {
      if (!form.chainId || String(c.chain_id) !== form.chainId) continue;
      push(c.rpc_url, c.name);
    }
    for (const url of customRpcUrls) push(url, "custom");
    return list;
  }, [enabledChains, form.chainId, customRpcUrls]);

  // Drop RPC selections that no longer belong to the chosen chain.
  useEffect(() => {
    setSelectedRpcUrls((prev) => {
      const kept = prev.filter((u) => rpcOptions.some((r) => r.url === u));
      return kept.length === prev.length ? prev : kept;
    });
  }, [rpcOptions]);

  // New chain selected → start with EVERY endpoint selected, so the runner
  // races them instead of depending on a single (possibly dead) RPC.
  useEffect(() => {
    const key = form.chainId || "";
    if (rpcInitKey.current === key) return;
    rpcInitKey.current = key;
    setSelectedRpcUrls(rpcOptions.map((r) => r.url));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form.chainId, rpcOptions]);

  // ABI lookup (Sourcify) for the contract target — debounced, cached per
  // contract+chain so typing an address doesn't refetch.
  useEffect(() => {
    const addr = form.contract.trim();
    if (!form.chainId || !isAddress(addr)) {
      setAbiInfo(null);
      setAbiErr(null);
      setAbiLoading(false);
      return;
    }
    let stale = false;
    setAbiLoading(true);
    const timer = window.setTimeout(() => {
      const chain = enabledChains.find((c) => String(c.chain_id) === form.chainId);
      loadAbi(Number(form.chainId), addr, chain?.rpc_url || null)
        .then((info) => {
          if (stale) return;
          setAbiInfo(info);
          setAbiErr(info ? null : "No verified ABI on Sourcify — presets only");
          setAbiLoading(false);
        })
        .catch((e: unknown) => {
          if (stale) return;
          setAbiInfo(null);
          setAbiErr(String(e).slice(0, 140));
          setAbiLoading(false);
        });
    }, 350);
    return () => {
      stale = true;
      window.clearTimeout(timer);
    };
  }, [form.contract, form.chainId, enabledChains]);

  const load = useCallback(async () => {
    try {
      const [c] = await Promise.all([
        ipc<ChainRow[]>("chain_list"),
        refreshTasks(),
        loadWallets(),
      ]);
      setChains(c);
    } catch (e) {
      setErr(String(e));
    }
  }, [loadWallets, refreshTasks]);

  useEffect(() => {
    void load();
  }, [load]);

  function toggleWallet(id: number) {
    setSelectedWalletIds((ids) =>
      ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id],
    );
  }

  function selectVisibleWallets() {
    setSelectedWalletIds(wallets.map((w) => w.id));
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

  /** Pick a function — prefills Parameters from the ABI signature. */
  function pickFunction(sig: string) {
    const fn: AbiFn | null =
      sig === "custom"
        ? null
        : abiInfo?.fns.find((f) => fnSignature(f) === sig) ?? null;
    setForm((prev) => ({
      ...prev,
      functionPreset: sig,
      parameters: fn ? fnDefaults(fn) : prev.parameters,
    }));
    setEncodedPreview(null);
    setEncodeErr(null);
  }

  const rpcGroups: DropdownGroup[] = useMemo(() => {
    const chain = enabledChains.find((c) => String(c.chain_id) === form.chainId);
    const picked = rpcOptions.filter((r) => selectedRpcUrls.includes(r.url)).length;
    return [
      {
        key: "rpc",
        title: chain ? `${chain.name} · chain ${chain.chain_id}` : "No chain selected",
        selected: picked,
        total: rpcOptions.length,
        onToggleAll: selectAllRpcs,
        items: rpcOptions.map((r) => ({
          key: r.url,
          label: r.label,
          sub: r.url,
          checked: selectedRpcUrls.includes(r.url),
          onToggle: () => toggleRpc(r.url),
        })),
      },
    ];
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rpcOptions, selectedRpcUrls, enabledChains, form.chainId]);

  /** Probe + append an extra endpoint for the current chain. */
  async function onAddRpc() {
    const url = newRpcUrl.trim();
    if (!/^https?:\/\/\S+$/.test(url)) {
      setRpcAddErr("Enter an http(s) RPC URL");
      return;
    }
    if (rpcOptions.some((r) => r.url === url)) {
      setRpcAddErr("Endpoint already listed");
      return;
    }
    setProbingRpc(true);
    setRpcAddErr(null);
    try {
      const probe = await ipc<{ chain_id: number | null; error: string | null }>(
        "chain_probe",
        { rpcUrl: url },
      );
      if (probe.error || probe.chain_id == null) throw new Error(probe.error ?? "probe failed");
      if (String(probe.chain_id) !== form.chainId) {
        throw new Error(`Endpoint is chain ${probe.chain_id}, expected ${form.chainId}`);
      }
      setCustomRpcUrls((prev) => [...prev, url]);
      setSelectedRpcUrls((prev) => [...prev, url]);
      setNewRpcUrl("");
    } catch (e) {
      setRpcAddErr(String(e).slice(0, 140));
    } finally {
      setProbingRpc(false);
    }
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
    if (enqueueing) return; // re-entrancy guard: Enter/double-click during submit
    setErr(null);
    setEnqueueing(true);
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
      // The queued-toast above already covers it — skip the feed's duplicate.
      suppressNextNewTaskToast();
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
    } catch (e2) {
      setErr(String(e2));
    } finally {
      setEnqueueing(false);
    }
  }

  /**
   * Schedule the broadcast `bcLead` ms before the phase opens so the tx waits
   * in the mempool and lands at/after T-0 (OSNM-Z PUBLIC_MINT_BROADCAST_OFFSET_MS).
   * Mining it early reverts with SeaDrop's NotActive, so the lead is clamped to 60s.
   */
  function applyDropSchedule() {
    if (!seaDrop) return;
    const lead = Math.min(60_000, Math.max(0, Math.floor(Number(bcLead) || 0)));
    setForm((f) => ({
      ...f,
      timestamp: toDatetimeLocal(seaDrop.drop.start_time * 1000 - lead),
    }));
    pushToast(
      "Schedule set",
      "ok",
      `broadcast ${lead}ms before start (${fmtTsLocal(seaDrop.drop.start_time)})`,
    );
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
      pushToast("OpenSea fetch failed", "error", msg.slice(0, 160));
    } finally {
      setFetchingOpenSea(false);
    }
  }

  async function onEnqueueOpenSeaStage() {
    if (enqueueingOpenSea) return; // re-entrancy guard: no double-submit
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
      // A future schedule defers the SIWE/stage call: mint data only exists
      // once the stage opens, so the runner resolves it when the task fires.
      const deferred = scheduledAt !== null && scheduledAt > Date.now();
      pushToast(
        deferred ? "OpenSea mint scheduled" : "OpenSea mint queued",
        "ok",
        (skipped > 0
          ? `${queued} queued · ${skipped} skipped · qty ${qty}`
          : `${queued} wallet(s) · qty ${qty}`) +
          (deferred ? " · calldata at fire time" : ""),
      );
      if (skipped > 0) {
        setOpenSeaErr(
          result.skipped
            .map((s) => `#${s.wallet_id}: ${s.reason}`)
            .join("; ")
            .slice(0, 400),
        );
      }
      suppressNextNewTaskToast();
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

  async function onRetry(id: number) {
    setErr(null);
    try {
      await ipc("mint_retry", { id });
      pushToast("Task retried", "ok", `#${id} → pending`);
      await load();
    } catch (e) {
      setErr(String(e));
      pushToast("Retry failed", "error", String(e).slice(0, 160));
    }
  }

  async function onRetryAll() {
    setRunning(true);
    setErr(null);
    try {
      const n = await ipc<number>("mint_retry_all");
      pushToast("All failed retried", "ok", `${n} task(s) → pending`);
      await load();
    } catch (e) {
      setErr(String(e));
    } finally {
      setRunning(false);
    }
  }

  const hasFailed = tasks.some((t) => t.status === "failed" || t.status === "canceled" || t.status === "cancelled");

    async function onPromoteDrafts() {
    setPromoting(true);
    setErr(null);
    try {
      const n = await ipc<number>("mint_promote_all");
      pushToast("Drafts promoted", "ok", `${n} task(s) → pending — auto-run will pick them up`);
      await load();
    } catch (e) {
      setErr(String(e));
      pushToast("Promote failed", "error", String(e).slice(0, 160));
    } finally {
      setPromoting(false);
    }
  }

  async function onPromote(id: number) {
    setErr(null);
    try {
      await ipc("mint_promote", { id });
      pushToast("Draft promoted", "ok", `Task #${id} → pending`);
      await load();
    } catch (e) {
      setErr(String(e));
      pushToast("Promote failed", "error", String(e).slice(0, 160));
    }
  }

  /// Preview the exact calldata that will be signed for the first selected
  /// wallet.  Catches ABI mistakes (unpadded address, wrong arg count) before
  /// the batch is enqueued.
  async function onEncode() {
    setEncodeErr(null);
    setEncodedPreview(null);
    if (!form.functionPreset && !form.customFn) {
      setEncodeErr("Pick or type a function first");
      return;
    }
    if (form.isHex) {
      setEncodeErr("Encode works on Function mode — switch HEX off, or paste raw calldata");
      return;
    }
    const fn =
      form.functionPreset === "custom" ? form.customFn.trim() : form.functionPreset;
    if (!fn.includes("(")) {
      setEncodeErr("Function must look like name(types)");
      return;
    }
    if (selectedWalletIds.length === 0) {
      setEncodeErr("Select a wallet first — calldata embeds its address");
      return;
    }
    const w = wallets.find((x) => x.id === selectedWalletIds[0]);
    if (!w) {
      setEncodeErr("Selected wallet not found");
      return;
    }
    setEncoding(true);
    try {
      const cd = await ipc<string>("mint_encode_calldata", {
        functionName: fn,
        parameters: form.parameters.trim(),
        quantity: Math.max(1, Math.floor(Number(form.quantity) || 1)),
        walletAddress: w.address,
      });
      setEncodedPreview(cd);
    } catch (e) {
      setEncodeErr(String(e));
    } finally {
      setEncoding(false);
    }
  }

  async function onCopyEncoded() {
    if (!encodedPreview) return;
    try {
      await navigator.clipboard.writeText(encodedPreview);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      setEncodeErr("Clipboard blocked — select and copy manually");
    }
  }

  /// Ship the previewed calldata into the HEX field, so you can queue it
  /// verbatim without relying on the encoder at run time.
  function onUseEncodedAsHex() {
    if (!encodedPreview) return;
    setForm({ ...form, isHex: true, hexCalldata: encodedPreview });
    setEncodedPreview(null);
    setEncodeErr(null);
    pushToast("Copied to HEX field", "ok", "Re-select your wallets, then Queue");
  }

  async function onCancel(id: number) {
    setCancelling(true);
    try {
      await ipc("mint_cancel", { id });
      setCancelTarget(null);
      await load();
    } catch (e) {
      setErr(String(e));
    } finally {
      setCancelling(false);
    }
  }

  const statusCounts = useMemo(() => {
    const c = { all: tasks.length, draft: 0, pending: 0, done: 0, failed: 0 };
    for (const t of tasks) {
      if (t.status === "draft") c.draft++;
      else if (t.status === "confirmed" || t.status === "simulated") c.done++;
      else if (t.status === "failed" || t.status === "canceled" || t.status === "cancelled") c.failed++;
      else c.pending++;
    }
    return c;
  }, [tasks]);

  const filteredTasks = useMemo(
    () => statusFilter === "all" ? tasks : tasks.filter((t) => {
      if (statusFilter === "draft") return t.status === "draft";
      if (statusFilter === "pending") return t.status === "pending" || t.status === "signing" || t.status === "broadcasting";
      if (statusFilter === "done") return t.status === "confirmed" || t.status === "simulated";
      if (statusFilter === "failed") return t.status === "failed" || t.status === "canceled" || t.status === "cancelled";
      return true;
    }),
    [tasks, statusFilter],
  );

  const hasPending = tasks.some((t) => t.status === "pending");
  const hasDrafts = tasks.some((t) => t.status === "draft");
  const draftCount = tasks.filter((t) => t.status === "draft").length;
  const [promoting, setPromoting] = useState(false);
  const matchedAllowlist =
    allowlistMatch?.filter((m) => m.matched).length ?? selectedWalletIds.length;
  const canSubmit =
    form.chainId &&
    form.contract.trim() &&
    selectedWalletIds.length > 0 &&
    (!form.isHex || form.hexCalldata.trim().length >= 2) &&
    (!form.allowlist.trim() || matchedAllowlist > 0) &&
    !enqueueing;

  // Chain names for the task rows (resolved per chain_id, not raw ids).
  const chainNameById = useMemo(() => {
    const m = new Map<number, string>();
    for (const c of chains) m.set(c.chain_id, c.name);
    return m;
  }, [chains]);
  const walletById = useMemo(() => {
    const m = new Map<number, (typeof wallets)[number]>();
    for (const w of wallets) m.set(w.id, w);
    return m;
  }, [wallets]);

  useDismissOnEscape(showForm, () => setShowForm(false));

  // Task refreshes are handled app-wide by the mint task feed (AppShell).

  return (
    <div className="p-6">
      <PageHeader
        suite="Operations"
        title="Minting"
        subtitle="Queue mint tasks — signed and broadcast from local vault"
        action={
          <div className="flex gap-2">
            {hasDrafts ? (
              <button
                onClick={onPromoteDrafts}
                disabled={promoting}
                className="flex items-center gap-1.5 rounded-lg bg-accent px-3 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
                title="Promote all draft tasks to pending so the auto-run scheduler picks them up"
              >
                {promoting ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : (
                  <Play className="h-4 w-4" />
                )}
                Promote {draftCount} draft{draftCount !== 1 ? "s" : ""}
              </button>
            ) : null}
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
          {/* Row: Contract Target + Chain — first, everything below depends on it */}
          <div className="mb-3 grid grid-cols-2 gap-2">
            <div>
              <label className="mb-1 block text-[11px] text-muted">Contract Target</label>
              <input
                placeholder="0x…"
                value={form.contract}
                onChange={(e) => setForm({ ...form, contract: e.target.value })}
                className="w-full rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[13px] outline-none focus:border-accent"
              />
              <div className="mt-1 min-h-[14px] text-[11px]">
                {abiLoading ? (
                  <span className="text-muted">fetching ABI…</span>
                ) : abiErr ? (
                  <span className="text-warn">{abiErr}</span>
                ) : abiInfo && abiInfo.fns.length === 0 ? (
                  <span className="text-warn">{abiInfo.source}</span>
                ) : abiInfo ? (
                  <span className="text-ok">
                    {abiInfo.fns.length} callable · {abiInfo.source}
                    {abiInfo.viaProxy ? " · via implementation" : ""}
                  </span>
                ) : null}
              </div>
            </div>
            <div>
              <label className="mb-1 block text-[11px] text-muted">Chain</label>
              <select
                value={form.chainId}
                onChange={(e) => {
                  const cid = e.target.value;
                  const c = enabledChains.find((x) => String(x.chain_id) === cid);
                  setForm({ ...form, chainId: cid });
                  setCustomRpcUrls([]);
                  setAbiInfo(null);
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

          {/* SeaDrop public mint: fetch plan from NFT collection address */}
          <div className="mb-3 rounded-lg border border-line bg-bg p-3">
            <div className="mb-2 flex flex-wrap items-end justify-between gap-2">
              <div className="min-w-0 flex-1">
                <label className="mb-1 flex items-center gap-2 text-[11px] text-muted">
                  NFT Collection
                  {(seaDrop || openSeaPlan) ? (
                    <button
                      type="button"
                      onClick={() => { setSeaDrop(null); setOpenSeaPlan(null); setDropErr(null); setOpenSeaErr(null); }}
                      className="rounded border border-line px-1.5 py-0.5 text-[10px] hover:bg-line"
                    >clear plan</button>
                  ) : null}
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
                  SeaDrop
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
                    {fmtTsLocal(seaDrop.drop.start_time)} → {fmtTsLocal(seaDrop.drop.end_time)}
                  </span>
                </span>
                {seaDrop.remaining != null ? (
                  <span>
                    left=<span className="text-fg">{seaDrop.remaining}</span>
                    <span className="opacity-60">
                      {" "}
                      ({seaDrop.total_supply}/{seaDrop.max_supply} minted)
                    </span>
                  </span>
                ) : null}
                <span>
                  fee=
                  <span className="text-fg">{shortAddress(seaDrop.fee_recipient, 4)}</span>
                  <span className="opacity-60"> ({seaDrop.fee_source})</span>
                </span>
                <span className={seaDrop.live ? "text-ok" : "text-warn"}>
                  {seaDrop.live ? "live" : "not live"}
                </span>
                <span className="flex items-center gap-1 text-muted">
                  lead(ms)
                  <input
                    value={bcLead}
                    onChange={(e) => setBcLead(e.target.value)}
                    className="w-16 rounded border border-line bg-bg px-1 py-0.5 text-[11px] text-fg outline-none focus:border-accent"
                  />
                  <button
                    type="button"
                    onClick={applyDropSchedule}
                    className="rounded border border-line bg-bg px-2 py-0.5 text-[11px] text-fg hover:border-accent"
                    title="Set the schedule field to phase start minus the lead"
                  >
                    schedule = start − lead
                  </button>
                </span>
              </div>
            ) : null}
          </div>

          {/* Function + HEX checkbox */}
          <div className="mb-3 grid grid-cols-[1fr_auto] gap-2">
            <div>
              <label className="mb-1 block text-[11px] text-muted">
                {form.isHex ? (
                  <>Raw calldata <span className="opacity-60">— {"{address}"} auto-padded to 32 bytes</span></>
                ) : (
                  <>
                    Function{" "}
                    <span className="opacity-60">
                      — {selectedFn ? fnHint(selectedFn) : "from contract ABI"}
                    </span>
                  </>
                )}
              </label>
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
                  onChange={(e) => pickFunction(e.target.value)}
                  className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
                >
                  {abiInfo ? (
                    <optgroup label={`Contract ABI · ${abiInfo.fns.length} callable`}>
                      {abiInfo.fns.map((f) => {
                        const sig = fnSignature(f);
                        return (
                          <option key={sig} value={sig}>
                            {sig}
                          </option>
                        );
                      })}
                    </optgroup>
                  ) : null}
                  <optgroup label="Presets">
                    {FUNCTION_PRESETS.map((p) =>
                      abiSigSet.has(p) ? null : (
                        <option key={p} value={p}>
                          {p === "custom" ? "custom…" : p}
                        </option>
                      ),
                    )}
                  </optgroup>
                  {knownFnValues.has(form.functionPreset) ? null : (
                    <option value={form.functionPreset}>{form.functionPreset}</option>
                  )}
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

          {/* Encode preview — Function mode, any selected signature */}
          {!form.isHex &&
            (form.functionPreset === "custom" ? form.customFn.trim() : Boolean(form.functionPreset)) && (
            <div className="mb-3 rounded-lg border border-accent/30 bg-accent/5 p-3">
              <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
                <span className="text-[11px] text-accent">Calldata Preview</span>
                <button
                  type="button"
                  onClick={onEncode}
                  disabled={encoding || selectedWalletIds.length === 0}
                  className="flex items-center gap-1 rounded-lg border border-accent/40 bg-accent/10 px-2.5 py-1 text-[12px] text-accent hover:bg-accent/20 disabled:opacity-40"
                >
                  <Zap className="h-3 w-3" />
                  {encoding ? "Encoding..." : "Encode"}
                </button>
              </div>
              {encodeErr && (
                <p className="mb-2 text-[12px] text-danger">{encodeErr}</p>
              )}
              {encodedPreview && (
                <>
                  <div className="group relative">
                    <pre className="max-h-24 overflow-x-auto rounded-lg border border-line bg-bg p-2 font-mono text-[11px] text-fg leading-relaxed break-all whitespace-pre-wrap">
                      {encodedPreview}
                    </pre>
                    <button
                      type="button"
                      onClick={onCopyEncoded}
                      className="absolute right-2 top-2 rounded-md border border-line bg-card px-1.5 py-0.5 text-[10px] text-muted opacity-0 transition hover:text-fg group-hover:opacity-100"
                    >
                      {copied ? "copied" : "copy"}
                    </button>
                  </div>
                  <button
                    type="button"
                    onClick={onUseEncodedAsHex}
                    className="mt-2 rounded-lg border border-line bg-card px-2.5 py-1 text-[12px] text-fg hover:border-muted/40"
                  >
                    Use as HEX calldata →
                  </button>
                </>
              )}
              {!encodedPreview && !encodeErr && !encoding && (
                <p className="text-[11px] text-muted">
                  Click Encode to see the exact calldata for wallet #{selectedWalletIds[0] ?? "?"}
                </p>
              )}
            </div>
          )}

          {/* Parameters + Value */}
          <div className="mb-3 grid grid-cols-2 gap-2">
            <div>
              <label className="mb-1 block text-[11px] text-muted">
                Parameters{" "}
                <span className="opacity-60">
                  {selectedFn
                    ? `— ${fnHint(selectedFn)} · ${"{address}"} / ${"{quantity}"} / ${"{proof}"}`
                    : `— to {"{address}"}; qty; {"{proof}"} / {"{signature}"}`}
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

          {/* Wallets — grouped dropdown ("N of M wallets") */}
          <div className="mb-3 rounded-lg border border-line bg-bg p-3">
            <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
              <div className="text-[11px] text-muted">
                Wallets <span className="text-fg">({selectedWalletIds.length} selected)</span>
              </div>
              <div className="flex flex-wrap gap-2">
                <button
                  type="button"
                  onClick={selectVisibleWallets}
                  className="rounded border border-line px-2 py-1 text-[12px] text-fg hover:bg-line"
                >
                  Select all
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
            <MultiSelectDropdown
              summary={
                wallets.length === 0
                  ? "No wallets yet"
                  : `${selectedWalletIds.length} of ${wallets.length} wallets`
              }
              emptyText="No wallets — create one on the Wallets page first."
              groups={walletGroups.map((g) => {
                const ids = g.items.map((w) => w.id);
                const all = ids.length > 0 && ids.every((id) => selectedWalletIds.includes(id));
                return {
                  key: g.key,
                  title: g.title,
                  selected: ids.filter((id) => selectedWalletIds.includes(id)).length,
                  total: ids.length,
                  onToggleAll: () =>
                    setSelectedWalletIds((prev) =>
                      all
                        ? prev.filter((id) => !ids.includes(id))
                        : [...prev, ...ids.filter((id) => !prev.includes(id))],
                    ),
                  items: g.items.map((w) => ({
                    key: String(w.id),
                    label: w.label,
                    sub: shortAddress(w.address, 4),
                    checked: selectedWalletIds.includes(w.id),
                    onToggle: () => toggleWallet(w.id),
                  })),
                };
              })}
            />
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
            <MultiSelectDropdown
              summary={
                rpcOptions.length === 0
                  ? "No RPC endpoints"
                  : selectedRpcUrls.length === rpcOptions.length
                    ? `All ${rpcOptions.length} endpoints`
                    : `${selectedRpcUrls.length} of ${rpcOptions.length} endpoints`
              }
              emptyText={
                form.chainId ? "No RPC endpoints configured." : "Select a chain first."
              }
              groups={rpcGroups}
              footer={
                <>
                  <div className="flex items-center gap-2">
                    <input
                      placeholder="https://… add endpoint for this chain"
                      value={newRpcUrl}
                      onChange={(e) => setNewRpcUrl(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          void onAddRpc();
                        }
                      }}
                      spellCheck={false}
                      className="min-w-0 flex-1 rounded border border-line bg-bg px-2 py-1 font-mono text-[11.5px] outline-none focus:border-accent"
                    />
                    <button
                      type="button"
                      onClick={() => void onAddRpc()}
                      disabled={probingRpc}
                      className="shrink-0 rounded border border-line px-2 py-1 text-[12px] text-fg hover:bg-line disabled:opacity-40"
                    >
                      {probingRpc ? "Probing…" : "Add"}
                    </button>
                  </div>
                  {rpcAddErr ? (
                    <div className="mt-1 text-[11px] text-danger">{rpcAddErr}</div>
                  ) : null}
                </>
              }
            />
          </div>

          {/* Advanced settings (collapsible) */}
          <button type="button" onClick={() => setShowAdvanced(a => !a)}
            className="mb-2 flex items-center gap-1 text-[11px] text-muted hover:text-fg">
            <span className="text-[10px]">{showAdvanced ? "\u25bc" : "\u25b6"}</span> Advanced — Gas · Nonce · Schedule
          </button>
          {showAdvanced && (
            <div className="mb-3 rounded-lg border border-line bg-bg p-3">
              <div className="grid grid-cols-3 gap-2">
                <div><label className="mb-1 block text-[11px] text-muted">Gas Limit</label><input placeholder="auto" value={form.gasLimit} onChange={(e) => setForm({...form, gasLimit: e.target.value})} className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent" /></div>
                <div><label className="mb-1 block text-[11px] text-muted">Max Fee (gwei)</label><input placeholder="auto" value={form.maxFee} onChange={(e) => setForm({...form, maxFee: e.target.value})} className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent" /></div>
                <div><label className="mb-1 block text-[11px] text-muted">Priority Fee (gwei)</label><input placeholder="auto" value={form.priorityFee} onChange={(e) => setForm({...form, priorityFee: e.target.value})} className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent" /></div>
              </div>
              <div className="mt-2 grid grid-cols-3 gap-2">
                <div><label className="mb-1 block text-[11px] text-muted">Nonce</label><input placeholder="auto" value={form.nonce} onChange={(e) => setForm({...form, nonce: e.target.value})} className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent" /></div>
                <div><label className="mb-1 block text-[11px] text-muted">Timestamp</label><input type="datetime-local" value={form.timestamp} onChange={(e) => setForm({...form, timestamp: e.target.value})} className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent" /></div>
                <div><label className="mb-1 block text-[11px] text-muted">Delay (ms)</label><input placeholder="0" value={form.delayMs} onChange={(e) => setForm({...form, delayMs: e.target.value})} className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent" /></div>
              </div>
            </div>
          )}

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
                          ? "border-sky-500 bg-sky-500/15 text-sky-700 dark:text-sky-300"
                          : mode === "spam"
                            ? "border-amber-500 bg-amber-500/15 text-amber-700 dark:text-amber-300"
                            : mode === "sweep"
                              ? "border-rose-500 bg-rose-500/15 text-rose-700 dark:text-rose-300"
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
                title={
                  !form.chainId
                    ? "Select a chain first"
                    : !form.contract.trim()
                      ? "Contract target required (or fetch a SeaDrop/OpenSea plan)"
                      : !/^0x[0-9a-fA-F]{40}$/.test(form.contract.trim())
                        ? "Contract must be 0x + 40 hex"
                        : selectedWalletIds.length === 0
                          ? "Select at least one wallet"
                          : "Enqueue mint task(s)"
                }
                className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
              >
                {enqueueing
                  ? "Queueing…"
                  : `Enqueue (${form.allowlist.trim() ? matchedAllowlist : selectedWalletIds.length})`}
              </button>
            </div>
          </div>
            </div>
          </form>
        </div>
      ) : null}

      {err ? <div className="mb-3 text-[12px] text-danger">{err}</div> : null}

      {/* Status filter + retry failed */}
      <div className="mb-3 flex flex-wrap items-center gap-1.5">
        {(["all", "draft", "pending", "done", "failed"] as const).map((s) => (
          <button
            key={s}
            type="button"
            onClick={() => setStatusFilter(s)}
            className={`rounded-full border px-2.5 py-1 text-[11px] transition ${
              statusFilter === s
                ? "border-accent bg-accent/10 text-accent"
                : "border-line text-muted hover:text-fg"
            }`}
          >
            {s === "all" ? "All" : s === "done" ? "Done" : s.charAt(0).toUpperCase() + s.slice(1)}
            <span className="ml-1 opacity-60">{statusCounts[s]}</span>
          </button>
        ))}
        <div className="flex-1" />
        {hasFailed ? (
          <button
            type="button"
            onClick={() => void onRetryAll()}
            disabled={running}
            className="rounded-lg border border-warn/40 bg-warn/10 px-3 py-1.5 text-[12px] text-warn hover:bg-warn/20 disabled:opacity-40"
          >
            Retry all failed ({statusCounts.failed})
          </button>
        ) : null}
      </div>

      <div className="overflow-x-auto rounded-[14px] border border-line bg-card">
        <div className="grid min-w-[920px] grid-cols-[36px_1fr_130px_80px_66px_82px_72px_190px_50px] gap-2 border-b border-line px-4 py-2.5 text-[11px] uppercase tracking-wide text-muted">
          <div>ID</div>
          <div>Contract</div>
          <div>Wallet</div>
          <div>Gas</div>
          <div>Value</div>
          <div>Schedule</div>
          <div>Mode</div>
          <div>Status</div>
          <div />
        </div>
        {tasks.length === 0 ? (
          <EmptyState
            title="No mint tasks"
            description="Enqueue a task targeting any contract you control or a public mint."
          />
        ) : filteredTasks.length === 0 ? (
          <div className="px-4 py-6 text-center text-[13px] text-muted">No {statusFilter} tasks.</div>
        ) : (
          filteredTasks.map((t) => {
            const st = statusTone(t.status);
            const w = t.wallet_id != null ? walletById.get(t.wallet_id) : undefined;
            const chainName = chainNameById.get(t.chain_id);
            return (
            <div
              key={t.id}
              className="grid min-w-[920px] grid-cols-[36px_1fr_130px_80px_66px_82px_72px_190px_50px] items-center gap-2 border-b border-line/60 px-4 py-2.5 text-[13px] last:border-0 hover:bg-line/30"
            >
              <div className="text-muted">#{t.id}</div>
              <div className="min-w-0 overflow-hidden truncate font-mono text-[12px]" title={t.contract}>
                {shortAddress(t.contract, 6)}
                {t.function_name ? (
                  <span className="ml-1.5 text-muted">· {t.function_name}</span>
                ) : null}
                {t.is_hex && t.calldata ? (
                  <span className="ml-1.5 text-muted opacity-70">· hex</span>
                ) : null}
                {chainName ? (
                  <span className="ml-1.5 text-muted opacity-70">· {chainName}</span>
                ) : null}
              </div>
              <div
                className="min-w-0 truncate text-[12px]"
                title={w ? `${w.label} · ${w.address}` : t.wallet_id != null ? `wallet #${t.wallet_id}` : undefined}
              >
                {w ? (
                  <span className="truncate">{w.label}</span>
                ) : t.wallet_id != null ? (
                  <span className="text-muted">#{t.wallet_id}</span>
                ) : (
                  <span className="text-muted">—</span>
                )}
              </div>
              <div className="truncate text-[12px] text-muted" title={t.gas_limit ? `gas limit ${t.gas_limit}` : t.max_fee_gwei ? `max fee ${t.max_fee_gwei} gwei` : "auto"}>
                {t.gas_limit
                  ? t.gas_limit
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
              <div className="truncate text-[12px] text-muted" title={t.scheduled_at ? new Date(t.scheduled_at).toLocaleString() : ""}>
                {t.scheduled_at ? (t.scheduled_at > Date.now() ? (
                  <span className="text-warn">{
                    t.scheduled_at - Date.now() < 60000
                      ? `in ${Math.max(1, Math.ceil((t.scheduled_at - Date.now()) / 1000))}s`
                      : t.scheduled_at - Date.now() < 3600000
                        ? `in ${Math.ceil((t.scheduled_at - Date.now()) / 60000)}m`
                        : `in ${Math.ceil((t.scheduled_at - Date.now()) / 3600000)}h`
                  }</span>
                ) : <span className="opacity-50">now</span> ) : ((t.delay_ms ?? 0) > 0 ? (
                  <span className="opacity-50">+{t.delay_ms}ms</span>
                ) : null)}
              </div>
              <div className="flex min-w-0 flex-col gap-0.5 overflow-hidden text-[12px] capitalize">
                <div className="flex items-center gap-1.5">
                  <StatusDot tone={st.tone} pulse={st.pulse} />
                  <span className={`min-w-0 truncate ${st.tone === "danger" ? "text-danger" : ""}`}>
                    {t.status === "confirmed" && t.tx_hash
                      ? `minted · ${shortAddress(t.tx_hash, 4)}`
                      : t.status}
                  </span>
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
                {t.error && t.status !== "confirmed" ? (
                  <div className="flex min-w-0 items-center gap-1 pl-[13px]">
                    <span className="min-w-0 flex-1 truncate text-[10px] text-danger/80" title={t.error}>
                      {t.error.slice(0, 40)}
                      {t.error.length > 40 ? "..." : ""}
                    </span>
                    {(t.status === "failed" || t.status === "canceled" || t.status === "cancelled") && !t.tx_hash ? (
                      <button
                        type="button"
                        onClick={() => void onRetry(t.id)}
                        className="shrink-0 rounded border border-warn/40 px-1.5 py-0.5 text-[10px] text-warn hover:bg-warn/20"
                        title="Retry this task"
                      >
                        retry
                      </button>
                    ) : null}
                  </div>
                ) : null}
              </div>
              <div className="flex items-center justify-end gap-1.5 text-right">
                {t.status === "draft" ? (
                  <>
                    <span
                      className="rounded border border-warn/40 bg-warn/10 px-2 py-0.5 text-[10px] uppercase tracking-wide text-warn"
                      title="Draft — promote to queue"
                    >
                      draft
                    </span>
                    <button
                      onClick={() => onPromote(t.id)}
                      className="rounded p-1 text-accent hover:bg-line"
                      title="Promote to pending"
                    >
                      <Play className="h-3.5 w-3.5" />
                    </button>
                  </>
                ) : null}
                {t.status === "pending" || t.status === "draft" ? (
                  <button
                    onClick={() => setCancelTarget(t)}
                    className="rounded p-1 text-muted hover:bg-line hover:text-danger"
                    title="Cancel (asks for confirmation)"
                  >
                    <X className="h-4 w-4" />
                  </button>
                ) : null}
              </div>
            </div>
            );
          })
        )}
      </div>

      <ConfirmDialog
        open={cancelTarget != null}
        title={cancelTarget ? `Cancel mint task #${cancelTarget.id}?` : ""}
        body="The queued task is deleted. This cannot be undone — tasks already broadcasting keep running."
        confirmLabel="Cancel task"
        cancelLabel="Keep"
        danger
        busy={cancelling}
        onConfirm={() => cancelTarget && void onCancel(cancelTarget.id)}
        onClose={() => setCancelTarget(null)}
      />
    </div>
  );
}

/** Dot + text tone per task status; in-flight states breathe. */
function statusTone(status: string): { tone: StatusTone; pulse: boolean } {
  if (status === "confirmed" || status === "simulated")
    return { tone: "ok", pulse: false };
  if (status === "failed" || status === "canceled" || status === "cancelled")
    return { tone: "danger", pulse: false };
  if (status === "draft") return { tone: "warn", pulse: false };
  if (status === "signing" || status === "broadcasting")
    return { tone: "info", pulse: true };
  if (status === "pending") return { tone: "info", pulse: false };
  return { tone: "idle", pulse: false };
}

function ModeBadge({ mode }: { mode: string }) {
  const cls =
    mode === "simulate"
      ? "border-sky-500/50 text-sky-700 dark:text-sky-300"
      : mode === "spam"
        ? "border-amber-500/50 text-amber-700 dark:text-amber-300"
        : mode === "sweep"
          ? "border-rose-500/50 text-rose-700 dark:text-rose-300"
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
