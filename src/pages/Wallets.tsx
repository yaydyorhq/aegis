import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";
import {
  ArrowLeftRight,
  Copy,
  FolderPlus,
  KeyRound,
  Layers,
  Pencil,
  Plus,
  RefreshCw,
  Trash2,
  Upload,
} from "lucide-react";
import { ipc } from "../lib/ipc";
import type {
  BulkImportItem,
  BulkImportResultItem,
  PortfolioLive,
} from "../lib/types";
import { cn, formatEth, shortAddress } from "../lib/utils";
import { EmptyState, PageHeader, pushToast, ConfirmDialog } from "../components/ui";
import { useVaultStore, useWalletStore } from "../store/app";
import { ManageFundsModal } from "../features/funds/ManageFundsModal";

type FormMode = "none" | "generate" | "import" | "bulk";

interface ParsedLine {
  label: string | null;
  private_key: string;
}

function parseBulkInput(raw: string): ParsedLine[] {
  return raw
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean)
    .map((line) => {
      // Formats: "key", "label,key", "label key", "label=0xkey"
      const byComma = line.split(",");
      if (byComma.length === 2) {
        const [label, key] = byComma.map((s) => s.trim());
        return { label: label || null, private_key: key };
      }
      const byEq = line.split("=");
      if (byEq.length === 2) {
        const [label, key] = byEq.map((s) => s.trim());
        return { label: label || null, private_key: key };
      }
      const bySpace = line.split(/\s+/);
      if (bySpace.length === 2 && bySpace[1].replace(/^0x/i, "").match(/^[0-9a-fA-F]{64}$/)) {
        return { label: bySpace[0], private_key: bySpace[1] };
      }
      return { label: null, private_key: line };
    });
}

