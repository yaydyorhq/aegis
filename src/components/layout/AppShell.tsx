import { useEffect, useState } from "react";
import { Outlet, useOutletContext } from "react-router-dom";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";
import { VaultGate } from "../../features/vault/VaultGate";
import { QuickTaskPalette } from "../../features/quicktask/QuickTaskPalette";
import { ToastHost } from "../ui";
import { startMintTaskFeed } from "../../store/tasks";

interface ShellCtx {
  openQuickTask: () => void;
}

export function useShell() {
  return useOutletContext<ShellCtx>();
}

export function AppShell({ profileName }: { profileName: string }) {
  const [paletteOpen, setPaletteOpen] = useState(false);
  const openQuickTask = () => setPaletteOpen(true);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen((v) => !v);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // One global task poll drives status toasts everywhere.
  useEffect(() => {
    startMintTaskFeed();
  }, []);

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-bg">
      <VaultGate />
      <Sidebar profileName={profileName} />
      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar onQuickTask={openQuickTask} />
        <main className="min-h-0 flex-1 overflow-y-auto">
          <Outlet context={{ openQuickTask } satisfies ShellCtx} />
        </main>
      </div>
      <QuickTaskPalette open={paletteOpen} onClose={() => setPaletteOpen(false)} />
      <ToastHost />
    </div>
  );
}
