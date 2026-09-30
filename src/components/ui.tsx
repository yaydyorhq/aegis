import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { AlertTriangle, CheckCircle2, ChevronDown, ChevronUp, Info, X } from "lucide-react";
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
      // Errors need reading time; info can go quickly.
      const ttl =
        item.tone === "error" ? 9000 : item.tone === "warn" ? 6500 : 4500;
      window.setTimeout(() => {
        setItems((prev) => prev.filter((x) => x.id !== item.id));
      }, ttl);
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
  onClick,
}: {
  label: string;
  value: string | number;
  hint?: string;
  icon?: ReactNode;
  /** When set the card navigates — rendered as a button with hover affordance. */
  onClick?: () => void;
}) {
  const inner = (
    <>
      <div className="flex items-start justify-between">
        <div className="text-[12px] text-muted">{label}</div>
        {icon ? <div className="text-muted">{icon}</div> : null}
      </div>
      <div className="mt-3 text-[28px] font-semibold tracking-tight text-fg">{value}</div>
      {hint ? <div className="mt-1 text-[12px] text-muted">{hint}</div> : null}
    </>
  );
  if (onClick) {
    return (
      <button
        onClick={onClick}
        className="rounded-[14px] border border-line bg-card p-4 text-left transition-colors hover:border-accent/50"
      >
        {inner}
      </button>
    );
  }
  return <div className="rounded-[14px] border border-line bg-card p-4">{inner}</div>;
}

export type StatusTone = "ok" | "info" | "warn" | "danger" | "idle";

export function StatusDot({
  ok,
  tone,
  pulse,
}: {
  ok?: boolean;
  /** Explicit tone — wins over the boolean `ok` shorthand. */
  tone?: StatusTone;
  /** Subtle breathing for in-flight states (signing / broadcasting). */
  pulse?: boolean;
}) {
  const t = tone ?? (ok ? "ok" : "idle");
  const cls = {
    ok: "bg-ok",
    info: "bg-accent",
    warn: "bg-warn",
    danger: "bg-danger",
    idle: "bg-muted/60",
  }[t];
  return (
    <span
      className={cn(
        "inline-block h-[7px] w-[7px] shrink-0 rounded-full",
        cls,
        pulse && "animate-pulse",
      )}
    />
  );
}

/** Close overlays (modals, dialogs) with Escape while mounted/active. */
export function useDismissOnEscape(
  active: boolean,
  onClose: () => void,
): void {
  useEffect(() => {
    if (!active) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, onClose]);
}

/**
 * In-app replacement for the webview's confirm()/prompt() dialogs: matches
 * the design system, masks the passphrase field, closes on Escape/backdrop.
 * With `passwordLabel` set, confirm stays disabled until something is typed
 * and `onConfirm` receives the passphrase.
 */
export function ConfirmDialog({
  open,
  title,
  body,
  confirmLabel = "Confirm",
  cancelLabel = "Cancel",
  danger = false,
  busy = false,
  passwordLabel,
  onConfirm,
  onClose,
}: {
  open: boolean;
  title: string;
  body?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  busy?: boolean;
  passwordLabel?: string;
  onConfirm: (passphrase: string) => void;
  onClose: () => void;
}) {
  const [pass, setPass] = useState("");
  useEffect(() => {
    if (open) setPass("");
  }, [open]);
  useDismissOnEscape(open, onClose);
  if (!open) return null;
  const canConfirm = !busy && (!passwordLabel || pass.length > 0);
  return (
    <div
      className="fixed inset-0 z-[75] flex items-center justify-center bg-black/60 p-4"
      onClick={onClose}
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (canConfirm) onConfirm(pass);
        }}
        onClick={(e) => e.stopPropagation()}
        className="w-[400px] rounded-[14px] border border-line bg-panel shadow-2xl"
      >
        <div className="border-b border-line px-5 py-4 text-[14px] font-semibold text-fg">
          {title}
        </div>
        <div className="space-y-2 px-5 py-4">
          {body ? (
            <div className="text-[12.5px] leading-relaxed text-muted">{body}</div>
          ) : null}
          {passwordLabel ? (
            <input
              type="password"
              autoFocus
              value={pass}
              onChange={(e) => setPass(e.target.value)}
              placeholder={passwordLabel}
              className="w-full rounded-lg border border-line bg-bg px-3 py-2 text-[13px] outline-none focus:border-accent"
            />
          ) : null}
        </div>
        <div className="flex justify-end gap-2 border-t border-line px-5 py-3.5">
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg border border-line bg-card px-3 py-2 text-[13px] text-fg hover:border-muted/40"
          >
            {cancelLabel}
          </button>
          <button
            type="submit"
            disabled={!canConfirm}
            autoFocus={passwordLabel == null}
            className={cn(
              "rounded-lg px-3.5 py-2 text-[13px] font-semibold text-white disabled:opacity-40",
              danger ? "bg-danger" : "bg-accent",
            )}
          >
            {busy ? "Working…" : confirmLabel}
          </button>
        </div>
      </form>
    </div>
  );
}

