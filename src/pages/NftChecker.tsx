import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ExternalLink, Search } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { listen } from "@tauri-apps/api/event";
import { ipc } from "../lib/ipc";
import type { ChainRow, NftItem } from "../lib/types";
import { EmptyState, PageHeader } from "../components/ui";
import { shortAddress } from "../lib/utils";
import {
  filterWalletsByGroup,
  groupNameMap,
  useWalletStore,
} from "../store/app";

type GroupFilter = number | "all" | "ungrouped";

interface ScanProgress {
  phase: string;
  done: number;
  total: number;
}

export function NftCheckerPage() {
  const { wallets, groups, load: loadWallets } = useWalletStore();
  const [chains, setChains] = useState<ChainRow[]>([]);
  const [walletId, setWalletId] = useState("");
  const [chainId, setChainId] = useState("");
  const [items, setItems] = useState<NftItem[]>([]);
  const [scanning, setScanning] = useState(false);
  const [progress, setProgress] = useState<ScanProgress | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [summary, setSummary] = useState<string | null>(null);
  const [walletGroupFilter, setWalletGroupFilter] = useState<GroupFilter>("all");
  const [windowBlocks, setWindowBlocks] = useState(() => {
    const saved = localStorage.getItem("nft.windowBlocks.v2");
    return saved !== null ? Number(saved) || 0 : 0;
  });
  const [fromCache, setFromCache] = useState(false);
  const loadedRef = useRef(false);

  const groupNames = useMemo(() => groupNameMap(groups), [groups]);
  const formWallets = useMemo(
    () => filterWalletsByGroup(wallets, walletGroupFilter),
    [wallets, walletGroupFilter],
  );
  const enabledChains = useMemo(() => chains.filter((c) => c.enabled), [chains]);

  const loadChainsAndWallets = useCallback(async () => {
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

  // Load cached results for the selected wallet/chain on first open.
  const loadCache = useCallback(async (wid: string, cid: string) => {
    if (!wid) return;
    try {
      const cached = await ipc<NftItem[]>("gallery_list", {
        walletId: Number(wid),
      });
      const filtered = cid ? cached.filter((n) => n.chain_id === Number(cid)) : cached;
      setItems(filtered);
      setFromCache(true);
      if (filtered.length > 0) {
        setSummary(`${filtered.length} cached result(s) — rescan to refresh`);
      }
    } catch {
      // cache is best-effort; ignore errors on open
    }
  }, []);

  useEffect(() => {
    if (loadedRef.current) return;
    loadedRef.current = true;
    void loadChainsAndWallets();
  }, [loadChainsAndWallets]);

  useEffect(() => {
    if (walletId && chainId) void loadCache(walletId, chainId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [walletId, chainId]);

  useEffect(() => {
    const un = listen<ScanProgress>("nft-scan-progress", (e) => {
      setProgress(e.payload);
    });
    return () => {
      void un.then((f) => f());
    };
  }, []);

  function onWindowChange(v: number) {
    const n = Math.max(0, Math.min(100_000_000, v || 0));
    setWindowBlocks(n);
    localStorage.setItem("nft.windowBlocks.v2", String(n));
  }

  async function onScan() {
    if (!walletId || !chainId) return;
    setScanning(true);
    setErr(null);
    setSummary(null);
    setFromCache(false);
    setProgress({ phase: "logs", done: 0, total: 0 });
    try {
      const r = await ipc<{ scanned: number; items: NftItem[]; address: string }>(
        "nft_scan",
        {
          walletId: Number(walletId),
          address: null,
          chainId: Number(chainId),
          windowBlocks,
        },
      );
      setItems(r.items);
      setSummary(`${r.scanned} NFT(s) for ${shortAddress(r.address, 6)}`);
    } catch (e) {
      setErr(String(e));
    } finally {
      setScanning(false);
      setProgress(null);
    }
  }

  const progressLabel = progress
    ? progress.phase === "logs"
      ? progress.total > 0
        ? `Scanning logs ${progress.done}/${progress.total}`
        : "Scanning logs…"
      : progress.phase === "opensea"
        ? "Checking OpenSea…"
        : `Metadata ${progress.done}/${progress.total}`
    : null;
  const progressPct =
    progress && progress.total > 0
      ? Math.min(100, Math.round((progress.done / progress.total) * 100))
      : 0;

  return (
    <div className="p-6">
      <PageHeader
        suite="Operations"
        title="NFT Checker"
        subtitle="Scan ERC-721 holdings via Transfer logs (window 0 = full history)"
        action={
          <button
            onClick={onScan}
            disabled={scanning || !walletId || !chainId}
            className="flex items-center gap-1.5 rounded-lg bg-fg px-3 py-2 text-[13px] font-semibold text-bg hover:opacity-90 disabled:opacity-40"
          >
            <Search className="h-4 w-4" />
            {scanning ? "Scanning…" : "Scan holdings"}
          </button>
        }
      />

      <div className="mb-4 flex flex-wrap gap-2">
        <select
          value={walletGroupFilter}
          onChange={(e) => {
            const v = e.target.value;
            setWalletGroupFilter(v === "all" || v === "ungrouped" ? v : Number(v));
          }}
          className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
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
          className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
        >
          <option value="">Wallet…</option>
          {formWallets.map((w) => {
            const g = w.group_id != null ? groupNames.get(w.group_id) : null;
            return (
              <option key={w.id} value={w.id}>
                {g ? `${w.label} [${g}] (${shortAddress(w.address)})` : `${w.label} (${shortAddress(w.address)})`}
              </option>
            );
          })}
        </select>
        <select
          value={chainId}
          onChange={(e) => setChainId(e.target.value)}
          className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
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
            min={0}
            max={100_000_000}
            step={10_000}
            value={windowBlocks}
            disabled={scanning}
            onChange={(e) => onWindowChange(Number(e.target.value))}
            className="w-24 rounded-lg border border-line bg-bg px-2 py-2 text-[13px] outline-none focus:border-accent disabled:opacity-50"
          />
          <span className="text-[11px]">blocks — 0 = full history</span>
        </label>
        {summary ? (
          <div className={`self-center text-[13px] ${fromCache ? "text-muted" : "text-ok"}`}>
            {summary}
          </div>
        ) : null}
      </div>

      {scanning && progressLabel ? (
        <div className="mb-4 rounded-[14px] border border-line bg-card p-3">
          <div className="mb-2 flex items-center justify-between text-[12px]">
            <span className="text-fg">{progressLabel}</span>
            <span className="text-muted">{progressPct}%</span>
          </div>
          <div className="h-1.5 overflow-hidden rounded-full bg-line">
            <div
              className="h-full rounded-full bg-accent transition-all duration-200"
              style={{ width: `${progressPct || (progress?.phase === "logs" ? 10 : 0)}%` }}
            />
          </div>
        </div>
      ) : null}

      {err ? <div className="mb-3 text-[12px] text-danger">{err}</div> : null}

      {items.length === 0 ? (
        <div className="rounded-[14px] border border-line bg-card">
          <EmptyState
            title="No results yet"
            description="Select a wallet and chain, then scan. Results are cached to Gallery."
          />
        </div>
      ) : (
        <div className="grid grid-cols-2 gap-3 md:grid-cols-4 lg:grid-cols-6">
          {items.map((n, i) => (
            <div
              key={i}
              className="group relative overflow-hidden rounded-xl border border-line bg-card"
            >
              <div className="aspect-square bg-line/40">
                {n.image ? (
                  <img
                    src={n.image}
                    alt={n.name || n.token_id}
                    loading="lazy"
                    className="h-full w-full object-cover"
                  />
                ) : (
                  <div className="flex h-full items-center justify-center text-[11px] text-muted">
                    no image
                  </div>
                )}
              </div>
              {n.opensea_url ? (
                <button
                  onClick={() => void openUrl(n.opensea_url!)}
                  title="View on OpenSea"
                  className="absolute right-1.5 top-1.5 rounded-md border border-line bg-bg/90 p-1.5 text-muted opacity-90 transition hover:text-fg group-hover:opacity-100"
                >
                  <ExternalLink className="h-3.5 w-3.5" />
                </button>
              ) : null}
              <div className="p-2">
                <div className="truncate text-[12px] font-medium">{n.name || `#${n.token_id}`}</div>
                <div className="truncate font-mono text-[10px] text-muted">
                  {shortAddress(n.contract, 4)} · #{n.token_id}
                </div>
                {n.opensea_url ? (
                  <button
                    onClick={() => void openUrl(n.opensea_url!)}
                    className="mt-1 text-[10px] text-accent hover:underline"
                  >
                    View on OpenSea ↗
                  </button>
                ) : null}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