export function WalletsPage() {
  const unlocked = useVaultStore((s) => s.status?.unlocked ?? false);
  const refreshVault = useVaultStore((s) => s.refresh);
  const { wallets, groups, load: loadStore, refresh: refreshStore } = useWalletStore();

  const [loading, setLoading] = useState(false);
  const [mode, setMode] = useState<FormMode>("none");
  const [label, setLabel] = useState("");
  const [pk, setPk] = useState("");
  const [bulkText, setBulkText] = useState("");
  const [bulkResults, setBulkResults] = useState<BulkImportResultItem[] | null>(null);
  const [formGroupId, setFormGroupId] = useState<number | null>(null);
  const [filterGroupId, setFilterGroupId] = useState<number | "all" | "ungrouped">("all");
  const [search, setSearch] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [showFunds, setShowFunds] = useState(false);
  /** Live native balances across every enabled chain (refreshable). */
  const [live, setLive] = useState<PortfolioLive | null>(null);
  const [liveLoading, setLiveLoading] = useState(false);
  /** Which chain the Balance column shows; null = first in response. */
  const [liveChainId, setLiveChainId] = useState<number | null>(null);

  const loadLive = useCallback(async () => {
    setLiveLoading(true);
    try {
      setLive(await ipc<PortfolioLive>("portfolio_live"));
    } catch {
      setLive(null);
    } finally {
      setLiveLoading(false);
    }
  }, []);

  // inline label edit
  const [editId, setEditId] = useState<number | null>(null);
  const [editVal, setEditVal] = useState("");

  // group management
  const [showGroupMgr, setShowGroupMgr] = useState(false);
  const [newGroupName, setNewGroupName] = useState("");
  const [renameId, setRenameId] = useState<number | null>(null);
  const [renameVal, setRenameVal] = useState("");

  /** Destructive/secret action awaiting in-app confirmation (replaces the
   *  webview's confirm()/prompt() — those are unstyled and, for prompt(),
   *  show the passphrase unmasked). */
  type Pending =
    | { kind: "delete-wallet"; id: number; label: string; address: string }
    | { kind: "delete-group"; id: number; name: string }
    | { kind: "export"; id: number; label: string };
  const [pending, setPending] = useState<Pending | null>(null);
  const [pendingBusy, setPendingBusy] = useState(false);

  const load = useCallback(async () => {
    await loadStore();
  }, [loadStore]);

  useEffect(() => {
    if (unlocked) {
      void load();
      void loadLive();
    }
  }, [unlocked, load, loadLive]);

  const selectedLiveChain =
    live?.chains.find((c) => c.chain_id === liveChainId) ?? live?.chains[0] ?? null;
  const balanceById = useMemo(
    () =>
      new Map(
        (selectedLiveChain?.balances ?? []).map((b) => [b.wallet_id, b]),
      ),
    [selectedLiveChain],
  );

  const groupNameById = useMemo(() => {
    const m = new Map<number, string>();
    for (const g of groups) m.set(g.id, g.name);
    return m;
  }, [groups]);

  const filteredWallets = useMemo(() => {
    let list = wallets;
    if (filterGroupId === "ungrouped") list = list.filter((w) => w.group_id == null);
    else if (filterGroupId !== "all") list = list.filter((w) => w.group_id === filterGroupId);
    const q = search.trim().toLowerCase();
    if (q) {
      list = list.filter(
        (w) =>
          w.label.toLowerCase().includes(q) ||
          w.address.toLowerCase().includes(q),
      );
    }
    return list;
  }, [wallets, filterGroupId, search]);

  const bulkParsed = useMemo(() => parseBulkInput(bulkText), [bulkText]);

  function closeForm() {
    setMode("none");
    setLabel("");
    setPk("");
    setBulkText("");
    setBulkResults(null);
    setFormGroupId(null);
    setErr(null);
  }

  // ── generate / single import ──────────────────────────────────────
  async function onGenerate(e: FormEvent) {
    e.preventDefault();
    setLoading(true);
    setErr(null);
    try {
      if (formGroupId != null) {
        await ipc("wallet_generate_in_group", { label, groupId: formGroupId });
      } else {
        await ipc("wallet_generate", { label });
      }
      closeForm();
      await load();
      await refreshVault();
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function onImport(e: FormEvent) {
    e.preventDefault();
    setLoading(true);
    setErr(null);
    try {
      if (formGroupId != null) {
        await ipc("wallet_import_in_group", {
          label,
          privateKey: pk,
          groupId: formGroupId,
        });
      } else {
        await ipc("wallet_import", { label, privateKey: pk });
      }
      closeForm();
      await load();
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  }

  // ── bulk import ───────────────────────────────────────────────────
  async function onBulkImport(e: FormEvent) {
    e.preventDefault();
    if (bulkParsed.length === 0) return;
    setLoading(true);
    setErr(null);
    setBulkResults(null);
    try {
      const items: BulkImportItem[] = bulkParsed.map((p) => ({
        label: p.label,
        private_key: p.private_key,
      }));
      const res = await ipc<BulkImportResultItem[]>("wallet_import_bulk", {
        items,
        groupId: formGroupId,
      });
      setBulkResults(res);
      setBulkText("");
      await load();
      await refreshVault();
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function onFileUpload(file: File) {
    try {
      const text = await file.text();
      setBulkText((prev) => (prev.trim() ? `${prev.trimEnd()}\n${text}` : text));
      setBulkResults(null);
    } catch {
      setErr("Failed to read file");
    }
  }

  // ── row actions ───────────────────────────────────────────────────
  async function onDeleteWallet(id: number) {
    setPendingBusy(true);
    try {
      await ipc("wallet_delete", { id });
      setPending(null);
      await load();
    } catch (e) {
      setErr(String(e));
    } finally {
      setPendingBusy(false);
    }
  }

  async function onExportWithPass(id: number, pass: string) {
    setPendingBusy(true);
    try {
      const key = await ipc<string>("wallet_export", { id, passConfirm: pass });
      await navigator.clipboard.writeText(key);
      setPending(null);
      pushToast(
        "Private key copied",
        "warn",
        `Wallet #${id} — paste it somewhere safe now; it is on your clipboard`,
      );
    } catch (e) {
      setErr(String(e));
    } finally {
      setPendingBusy(false);
    }
  }

  async function onCopy(addr: string) {
    try {
      await navigator.clipboard.writeText(addr);
      setCopied(addr);
      setTimeout(() => setCopied(null), 1500);
    } catch (e) {
      setErr(String(e));
    }
  }

  async function onSetGroup(walletId: number, gid: number | null) {
    try {
      await ipc("wallet_set_group", { walletId, groupId: gid });
      await load();
      await refreshStore();
    } catch (e) {
      setErr(String(e));
    }
  }

  async function onRenameWallet(id: number) {
    const v = editVal.trim();
    if (!v) return;
    try {
      await ipc("wallet_rename", { id, label: v });
      setEditId(null);
      setEditVal("");
      await load();
    } catch (e) {
      setErr(String(e));
    }
  }

  // ── group management ──────────────────────────────────────────────
  async function onCreateGroup(e: FormEvent) {
    e.preventDefault();
    if (!newGroupName.trim()) return;
    try {
      const g = await ipc<{ id: number }>("group_create", { name: newGroupName.trim() });
      setNewGroupName("");
      setFilterGroupId(g.id);
      await load();
    } catch (e) {
      setErr(String(e));
    }
  }

  async function onRenameGroup(id: number) {
    if (!renameVal.trim()) return;
    try {
      await ipc("group_rename", { id, name: renameVal.trim() });
      setRenameId(null);
      setRenameVal("");
      await load();
    } catch (e) {
      setErr(String(e));
    }
  }

  async function onDeleteGroup(id: number) {
    setPendingBusy(true);
    try {
      await ipc("group_delete", { id });
      if (filterGroupId === id) setFilterGroupId("all");
      setPending(null);
      await load();
    } catch (e) {
      setErr(String(e));
    } finally {
      setPendingBusy(false);
    }
  }

  const groupSelect = (
    <select
      value={formGroupId ?? ""}
      onChange={(e) => setFormGroupId(e.target.value === "" ? null : Number(e.target.value))}
      className="rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
    >
      <option value="">No group</option>
      {groups.map((g) => (
        <option key={g.id} value={g.id}>
          {g.name}
        </option>
      ))}
    </select>
  );

  const bulkStatusColor = (s: string) =>
    s === "imported" ? "text-ok" : s === "duplicate" ? "text-warn" : "text-danger";

  return (
    <div className="p-6">
      <PageHeader
        suite="Operations"
        title="Wallets"
        subtitle="Local encrypted wallet vault — keys never leave this device"
        action={
          <div className="flex gap-2">
            <button
              onClick={() => {
                setMode(mode === "generate" ? "none" : "generate");
                setErr(null);
                setBulkResults(null);
              }}
              className="flex items-center gap-1.5 rounded-lg bg-fg px-3 py-2 text-[13px] font-semibold text-bg hover:opacity-90"
            >
              <Plus className="h-4 w-4" /> Generate
            </button>
            <button
              onClick={() => {
                setMode(mode === "import" ? "none" : "import");
                setErr(null);
                setBulkResults(null);
              }}
              className="flex items-center gap-1.5 rounded-lg border border-line bg-card px-3 py-2 text-[13px] text-fg hover:border-muted/40"
            >
              <KeyRound className="h-4 w-4" /> Import
            </button>
            <button
              onClick={() => {
                setMode(mode === "bulk" ? "none" : "bulk");
                setErr(null);
                setBulkResults(null);
              }}
              className={`flex items-center gap-1.5 rounded-lg border px-3 py-2 text-[13px] ${
                mode === "bulk"
                  ? "border-accent bg-accent/10 text-accent"
                  : "border-line bg-card text-fg hover:border-muted/40"
              }`}
            >
              <Layers className="h-4 w-4" /> Bulk
            </button>
            <button
              onClick={() => {
                if (!unlocked) {
                  pushToast("Vault locked", "warn", "Unlock the vault to move funds");
                  return;
                }
                setShowFunds((v) => !v);
                setErr(null);
                setBulkResults(null);
              }}
              disabled={!unlocked}
              className="flex items-center gap-1.5 rounded-lg border border-line bg-card px-3 py-2 text-[13px] text-fg hover:border-muted/40 disabled:opacity-40"
              title="Disperse / consolidate funds across wallets"
            >
              <ArrowLeftRight className="h-4 w-4" /> Manage Funds
            </button>
          </div>
        }
      />

      {/* ── Generate form ── */}
      {mode === "generate" ? (
        <form
          onSubmit={onGenerate}
          className="mb-4 flex gap-2 rounded-[14px] border border-line bg-card p-4"
        >
          <input
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder="Wallet label (e.g. main-01)"
            className="flex-1 rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          />
          {groupSelect}
          <button
            type="submit"
            disabled={loading || !label.trim()}
            className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
          >
            Create
          </button>
        </form>
      ) : null}

      {/* ── Single import form ── */}
      {mode === "import" ? (
        <form
          onSubmit={onImport}
          className="mb-4 space-y-2 rounded-[14px] border border-line bg-card p-4"
        >
          <input
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder="Wallet label"
            className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
          />
          <div className="flex gap-2">
            <input
              value={pk}
              onChange={(e) => setPk(e.target.value)}
              placeholder="0x private key"
              type="password"
              className="flex-1 rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
            />
            {groupSelect}
            <button
              type="submit"
              disabled={loading || !label.trim() || !pk.trim()}
              className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
            >
              Import
            </button>
          </div>
        </form>
      ) : null}

      {/* ── Bulk import form ── */}
      {mode === "bulk" ? (
        <form
          onSubmit={onBulkImport}
          className="mb-4 space-y-3 rounded-[14px] border border-line bg-card p-4"
        >
          <div className="flex items-center justify-between">
            <div className="text-[13px] font-medium text-fg">Bulk import private keys</div>
            <label className="flex cursor-pointer items-center gap-1.5 rounded-lg border border-line bg-bg px-3 py-1.5 text-[12px] text-muted hover:text-fg">
              <Upload className="h-3.5 w-3.5" /> Upload .txt
              <input
                type="file"
                accept=".txt,.csv,text/plain"
                className="hidden"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) void onFileUpload(f);
                  e.target.value = "";
                }}
              />
            </label>
          </div>
          <textarea
            value={bulkText}
            onChange={(e) => {
              setBulkText(e.target.value);
              setBulkResults(null);
            }}
            rows={8}
            spellCheck={false}
            placeholder={
              "One key per line. Optional label formats:\n" +
              "0xabc...def\n" +
              "my-wallet,0xabc...def\n" +
              "my-wallet 0xabc...def"
            }
            className="w-full resize-y rounded-lg border border-line bg-bg px-3 py-2 font-mono text-[12px] leading-relaxed outline-none focus:border-accent"
          />
          <div className="flex items-center gap-3">
            <span className="text-[12px] text-muted">
              {bulkParsed.length} key{bulkParsed.length === 1 ? "" : "s"} parsed
            </span>
            <div className="flex-1" />
            {groupSelect}
            <button
              type="submit"
              disabled={loading || bulkParsed.length === 0}
              className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
            >
              {loading ? "Importing…" : `Import ${bulkParsed.length || ""}`}
            </button>
          </div>

          {/* bulk results */}
          {bulkResults ? (
            <div className="max-h-48 overflow-y-auto rounded-lg border border-line bg-bg p-3">
              <div className="mb-2 text-[12px] text-muted">
                {bulkResults.filter((r) => r.status === "imported").length} imported ·{" "}
                {bulkResults.filter((r) => r.status === "duplicate").length} duplicate ·{" "}
                {bulkResults.filter((r) => r.status === "invalid").length} invalid
              </div>
              <ul className="space-y-1">
                {bulkResults.map((r) => (
                  <li key={r.index} className="flex items-start gap-2 text-[12px]">
                    <span className={`w-16 shrink-0 font-medium ${bulkStatusColor(r.status)}`}>
                      {r.status}
                    </span>
                    <span className="font-mono text-muted">
                      {r.address
                        ? shortAddress(r.address, 6)
                        : r.error ?? `line ${r.index + 1}`}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
        </form>
      ) : null}

      {err ? <div className="mb-3 text-[12px] text-danger">{err}</div> : null}

      {/* ── Group filter bar ── */}
      <div className="mb-4 flex flex-wrap items-center gap-2">
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search label or address…"
          className="w-56 rounded-lg border border-line bg-card px-3 py-1.5 text-[12px] text-fg outline-none placeholder:text-muted focus:border-accent"
        />
        <button
          onClick={() => setFilterGroupId("all")}
          className={`rounded-full border px-3 py-1 text-[12px] ${
            filterGroupId === "all"
              ? "border-accent bg-accent/10 text-accent"
              : "border-line bg-card text-muted hover:text-fg"
          }`}
        >
          All ({wallets.length})
        </button>
        <button
          onClick={() => setFilterGroupId("ungrouped")}
          className={`rounded-full border px-3 py-1 text-[12px] ${
            filterGroupId === "ungrouped"
              ? "border-accent bg-accent/10 text-accent"
              : "border-line bg-card text-muted hover:text-fg"
          }`}
        >
          Ungrouped ({wallets.filter((w) => w.group_id == null).length})
        </button>
        {groups.map((g) => (
          <button
            key={g.id}
            onClick={() => setFilterGroupId(g.id)}
            className={`rounded-full border px-3 py-1 text-[12px] ${
              filterGroupId === g.id
                ? "border-accent bg-accent/10 text-accent"
                : "border-line bg-card text-muted hover:text-fg"
            }`}
          >
            {g.name} ({g.wallet_count})
          </button>
        ))}
        <button
          onClick={() => setShowGroupMgr((v) => !v)}
          title="Manage groups"
          className="flex items-center gap-1 rounded-full border border-line bg-card px-3 py-1 text-[12px] text-muted hover:border-muted/40 hover:text-fg"
        >
          <FolderPlus className="h-3.5 w-3.5" /> Groups
        </button>
      </div>

      {/* ── Group manager ── */}
      {showGroupMgr ? (
        <div className="mb-4 rounded-[14px] border border-line bg-card p-4">
          <div className="mb-3 text-[13px] font-medium text-fg">Manage groups</div>
          <form onSubmit={onCreateGroup} className="mb-3 flex gap-2">
            <input
              value={newGroupName}
              onChange={(e) => setNewGroupName(e.target.value)}
              placeholder="New group name"
              maxLength={64}
              className="flex-1 rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
            />
            <button
              type="submit"
              disabled={!newGroupName.trim()}
              className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white disabled:opacity-40"
            >
              Create
            </button>
          </form>
          {groups.length === 0 ? (
            <div className="text-[12px] text-muted">No groups yet.</div>
          ) : (
            <ul className="space-y-1.5">
              {groups.map((g) => (
                <li key={g.id} className="flex items-center gap-2">
                  {renameId === g.id ? (
                    <>
                      <input
                        value={renameVal}
                        onChange={(e) => setRenameVal(e.target.value)}
                        maxLength={64}
                        autoFocus
                        className="flex-1 rounded-lg border border-line bg-bg px-3 py-1.5 text-[13px] outline-none focus:border-accent"
                        onKeyDown={(e) => {
                          if (e.key === "Enter") {
                            e.preventDefault();
                            void onRenameGroup(g.id);
                          }
                          if (e.key === "Escape") setRenameId(null);
                        }}
                      />
                      <button
                        onClick={() => void onRenameGroup(g.id)}
                        className="rounded-md bg-accent px-2.5 py-1.5 text-[12px] font-semibold text-white"
                      >
                        Save
                      </button>
                      <button
                        onClick={() => setRenameId(null)}
                        className="rounded-md px-2 py-1.5 text-[12px] text-muted hover:text-fg"
                      >
                        Cancel
                      </button>
                    </>
                  ) : (
                    <>
                      <span className="flex-1 text-[13px] text-fg">{g.name}</span>
                      <span className="text-[12px] text-muted">{g.wallet_count} wallets</span>
                      <button
                        onClick={() => {
                          setRenameId(g.id);
                          setRenameVal(g.name);
                        }}
                        title="Rename"
                        className="rounded-md p-1.5 text-muted hover:bg-line hover:text-fg"
                      >
                        <Pencil className="h-3.5 w-3.5" />
                      </button>
                      <button
                        onClick={() =>
                          setPending({
                            kind: "delete-group",
                            id: g.id,
                            name: g.name,
                          })
                        }
                        title="Delete group"
                        className="rounded-md p-1.5 text-muted hover:bg-line hover:text-danger"
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </button>
                    </>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}

      {/* ── Wallet table ── */}
      <div className="overflow-hidden rounded-[14px] border border-line bg-card">
        <div className="grid grid-cols-[64px_1.1fr_1.2fr_130px_110px_130px_136px] items-center gap-2 border-b border-line px-4 py-2 font-mono text-[9.5px] font-semibold uppercase tracking-[0.1em] text-muted">
          <div>ID</div>
          <div>Label</div>
          <div>Address</div>
          <div className="flex items-center justify-end gap-1.5 text-right">
            <button
              onClick={() => void loadLive()}
              title="Refresh balances"
              className="rounded p-0.5 hover:text-fg"
            >
              <RefreshCw className={cn("h-3 w-3", liveLoading && "animate-spin")} />
            </button>
            <span>Balance</span>
            {live && live.chains.length > 0 ? (
              <select
                value={String(selectedLiveChain?.chain_id ?? "")}
                onChange={(e) => setLiveChainId(Number(e.target.value))}
                title="Chain for the balance column"
                className="max-w-[110px] rounded border border-line bg-bg px-1 py-0.5 text-[10px] normal-case text-muted outline-none hover:text-fg focus:border-accent"
              >
                {live.chains.map((c) => (
                  <option key={c.chain_id} value={c.chain_id}>
                    {c.chain_name}
                  </option>
                ))}
              </select>
            ) : null}
          </div>
          <div>Group</div>
          <div className="text-right">Move to</div>
          <div className="text-right">Actions</div>
        </div>
        {filteredWallets.length === 0 ? (
          <EmptyState
            title={wallets.length === 0 ? "No wallets yet" : "No wallets in this filter"}
            description={
              wallets.length === 0
                ? "Generate a new keypair or import existing private keys. Everything is encrypted locally."
                : "Try a different group filter or clear the filter."
            }
          />
        ) : (
          filteredWallets.map((w) => {
            const b = balanceById.get(w.id);
            return (
            <div
              key={w.id}
              className="grid grid-cols-[64px_1.1fr_1.2fr_130px_110px_130px_136px] items-center gap-2 border-b border-line/60 px-4 py-2.5 text-[13px] last:border-0 hover:bg-line/30"
            >
              <div className="text-muted">#{w.id}</div>
              <div className="min-w-0">
                {editId === w.id ? (
                  <div className="flex items-center gap-1">
                    <input
                      value={editVal}
                      onChange={(e) => setEditVal(e.target.value)}
                      maxLength={128}
                      autoFocus
                      className="w-full min-w-0 rounded-md border border-line bg-bg px-2 py-1 text-[13px] outline-none focus:border-accent"
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          void onRenameWallet(w.id);
                        }
                        if (e.key === "Escape") setEditId(null);
                      }}
                    />
                    <button
                      onClick={() => void onRenameWallet(w.id)}
                      className="shrink-0 rounded-md bg-accent px-2 py-1 text-[11px] font-semibold text-white"
                    >
                      Save
                    </button>
                    <button
                      onClick={() => setEditId(null)}
                      className="shrink-0 rounded-md px-1.5 py-1 text-[11px] text-muted hover:text-fg"
                    >
                      ✕
                    </button>
                  </div>
                ) : (
                  <button
                    onClick={() => {
                      setEditId(w.id);
                      setEditVal(w.label);
                    }}
                    title="Click to rename"
                    className="group flex w-full min-w-0 items-center gap-1 truncate text-left font-medium hover:text-accent"
                  >
                    <span className="truncate">{w.label}</span>
                    <Pencil className="h-3 w-3 shrink-0 opacity-0 transition group-hover:opacity-60" />
                  </button>
                )}
              </div>
              <button
                onClick={() => onCopy(w.address)}
                className="flex items-center gap-1.5 font-mono text-[12px] text-muted hover:text-fg"
                title={w.address}
              >
                {copied === w.address ? "Copied!" : shortAddress(w.address, 6)}
                <Copy className="h-3.5 w-3.5" />
              </button>
              <div
                className="text-right font-mono text-[12px] tabular-nums"
                title={
                  b?.balance_eth != null
                    ? `${b.balance_eth} ${selectedLiveChain?.native_symbol ?? ""}`
                    : undefined
                }
              >
                {b?.balance_eth != null
                  ? `${formatEth(b.balance_eth)} ${selectedLiveChain?.native_symbol ?? ""}`
                  : "—"}
              </div>
              <div>
                {w.group_id != null ? (
                  <span className="inline-block max-w-full truncate rounded-full border border-accent/30 bg-accent/10 px-2 py-0.5 text-[11px] text-accent">
                    {groupNameById.get(w.group_id) ?? "—"}
                  </span>
                ) : (
                  <span className="text-[12px] text-muted">—</span>
                )}
              </div>
              <div className="flex justify-end">
                <select
                  value={w.group_id ?? ""}
                  onChange={(e) =>
                    void onSetGroup(w.id, e.target.value === "" ? null : Number(e.target.value))
                  }
                  className="max-w-[130px] rounded-md border border-line bg-bg px-2 py-1 text-[12px] text-muted outline-none hover:text-fg focus:border-accent"
                >
                  <option value="">Ungrouped</option>
                  {groups.map((g) => (
                    <option key={g.id} value={g.id}>
                      {g.name}
                    </option>
                  ))}
                </select>
              </div>
              <div className="flex justify-end gap-1">
                <button
                  onClick={() => {
                    setEditId(w.id);
                    setEditVal(w.label);
                  }}
                  title="Rename label"
                  className="rounded-md p-1.5 text-muted hover:bg-line hover:text-fg"
                >
                  <Pencil className="h-4 w-4" />
                </button>
                <button
                  onClick={() =>
                    setPending({
                      kind: "export",
                      id: w.id,
                      label: w.label,
                    })
                  }
                  title="Export key"
                  className="rounded-md p-1.5 text-muted hover:bg-line hover:text-fg"
                >
                  <KeyRound className="h-4 w-4" />
                </button>
                <button
                  onClick={() =>
                    setPending({
                      kind: "delete-wallet",
                      id: w.id,
                      label: w.label,
                      address: w.address,
                    })
                  }
                  title="Delete"
                  className="rounded-md p-1.5 text-muted hover:bg-line hover:text-danger"
                >
                  <Trash2 className="h-4 w-4" />
                </button>
              </div>
            </div>
            );
          })
        )}
      </div>

      <div className="mt-3 font-mono text-[9.5px] uppercase tracking-[0.1em] text-muted">
        {wallets.length} STORED LOCALLY
        {filterGroupId !== "all" || search.trim()
          ? ` · showing ${filteredWallets.length}`
          : ""}
        {!unlocked ? " — unlock vault to manage" : ""}
      </div>

      {showFunds ? <ManageFundsModal onClose={() => setShowFunds(false)} /> : null}

      <ConfirmDialog
        open={pending?.kind === "delete-wallet"}
        title={pending?.kind === "delete-wallet" ? `Delete wallet #${pending.id} "${pending.label}"?` : ""}
        body={
          pending?.kind === "delete-wallet" ? (
            <>
              <span className="font-mono">{pending.address}</span>
              <br />
              This permanently destroys the encrypted private key. Make sure you
              exported it first.
            </>
          ) : null
        }
        confirmLabel="Delete wallet"
        cancelLabel="Keep"
        danger
        busy={pendingBusy}
        onConfirm={() => pending?.kind === "delete-wallet" && void onDeleteWallet(pending.id)}
        onClose={() => setPending(null)}
      />

      <ConfirmDialog
        open={pending?.kind === "delete-group"}
        title={pending?.kind === "delete-group" ? `Delete group "${pending.name}"?` : ""}
        body="Wallets in this group will be ungrouped (not deleted)."
        confirmLabel="Delete group"
        danger
        busy={pendingBusy}
        onConfirm={() => pending?.kind === "delete-group" && void onDeleteGroup(pending.id)}
        onClose={() => setPending(null)}
      />

      <ConfirmDialog
        open={pending?.kind === "export"}
        title="Export private key"
        body={
          pending?.kind === "export" ? (
            <>
              Wallet #{pending.id} “{pending.label}” — the key will be copied to
              your clipboard. Confirm your vault passphrase to decrypt it.
            </>
          ) : null
        }
        confirmLabel="Unlock & copy"
        cancelLabel="Cancel"
        passwordLabel="Vault passphrase"
        busy={pendingBusy}
        onConfirm={(pass) => pending?.kind === "export" && void onExportWithPass(pending.id, pass)}
        onClose={() => setPending(null)}
      />
    </div>
  );
}
