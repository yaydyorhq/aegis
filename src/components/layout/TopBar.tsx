import { useEffect, useState } from "react";
import { useLocation } from "react-router-dom";
import { Zap } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";

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

export function TopBar({ onQuickTask }: { onQuickTask: () => void }) {
  const { pathname } = useLocation();
  const page = titles[pathname] ?? "Dashboard";
  const win = getCurrentWindow();
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

  /** toggleMaximize + a forced reflow: frameless maximize on Windows can
   *  leave the webview layout at the pre-maximize geometry. */
  async function onToggleMaximize() {
    await win.toggleMaximize();
    requestAnimationFrame(() => {
      document.body.style.width = "100.01%";
      requestAnimationFrame(() => {
        document.body.style.width = "";
      });
    });
  }

  return (
    <header
      className="flex h-[52px] shrink-0 items-center justify-between border-b border-line bg-panel/80 px-5"
      data-tauri-drag-region
    >
      <div className="flex items-center gap-2 font-mono text-[10.5px] uppercase tracking-[0.1em]">
        <span className="text-muted/70">Aegis</span>
        <span className="text-muted/40">/</span>
        <span className="font-medium text-fg">{page}</span>
      </div>
      <div className="flex items-center gap-3 pr-1">
        <span className="font-mono text-[11px] tabular-nums text-muted">{clock}</span>
        <button
          onClick={onQuickTask}
          className="flex items-center gap-1.5 rounded-lg border border-line bg-card px-2.5 py-1.5 text-[12px] text-muted transition-colors hover:border-muted/40 hover:text-fg"
          title="Quick task (Ctrl+K)"
        >
          <Zap className="h-3.5 w-3.5" />
          Quick task
          <kbd className="rounded border border-line px-1 text-[10px]">Ctrl+K</kbd>
        </button>
        <div className="flex items-center gap-1.5">
          <button
            onClick={() => void win.minimize()}
            className="h-3 w-3 rounded-full bg-[#28c840] hover:brightness-110"
            title="Minimize"
            aria-label="Minimize"
          />
          <button
            onClick={() => void onToggleMaximize()}
            className="h-3 w-3 rounded-full bg-[#f5bf4f] hover:brightness-110"
            title="Maximize"
            aria-label="Maximize"
          />
          <button
            onClick={() => void win.close()}
            className="h-3 w-3 rounded-full bg-[#f74c3a] hover:brightness-110"
            title="Close"
            aria-label="Close"
          />
        </div>
      </div>
    </header>
  );
}
