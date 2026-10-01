import { artifactUrl } from "../lib/api";
import { cx } from "./ui";

export function fmtBytes(n: number | null | undefined): string {
  if (n == null) return "";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export interface ArtifactLike { id: string; name: string; mime: string; bytes: number }

/** Image artifacts render inline; everything else is a download chip. */
export function ArtifactView({ a, className }: { a: ArtifactLike; className?: string }) {
  const url = artifactUrl(a.id);
  if (a.mime?.startsWith("image/")) {
    return (
      <a href={url} target="_blank" rel="noreferrer" className={cx("block", className)} title={a.name}>
        <img src={url} alt={a.name} loading="lazy" className="max-h-72 max-w-full rounded-md border border-line bg-sunken" />
        <span className="mono text-xs text-muted">{a.name} · {fmtBytes(a.bytes)}</span>
      </a>
    );
  }
  return (
    <a href={url} download={a.name} className={cx("inline-flex items-center gap-2 rounded-md border border-line bg-surface hover:bg-sunken px-3 min-h-9 text-sm", className)}>
      <span className="mono truncate max-w-56">{a.name}</span>
      <span className="text-xs text-muted">{fmtBytes(a.bytes)}</span>
    </a>
  );
}
