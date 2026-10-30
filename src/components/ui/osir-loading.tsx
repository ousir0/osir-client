import type { ReactNode } from "react";
import "./osir-loading.css";

/** A shared OSIR read state. Callers keep existing data and errors visible. */
export function OsirLoading({
  label = "正在加载…",
  rows = 3,
  compact = false,
}: {
  label?: string;
  rows?: number;
  compact?: boolean;
}) {
  return (
    <div
      className="osir-loading"
      data-compact={compact || undefined}
      role="status"
    >
      <span className="osir-loading-label">
        <i aria-hidden="true" />
        {label}
      </span>
      {!compact && (
        <div className="osir-loading-lines" aria-hidden="true">
          {Array.from({ length: rows }, (_, index) => (
            <i key={index} />
          ))}
        </div>
      )}
    </div>
  );
}

export function OsirReveal({ children }: { children: ReactNode }) {
  return <div className="osir-load-reveal">{children}</div>;
}