export interface DropdownItem {
  key: string;
  label: string;
  sub?: string;
  checked: boolean;
  onToggle: () => void;
}

export interface DropdownGroup {
  key: string;
  title: string;
  selected: number;
  total: number;
  onToggleAll?: () => void;
  items: DropdownItem[];
}

/** Collapsed multi-select with grouped, select-all-per-group rows. */
export function MultiSelectDropdown({
  summary,
  groups,
  emptyText,
  footer,
  className,
}: {
  summary: string;
  groups: DropdownGroup[];
  emptyText?: string;
  footer?: ReactNode;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [dropUp, setDropUp] = useState(false);
  const boxRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      if (boxRef.current && !boxRef.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  // Flip the panel upward when the trigger sits near the viewport bottom —
  // the form scrolls inside a clipped container, so downward-only clips options.
  function toggleOpen() {
    if (!open) {
      const el = boxRef.current;
      if (el) {
        const rect = el.getBoundingClientRect();
        setDropUp(window.innerHeight - rect.bottom < 260 && rect.top > 260);
      }
    }
    setOpen((o) => !o);
  }

  return (
    <div ref={boxRef} className={cn("relative", className)}>
      <button
        type="button"
        onClick={toggleOpen}
        className="flex w-full items-center justify-between gap-2 rounded-lg border border-line bg-card px-3 py-2 text-left text-[13px] hover:border-muted/40"
      >
        <span className="truncate text-fg">{summary}</span>
        {open ? (
          <ChevronUp className="h-4 w-4 shrink-0 text-muted" />
        ) : (
          <ChevronDown className="h-4 w-4 shrink-0 text-muted" />
        )}
      </button>
      {open ? (
        <div
          className={cn(
            "absolute left-0 right-0 z-30 max-h-64 overflow-y-auto rounded-lg border border-line bg-card shadow-xl",
            dropUp ? "bottom-full mb-1" : "top-full mt-1",
          )}
        >
          {groups.length === 0 ? (
            <div className="px-3 py-3 text-[12px] text-muted">
              {emptyText ?? "Nothing to select"}
            </div>
          ) : (
            groups.map((g) => {
              const all = g.total > 0 && g.selected === g.total;
              return (
                <div key={g.key} className="border-b border-line/60 last:border-0">
                  <div className="flex items-center gap-2 bg-bg/70 px-3 py-1.5">
                    <input
                      type="checkbox"
                      checked={all}
                      onChange={g.onToggleAll}
                      disabled={!g.onToggleAll}
                      aria-label={`Toggle ${g.title}`}
                      className="h-3.5 w-3.5 accent-[var(--accent,#6d5efc)] disabled:opacity-40"
                    />
                    <span className="min-w-0 flex-1 truncate text-[11px] font-medium uppercase tracking-wide text-muted">
                      {g.title}
                    </span>
                    <span className="shrink-0 font-mono text-[10px] text-muted">
                      {g.selected}/{g.total}
                    </span>
                  </div>
                  {g.items.map((it) => (
                    <label
                      key={it.key}
                      className="flex cursor-pointer items-center gap-2 px-3 py-1.5 hover:bg-bg"
                    >
                      <input
                        type="checkbox"
                        checked={it.checked}
                        onChange={it.onToggle}
                        className="h-3.5 w-3.5 shrink-0 accent-[var(--accent,#6d5efc)]"
                      />
                      <span className="min-w-0 flex-1 truncate text-[12.5px] text-fg">
                        {it.label}
                      </span>
                      {it.sub ? (
                        <span
                          className="max-w-[48%] shrink-0 truncate font-mono text-[11px] text-muted"
                          title={it.sub}
                        >
                          {it.sub}
                        </span>
                      ) : null}
                    </label>
                  ))}
                </div>
              );
            })
          )}
          {footer ? (
            <div className="border-t border-line px-3 py-2">{footer}</div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
