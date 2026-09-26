import { useCallback, useEffect, useMemo, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink, Search } from "lucide-react";
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

function ageLabel(ts: number): string {
  const mins = Math.floor((Date.now() - ts) / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

export function GalleryPage() {
  const { wallets, groups, load: loadWallets } = useWalletStore();
  const [items, setItems] = useState<NftItem[]>([]);
  const [chains, setChains] = useState<ChainRow[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [walletId, setWalletId] = useState("");
  const [walletGroupFilter, setWalletGroupFilter] = useState<GroupFilter>("all");
  const [chainFilter, setChainFilter] = useState("");
  const [query, setQuery] = useState("");

  const groupNames = useMemo(() => groupNameMap(groups), [groups]);
  const formWallets = useMemo(
    () => filterWalletsByGroup(wallets, walletGroupFilter),
    [wallets, walletGroupFilter],
  );
  const chainMap = useMemo(() => {
    const m = new Map<number, ChainRow>();
    for (const c of chains) m.set(c.chain_id, c);
    return m;
  }, [chains]);

  const load = useCallback(async () => {
    try {
      const c = await ipc<ChainRow[]>("chain_list");
      setChains(c);
      await loadWallets();
      const id = walletId ? Number(walletId) : null;
      setItems(await ipc<NftItem[]>("gallery_list", { walletId: id }));
    } catch (e) {
      setErr(String(e));
    }
  }, [loadWallets, walletId]);

  useEffect(() => {
    void load();
  }, [load]);

  const chainsWithItems = useMemo(() => {
    const ids = new Set(items.map((n) => n.chain_id));
    return chains.filter((c) => ids.has(c.chain_id));
  }, [items, chains]);

  const filtered = useMemo(() => {
    let out = items;
    if (walletId) {
      const id = Number(walletId);
      out = out.filter((n) => n.wallet_id == null || n.wallet_id === id);
    }
    if (chainFilter) {
      const cid = Number(chainFilter);
      out = out.filter((n) => n.chain_id === cid);
    }
    const q = query.trim().toLowerCase();
    if (q) {
      out = out.filter(
        (n) =>
          (n.name || "").toLowerCase().includes(q) ||
          n.token_id.toLowerCase().includes(q) ||
          n.contract.toLowerCase().includes(q),
      );
    }
    return out;
  }, [items, walletId, chainFilter, query]);

  function explorerTokenUrl(chain: ChainRow | undefined, n: NftItem): string | null {
    if (!chain?.explorer) return null;
    const base = chain.explorer.replace(/\/+$/, "");
    return `${base}/token/${n.contract}?a=${n.token_id}`;
  }

  return (
    <div className="p-6">
      <PageHeader
        suite="Portfolio"
        title="Gallery"
        subtitle="Cached NFT collection from NFT Checker scans"
        action={
          <button
            onClick={() => void load()}
            className="rounded-lg border border-line bg-card px-3 py-2 text-[13px] text-fg hover:border-muted/40"
          >
            Refresh
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
          <option value="">All wallets</option>
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
          value={chainFilter}
          onChange={(e) => setChainFilter(e.target.value)}
          className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
        >
          <option value="">All chains</option>
          {chainsWithItems.map((c) => (
            <option key={c.id} value={c.chain_id}>
              {c.name}
            </option>
          ))}
        </select>
        <label className="flex min-w-48 flex-1 items-center gap-2 rounded-lg border border-line bg-bg px-3 py-2 focus-within:border-accent">
          <Search className="h-3.5 w-3.5 shrink-0 text-muted" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search name / token id / contract…"
            className="w-full bg-transparent text-[13px] outline-none placeholder:text-muted"
          />
          {query ? (
            <button
              onClick={() => setQuery("")}
              className="text-[11px] text-muted hover:text-fg"
            >
              clear
            </button>
          ) : null}
        </label>
      </div>
      {err ? <div className="mb-3 text-[12px] text-danger">{err}</div> : null}
      {filtered.length === 0 ? (
        <div className="rounded-[14px] border border-line bg-card">
          <EmptyState
            title={query || chainFilter ? "No matches" : "Gallery is empty"}
            description={
              query || chainFilter
                ? "Try clearing the search or chain filter."
                : "Run a scan from NFT Checker to populate your local gallery."
            }
          />
        </div>
      ) : (
        <div className="grid grid-cols-2 gap-3 md:grid-cols-4 lg:grid-cols-6">
          {filtered.map((n) => {
            const chain = chainMap.get(n.chain_id);
            const url = n.opensea_url || explorerTokenUrl(chain, n);
            const stale = Date.now() - n.fetched_at > 24 * 60 * 60 * 1000;
            return (
              <div
                key={n.id || `${n.chain_id}-${n.contract}-${n.token_id}`}
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
                {url ? (
                  <button
                    onClick={() => void openUrl(url)}
                    title={n.opensea_url ? "View on OpenSea" : "Open in explorer"}
                    className="absolute right-1.5 top-1.5 rounded-md border border-line bg-bg/90 p-1.5 text-muted opacity-0 transition hover:text-fg group-hover:opacity-100"
                  >
                    <ExternalLink className="h-3.5 w-3.5" />
                  </button>
                ) : null}
                <div className="p-2">
                  <div className="truncate text-[12px] font-medium">{n.name || `#${n.token_id}`}</div>
                  <div className="truncate font-mono text-[10px] text-muted">
                    {shortAddress(n.contract, 4)} · #{n.token_id}
                  </div>
                  <div className="mt-1 flex items-center justify-between text-[10px]">
                    <span className="text-muted">{chain?.name || `chain ${n.chain_id}`}</span>
                    <span className={stale ? "text-warn" : "text-muted"} title={new Date(n.fetched_at).toLocaleString()}>
                      {ageLabel(n.fetched_at)}
                    </span>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}
      <div className="mt-3 text-[12px] text-muted">
        {filtered.length} item(s) shown{filtered.length !== items.length ? ` of ${items.length} cached` : " locally"}
      </div>
    </div>
  );
}
