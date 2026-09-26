import { Outlet } from "react-router-dom";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";

export function AppShell({ profileName }: { profileName: string }) {
  return (
    <div className="flex h-screen w-screen overflow-hidden bg-bg">
      <Sidebar profileName={profileName} />
      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar />
        <main className="min-h-0 flex-1 overflow-y-auto">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
