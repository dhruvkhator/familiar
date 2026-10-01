import { lazy, Suspense, type ComponentType, type ReactNode } from "react";

const loaders: (() => Promise<unknown>)[] = [];

/** React.lazy that also registers its chunk for idle preloading. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function lazyPage<C extends ComponentType<any>>(load: () => Promise<{ default: C }>) {
  let p: Promise<{ default: C }> | undefined;
  const once = () => (p ??= load());
  loaders.push(once);
  return lazy(once);
}

/** Fetch every lazy chunk once the app is idle, so the first navigation to them is instant. */
export function preloadLazyPages() {
  const run = () => loaders.forEach((l) => { l().catch(() => { /* retried on real navigation */ }); });
  const w = window as Window & { requestIdleCallback?: (cb: () => void, o?: { timeout: number }) => number };
  if (w.requestIdleCallback) w.requestIdleCallback(run, { timeout: 4000 });
  else setTimeout(run, 1500);
}

/** The same skeleton Today uses while it loads. */
export function PageSkeleton() {
  return <div className="p-8 max-w-3xl mx-auto space-y-3"><div className="skeleton h-10 w-60" /><div className="skeleton h-24" /><div className="skeleton h-40" /></div>;
}

export function Lazy({ children, compact }: { children: ReactNode; compact?: boolean }) {
  return <Suspense fallback={compact ? <div className="space-y-3"><div className="skeleton h-10 w-60" /><div className="skeleton h-24" /></div> : <PageSkeleton />}>{children}</Suspense>;
}
