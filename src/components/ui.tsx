import type { ReactNode } from "react";
import { cn } from "../lib/utils";

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
