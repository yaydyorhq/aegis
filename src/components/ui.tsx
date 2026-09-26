import type { ReactNode } from "react";
import { useEffect, useState } from "react";
import { AlertTriangle, CheckCircle2, Info, X } from "lucide-react";
import { cn } from "../lib/utils";

export type ToastTone = "ok" | "warn" | "info" | "error";

export interface ToastItem {
  id: number;
  title: string;
  detail?: string | null;
  tone: ToastTone;
  at: number;
}

let toastSeq = 1;

/** Fire a toast from anywhere (page or store) — host is mounted in AppShell. */
export function pushToast(
  title: string,
  tone: ToastTone = "info",
  detail?: string | null,
): void {
  window.dispatchEvent(
    new CustomEvent("aegis:toast", {
      detail: { id: toastSeq++, title, detail: detail ?? null, tone, at: Date.now() },
    }),
  );
}

export function ToastHost() {
  const [items, setItems] = useState<ToastItem[]>([]);

  useEffect(() => {
    function onToast(e: Event) {
      const item = (e as CustomEvent<ToastItem>).detail;
      setItems((prev) => [...prev, item].slice(-5));
      window.setTimeout(() => {
        setItems((prev) => prev.filter((x) => x.id !== item.id));
      }, 5000);
    }
    window.addEventListener("aegis:toast", onToast);
    return () => window.removeEventListener("aegis:toast", onToast);
  }, []);

  if (items.length === 0) return null;

  return (
    <div
      className="pointer-events-none fixed bottom-4 right-4 z-[80] flex w-[340px] flex-col gap-2"
      role="status"
      aria-live="polite"
    >
      {items.map((t) => (
        <div
          key={t.id}
          className={cn(
            "pointer-events-auto flex items-start gap-2 rounded-[12px] border bg-card px-3 py-2.5 shadow-lg",
            t.tone === "ok" && "border-ok/50",
            t.tone === "warn" && "border-warn/50",
            t.tone === "error" && "border-danger/50",
            t.tone === "info" && "border-accent/50",
          )}
        >
          <span className="mt-0.5 shrink-0">
            {t.tone === "ok" ? (
              <CheckCircle2 className="h-4 w-4 text-ok" />
            ) : t.tone === "warn" ? (
              <AlertTriangle className="h-4 w-4 text-warn" />
            ) : t.tone === "error" ? (
              <AlertTriangle className="h-4 w-4 text-danger" />
            ) : (
              <Info className="h-4 w-4 text-accent" />
            )}
          </span>
          <div className="min-w-0 flex-1">
            <div className="text-[13px] font-medium text-fg">{t.title}</div>
            {t.detail ? (
              <div className="mt-0.5 break-words text-[11.5px] leading-snug text-muted">
                {t.detail}
              </div>
            ) : null}
          </div>
          <button
            type="button"
            onClick={() => setItems((prev) => prev.filter((x) => x.id !== t.id))}
            className="shrink-0 rounded p-0.5 text-muted hover:text-fg"
            aria-label="Dismiss"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      ))}
    </div>
  );
}

export function EmptyState({
  icon,
  title,
  description,
  className,
}: {
  icon?: ReactNode;
  title: string;
  description?: string;
  className?: string;
}) {
  return (
    <div className={cn("flex flex-col items-center justify-center gap-2 px-6 py-10 text-center", className)}>
      {icon ? <div className="mb-1 text-accent">{icon}</div> : null}
      <div className="text-[14px] font-medium text-fg">{title}</div>
      {description ? <div className="max-w-sm text-[12.5px] leading-relaxed text-muted">{description}</div> : null}
    </div>
  );
}

export function PageHeader({
  suite,
  title,
  subtitle,
  action,
}: {
  suite?: string;
  title: string;
  subtitle?: string;
  action?: ReactNode;
}) {
  return (
    <div className="mb-6 flex items-start justify-between gap-4">
      <div>
        {suite ? <div className="mb-1 text-[12px] text-muted">{suite}</div> : null}
        <h1 className="text-[26px] font-semibold tracking-tight text-fg">{title}</h1>
        {subtitle ? <p className="mt-1 text-[13px] text-muted">{subtitle}</p> : null}
      </div>
      {action}
    </div>
  );
}

export function Panel({
  title,
  subtitle,
  right,
  children,
  className,
  footer,
}: {
  title?: string;
  subtitle?: string;
  right?: ReactNode;
  children: ReactNode;
  className?: string;
  footer?: ReactNode;
}) {
  return (
    <section className={cn("flex flex-col overflow-hidden rounded-[14px] border border-line bg-card", className)}>
      {title || right ? (
        <div className="flex items-start justify-between gap-3 border-b border-line px-5 py-4">
          <div>
            {subtitle ? <div className="text-[12px] text-muted">{subtitle}</div> : null}
            {title ? <div className="text-[14px] font-semibold text-fg">{title}</div> : null}
          </div>
          {right}
        </div>
      ) : null}
      <div className="min-h-0 flex-1">{children}</div>
      {footer ? (
        <div className="flex items-center justify-between border-t border-line px-5 py-3 text-[12px] text-muted">
          {footer}
        </div>
      ) : null}
    </section>
  );
}

export function StatCard({
  label,
  value,
  hint,
  icon,
}: {
  label: string;
  value: string | number;
  hint?: string;
  icon?: ReactNode;
}) {
  return (
    <div className="rounded-[14px] border border-line bg-card p-4">
      <div className="flex items-start justify-between">
        <div className="text-[12px] text-muted">{label}</div>
        {icon ? <div className="text-muted">{icon}</div> : null}
      </div>
      <div className="mt-3 text-[28px] font-semibold tracking-tight text-fg">{value}</div>
      {hint ? <div className="mt-1 text-[12px] text-muted">{hint}</div> : null}
    </div>
  );
}

export function StatusDot({ ok }: { ok: boolean }) {
  return (
    <span
      className={cn(
        "inline-block h-[7px] w-[7px] rounded-full",
        ok ? "bg-ok" : "bg-muted/60",
      )}
    />
  );
}
