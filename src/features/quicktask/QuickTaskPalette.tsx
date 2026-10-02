import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Command, Flame, BadgeCheck, GalleryVerticalEnd, Wallet, ChartNoAxesCombined, Network, Play, Lock, RefreshCw } from "lucide-react";
import { ipc } from "../../lib/ipc";
import { pushToast } from "../../components/ui";
import { useVaultStore } from "../../store/app";

type IconType = typeof Wallet;

interface Item {
  key: string;
  label: string;
  hint: string;
  icon: IconType;
  /** Navigation target — mutually exclusive with `action`. */
  to?: string;
  /** In-place action (queue, vault, rescans) — mutually exclusive with `to`. */
  action?: () => void;
}

export function QuickTaskPalette({ open, onClose }: { open: boolean; onClose: () => void }) {
  const nav = useNavigate();
  const [q, setQ] = useState("");
  const [idx, setIdx] = useState(0);

  // Actions alongside routes: the palette became the keyboard entry point for
  // the app's most-repeated operations, not just a page switcher.
  const items = useMemo<Item[]>(
    () => [
      { key: "nav-minting", label: "Minting", hint: "Queue & broadcast mint tasks", icon: Flame, to: "/minting" },
      { key: "nav-eligible", label: "Eligible Check", hint: "Batch eligibility for wallets", icon: BadgeCheck, to: "/eligible" },
      { key: "nav-nft", label: "NFT Checker", hint: "Scan ERC-721 holdings", icon: GalleryVerticalEnd, to: "/nft-checker" },
      { key: "nav-wallets", label: "Wallets", hint: "Create / import / export", icon: Wallet, to: "/wallets" },
      { key: "nav-pnl", label: "PnL", hint: "Historical scan estimate", icon: ChartNoAxesCombined, to: "/pnl" },
      { key: "nav-chains", label: "Chains & RPC", hint: "Add or test endpoints", icon: Network, to: "/chains" },
      {
        key: "act-run-queue",
        label: "Run mint queue",
        hint: "action — process pending tasks now",
        icon: Play,
        action: () => {
          pushToast("Running mint queue…", "info");
          void ipc("mint_run").catch(() => pushToast("Queue run failed", "error"));
        },
      },
      {
        key: "act-rescan",
        label: "Re-scan portfolio",
        hint: "action — repeat last Collection PnL scan",
        icon: RefreshCw,
        action: () => {
          pushToast("Portfolio re-scan started", "info");
          void ipc("collection_pnl_rescan")
            .then((r) =>
              pushToast(
                r ? "Portfolio re-scanned" : "Nothing to re-scan yet",
                "ok",
                r ? "result persisted to the dashboard" : "run a Collection PnL scan first",
              ),
            )
            .catch(() => pushToast("Portfolio re-scan failed", "error"));
        },
      },
      {
        key: "act-lock",
        label: "Lock vault",
        hint: "action — private keys out of memory",
        icon: Lock,
        action: () => {
          void useVaultStore
            .getState()
            .lock()
            .then(() => pushToast("Vault locked", "warn", "unlock to sign again"))
            .catch(() => pushToast("Could not lock vault", "error"));
        },
      },
    ],
    [],
  );

  const filtered = useMemo(() => {
    const s = q.trim().toLowerCase();
    if (!s) return items;
    return items.filter(
      (i) => i.label.toLowerCase().includes(s) || i.hint.toLowerCase().includes(s),
    );
  }, [q, items]);

  useEffect(() => {
    if (open) {
      setQ("");
      setIdx(0);
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setIdx((i) => Math.min(i + 1, filtered.length - 1));
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setIdx((i) => Math.max(i - 1, 0));
      }
      if (e.key === "Enter" && filtered[idx]) {
        const item = filtered[idx];
        if (item.to) nav(item.to);
        else item.action?.();
        onClose();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, filtered, idx, nav, onClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-bg/70 pt-[12vh] backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="w-[440px] overflow-hidden rounded-2xl border border-line bg-card shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-2 border-b border-line px-4 py-3">
          <Command className="h-4 w-4 text-muted" />
          <input
            autoFocus
            value={q}
            onChange={(e) => {
              setQ(e.target.value);
              setIdx(0);
            }}
            placeholder="Quick task…"
            className="flex-1 bg-transparent text-[14px] text-fg outline-none placeholder:text-muted"
          />
          <kbd className="rounded border border-line px-1.5 py-0.5 text-[10px] text-muted">Esc</kbd>
        </div>
        <div className="max-h-[300px] overflow-y-auto py-1">
          {filtered.length === 0 ? (
            <div className="px-4 py-6 text-center text-[13px] text-muted">No matches</div>
          ) : (
            filtered.map((item, i) => (
              <button
                key={item.key}
                onMouseEnter={() => setIdx(i)}
                onClick={() => {
                  if (item.to) nav(item.to);
                  else item.action?.();
                  onClose();
                }}
                className={`flex w-full items-center gap-3 px-4 py-2.5 text-left ${
                  i === idx ? "bg-line/70" : ""
                }`}
              >
                <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-line text-accent">
                  <item.icon className="h-4 w-4" />
                </div>
                <div className="min-w-0 flex-1">
                  <div className="text-[13px] font-medium text-fg">{item.label}</div>
                  <div className="truncate text-[11px] text-muted">{item.hint}</div>
                </div>
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
