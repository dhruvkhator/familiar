import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { api, useToken } from "./api";

interface Ctx { token: string | null; ready: boolean; }
const SessionCtx = createContext<Ctx>({ token: null, ready: false });

/** Validates a stored token once on load (a 401 clears it and sends the user to login). */
export function SessionProvider({ children }: { children: ReactNode }) {
  const token = useToken();
  const [ready, setReady] = useState(!token);
  useEffect(() => {
    if (!token) { setReady(true); return; }
    let dead = false;
    api.get("/api/me").catch(() => { /* 401 already cleared the token */ }).finally(() => { if (!dead) setReady(true); });
    return () => { dead = true; };
    // only on first load with a token; later logins are already validated by the login call
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return <SessionCtx.Provider value={{ token, ready }}>{children}</SessionCtx.Provider>;
}
export const useSession = () => useContext(SessionCtx);
