import { createBrowserRouter, Navigate } from "react-router-dom";
import { AppShell } from "./components/layout/AppShell";
import { DashboardPage } from "./pages/Dashboard";
import { ActivityPage } from "./pages/Activity";
import { WalletsPage } from "./pages/Wallets";
import { MintingPage } from "./pages/Minting";
import { EligibleCheckPage } from "./pages/EligibleCheck";
import { NftCheckerPage } from "./pages/NftChecker";
import { GalleryPage } from "./pages/Gallery";
import { PnlPage } from "./pages/Pnl";
import { ChainsRpcPage } from "./pages/ChainsRpc";
import { ApiSettingsPage } from "./pages/ApiSettings";
import { SettingsPage } from "./pages/Settings";
import { useAppStore } from "./store/app";

function Shell() {
  const profileName = useAppStore((s) => s.profileName);
  return <AppShell profileName={profileName} />;
}

export const router = createBrowserRouter([
  {
    element: <Shell />,
    children: [
      { path: "/", element: <DashboardPage /> },
      { path: "/activity", element: <ActivityPage /> },
      { path: "/wallets", element: <WalletsPage /> },
      { path: "/minting", element: <MintingPage /> },
      { path: "/eligible", element: <EligibleCheckPage /> },
      { path: "/nft-checker", element: <NftCheckerPage /> },
      { path: "/gallery", element: <GalleryPage /> },
      { path: "/pnl", element: <PnlPage /> },
      { path: "/chains", element: <ChainsRpcPage /> },
      { path: "/api-settings", element: <ApiSettingsPage /> },
      { path: "/settings", element: <SettingsPage /> },
      { path: "*", element: <Navigate to="/" replace /> },
    ],
  },
]);
