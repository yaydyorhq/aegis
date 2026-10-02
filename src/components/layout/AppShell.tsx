import { useEffect, useRef, useState } from "react";
import { Outlet, useOutletContext } from "react-router-dom";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";
import { VaultGate } from "../../features/vault/VaultGate";
import { QuickTaskPalette } from "../../features/quicktask/QuickTaskPalette";
import { ToastHost, pushToast } from "../ui";
import { startMintTaskFeed } from "../../store/tasks";
import { useVaultStore } from "../../store/app";
import { ipc } from "../../lib/ipc";

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

  // Vault auto-lock: idle N minutes (meta "vault_autolock", 0 = off) with the
  // vault unlocked → lock it. Activity = mouse / keys / touch anywhere.
  const lastActivity = useRef(Date.now());
  useEffect(() => {
    const mark = () => {
      lastActivity.current = Date.now();
    };
    const events: (keyof WindowEventMap)[] = [
      "mousemove",
      "mousedown",
      "keydown",
      "touchstart",
    ];
    events.forEach((e) => window.addEventListener(e, mark, { passive: true }));
    const t = window.setInterval(() => {
      void (async () => {
        try {
          const v = await ipc<string | null>("meta_get", {
            key: "vault_autolock",
          });
          const minutes = v ? Number(v) || 0 : 0;
          if (minutes <= 0) return;
          if (Date.now() - lastActivity.current < minutes * 60_000) return;
          const st = useVaultStore.getState();
          if (st.status?.unlocked) {
            await st.lock();
            pushToast(
              "Vault auto-locked",
              "warn",
              `idle for ${minutes} minute(s) — unlock to sign again`,
            );
          }
        } catch {
          /* ignore — the next tick retries */
        }
      })();
    }, 30_000);
    return () => {
      events.forEach((e) => window.removeEventListener(e, mark));
      window.clearInterval(t);
    };
  }, []);

  // Frameless windows on Windows (WebView2) sometimes leave the layout stale
  // after a maximize/restore/edge-resize — the webview bounds change but the
  // CSS layout keeps the old geometry. Force a reflow pass on every resize.
  useEffect(() => {
    const win = getCurrentWindow();
    let timer = 0;
    const un = win.onResized(() => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        document.body.style.width = "100.01%";
        requestAnimationFrame(() => {
          document.body.style.width = "";
        });
      }, 120);
    });
    return () => {
      void un.then((f) => f());
      window.clearTimeout(timer);
    };
  }, []);

  return (
    <div className="flex h-full w-full overflow-hidden bg-bg">
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
