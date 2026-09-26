import { useLocation } from "react-router-dom";

const titles: Record<string, string> = {
  "/": "Dashboard",
  "/activity": "Activity",
  "/wallets": "Wallets",
  "/minting": "Minting",
  "/eligible": "Eligible Check",
  "/nft-checker": "NFT Checker",
  "/gallery": "Gallery",
  "/pnl": "PnL",
  "/chains": "Chains & RPC",
  "/api-settings": "API Settings",
  "/settings": "Settings",
};

export function TopBar() {
  const { pathname } = useLocation();
  const page = titles[pathname] ?? "Dashboard";

  return (
    <header
      className="flex h-[52px] shrink-0 items-center justify-between border-b border-line bg-panel/80 px-5"
      data-tauri-drag-region
    >
      <div className="flex items-center gap-2 text-[13px]">
        <span className="text-muted">Aevora</span>
        <span className="text-muted/50">/</span>
        <span className="font-medium text-fg">{page}</span>
      </div>
      <div className="flex items-center gap-1.5 pr-1">
        <span className="h-3 w-3 rounded-full bg-[#28c840]" />
        <span className="h-3 w-3 rounded-full bg-[#f5bf4f]" />
        <span className="h-3 w-3 rounded-full bg-[#f74c3a]" />
      </div>
    </header>
  );
}
