import { useEffect, useState } from "react";
import { NavLink } from "react-router-dom";
import {
  LayoutDashboard,
  Activity,
  Wallet,
  Flame,
  BadgeCheck,
  Image,
  GalleryVerticalEnd,
  ChartNoAxesCombined,
  Network,
  KeyRound,
  Settings,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { StatusDot } from "../ui";
import { useVaultStore } from "../../store/app";

const groups = [
  {
    title: "Overview",
    items: [
      { to: "/", label: "Dashboard", icon: LayoutDashboard, end: true },
      { to: "/activity", label: "Activity", icon: Activity },
    ],
  },
  {
    title: "Operations",
    items: [
      { to: "/wallets", label: "Wallets", icon: Wallet },
      { to: "/minting", label: "Minting", icon: Flame },
      { to: "/eligible", label: "Eligible Check", icon: BadgeCheck },
      { to: "/nft-checker", label: "NFT Checker", icon: GalleryVerticalEnd },
    ],
  },
  {
    title: "Portfolio",
    items: [
      { to: "/gallery", label: "Gallery", icon: Image },
      { to: "/pnl", label: "PnL", icon: ChartNoAxesCombined },
    ],
  },
  {
    title: "Infrastructure",
    items: [
      { to: "/chains", label: "Chains & RPC", icon: Network },
      { to: "/api-settings", label: "API Settings", icon: KeyRound },
      { to: "/settings", label: "Settings", icon: Settings },
    ],
  },
];

export function Sidebar({ profileName }: { profileName: string }) {
  const unlocked = useVaultStore((s) => s.status?.unlocked ?? false);
  const [clock, setClock] = useState(() =>
    new Date().toLocaleTimeString("en-GB", { hour12: false }),
  );

  useEffect(() => {
    const t = setInterval(
      () => setClock(new Date().toLocaleTimeString("en-GB", { hour12: false })),
      1000,
    );
    return () => clearInterval(t);
  }, []);

  return (
    <aside className="flex h-full w-60 shrink-0 flex-col border-r border-line bg-panel">
      <div
        className="flex h-[52px] items-center gap-2.5 border-b border-line px-4"
        data-tauri-drag-region
      >
        <div className="flex h-7 w-7 items-center justify-center rounded-md bg-fg text-[13px] font-black text-bg">
          ∥
        </div>
        <span className="font-mono text-[13px] font-semibold uppercase tracking-[0.18em]">
          Aegis
        </span>
      </div>

      <nav className="flex-1 overflow-y-auto px-2.5 py-3">
        {groups.map((g) => (
          <div key={g.title} className="mb-4">
            <div className="px-2 pb-1.5 font-mono text-[9.5px] font-semibold uppercase tracking-[0.12em] text-muted">
              {g.title}
            </div>
            <div className="space-y-0.5">
              {g.items.map((item) => (
                <NavLink
                  key={item.to}
                  to={item.to}
                  end={item.end}
                  className={({ isActive }) =>
                    cn(
                      "flex items-center gap-2.5 border-l-2 px-[8px] py-[7px] text-[13px] transition-colors",
                      isActive
                        ? "border-accent bg-line/70 text-fg"
                        : "border-transparent text-muted hover:bg-line/40 hover:text-fg",
                    )
                  }
                >
                  <item.icon className="h-[15px] w-[15px]" strokeWidth={1.75} />
                  {item.label}
                </NavLink>
              ))}
            </div>
          </div>
        ))}
      </nav>

      <div className="border-t border-line px-4 py-2">
        <div className="flex items-center justify-between">
          <span className="flex items-center gap-1.5 font-mono text-[9.5px] uppercase tracking-[0.1em] text-muted">
            <StatusDot ok={unlocked} /> Vault {unlocked ? "unlocked" : "locked"}
          </span>
          <span className="font-mono text-[9.5px] tabular-nums text-muted">{clock}</span>
        </div>
      </div>

      <div className="p-2.5 pt-0">
        <NavLink
          to="/settings"
          title="Profile & settings"
          className="flex w-full items-center gap-2.5 rounded-lg border border-line bg-card px-2.5 py-2 text-left transition-colors hover:border-muted/40"
        >
          <div className="flex h-7 w-7 shrink-0 items-center justify-center overflow-hidden rounded-md bg-accent/20 text-[11px] font-semibold text-accent">
            {profileName.slice(0, 2).toUpperCase()}
          </div>
          <div className="min-w-0 flex-1">
            <div className="truncate text-[13px] font-medium">{profileName}</div>
          </div>
          <svg className="h-3.5 w-3.5 text-muted" viewBox="0 0 16 16" fill="none">
            <path d="M4 6l4 4 4-4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          </svg>
        </NavLink>
      </div>
    </aside>
  );
}
