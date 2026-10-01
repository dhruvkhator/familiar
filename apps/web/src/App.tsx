import { useEffect, useState } from "react";
import { HashRouter, Navigate, Route, Routes, useNavigate } from "react-router-dom";
import { isConfigured, isTauri, useWaking } from "./lib/api";
import { ToastProvider } from "./lib/toast";
import { DesktopSetup, FirstTeammate } from "./pages/Onboarding";
import { SessionProvider, useSession } from "./lib/session";
import { AppDataProvider, useApp } from "./lib/appdata";
import { Shell } from "./components/Shell";
import { Spinner } from "./components/ui";
import { Login, Unconfigured } from "./pages/Login";
import { Home } from "./pages/Home";
import { Approvals } from "./pages/Approvals";
import { BotPage } from "./pages/BotPage";
import { Lazy, lazyPage, preloadLazyPages } from "./lib/lazy";

const GlobalRules = lazyPage(() => import("./pages/Rules").then((m) => ({ default: m.GlobalRules })));
const Integrations = lazyPage(() => import("./pages/Integrations").then((m) => ({ default: m.Integrations })));
const AppSettings = lazyPage(() => import("./pages/AppSettings").then((m) => ({ default: m.AppSettings })));

function Authed() {
  const { token, ready } = useSession();
  if (!ready) return <Spinner label="Signing in" />;
  if (!token) return isTauri ? <DesktopSetup /> : <Login />;
  return (
    <AppDataProvider>
      <Gate />
    </AppDataProvider>
  );
}

/** Desktop with no teammates yet: show first-run creation, then jump into its first chat. */
function Gate() {
  const { bots, botsLoaded, reload } = useApp();
  const nav = useNavigate();
  useEffect(() => { preloadLazyPages(); }, []);
  const [target, setTarget] = useState<{ path: string; slug: string } | null>(null);
  useEffect(() => {
    if (target && bots.some((b) => b.slug === target.slug)) { nav(target.path); setTarget(null); }
  }, [target, bots, nav]);
  if (isTauri && botsLoaded && (bots.length === 0 || target)) {
    if (target) return <Spinner label="Setting up" />;
    return <FirstTeammate onCreated={(path, slug) => { setTarget({ path, slug }); reload(); }} />;
  }
  return (
    <>
      <Routes>
        <Route element={<Shell />}>
          <Route index element={<Home />} />
          <Route path="approvals" element={<Approvals />} />
          <Route path="integrations" element={<Lazy><Integrations /></Lazy>} />
          <Route path="rules" element={<Lazy><GlobalRules /></Lazy>} />
          <Route path="settings" element={<Lazy><AppSettings /></Lazy>} />
          <Route path="bot/:slug/:tab?/:sub?" element={<BotPage />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>
      </Routes>
    </>
  );
}

function Waking() {
  const waking = useWaking();
  if (!waking) return null;
  return (
    <div role="status" className="settle fixed top-2 left-1/2 -translate-x-1/2 z-[60] rounded-full border border-line bg-surface px-3 py-1 text-xs text-muted shadow-sm">
      Waking server…
    </div>
  );
}

export function App() {
  if (!isConfigured) return <Unconfigured />;
  return (
    <SessionProvider>
      <ToastProvider>
        <HashRouter>
          <Authed />
        </HashRouter>
        <Waking />
      </ToastProvider>
    </SessionProvider>
  );
}
