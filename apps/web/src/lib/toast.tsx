import { createContext, useCallback, useContext, useState, type ReactNode } from "react";

interface Toast { id: number; text: string; tone: "ok" | "bad" }
const Ctx = createContext<(text: string, tone?: "ok" | "bad") => void>(() => {});
let seq = 0;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<Toast[]>([]);
  const push = useCallback((text: string, tone: "ok" | "bad" = "ok") => {
    const id = ++seq;
    setItems((l) => [...l, { id, text, tone }]);
    setTimeout(() => setItems((l) => l.filter((t) => t.id !== id)), tone === "bad" ? 6000 : 2800);
  }, []);
  return (
    <Ctx.Provider value={push}>
      {children}
      <div className="fixed bottom-20 md:bottom-6 left-1/2 -translate-x-1/2 z-[70] flex flex-col gap-2 items-center pointer-events-none px-4" aria-live="polite">
        {items.map((t) => (
          <div key={t.id} className={"toast card px-4 py-2 text-sm " + (t.tone === "bad" ? "text-bad" : "")}>{t.text}</div>
        ))}
      </div>
    </Ctx.Provider>
  );
}
export const useToast = () => useContext(Ctx);
