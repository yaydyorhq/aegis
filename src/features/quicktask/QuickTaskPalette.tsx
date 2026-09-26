import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Command, Flame, BadgeCheck, GalleryVerticalEnd, Wallet, ChartNoAxesCombined, Network } from "lucide-react";

interface Item {
  to: string;
  label: string;
  hint: string;
  icon: typeof Wallet;
}

const ITEMS: Item[] = [
  { to: "/minting", label: "Minting", hint: "Queue & broadcast mint tasks", icon: Flame },
  { to: "/eligible", label: "Eligible Check", hint: "Batch eligibility for wallets", icon: BadgeCheck },
  { to: "/nft-checker", label: "NFT Checker", hint: "Scan ERC-721 holdings", icon: GalleryVerticalEnd },
  { to: "/wallets", label: "Wallets", hint: "Create / import / export", icon: Wallet },
  { to: "/pnl", label: "PnL", hint: "Historical scan estimate", icon: ChartNoAxesCombined },
  { to: "/chains", label: "Chains & RPC", hint: "Add or test endpoints", icon: Network },
];

export function QuickTaskPalette({ open, onClose }: { open: boolean; onClose: () => void }) {
  const nav = useNavigate();
  const [q, setQ] = useState("");
  const [idx, setIdx] = useState(0);

  const filtered = useMemo(() => {
    const s = q.trim().toLowerCase();
    if (!s) return ITEMS;
    return ITEMS.filter(
      (i) => i.label.toLowerCase().includes(s) || i.hint.toLowerCase().includes(s),
    );
  }, [q]);

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
        nav(filtered[idx].to);
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
                key={item.to}
                onMouseEnter={() => setIdx(i)}
                onClick={() => {
                  nav(item.to);
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
