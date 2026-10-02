import { useCallback, useEffect, useRef, useState } from "react";
import { ipc } from "../lib/ipc";
import type { ActivityRow } from "../lib/types";
import { EmptyState, PageHeader, StatusDot } from "../components/ui";

const KINDS = ["", "wallet", "mint", "eligibility", "nft", "vault", "chain", "pnl", "api_key", "rpc"];

export function ActivityPage() {
  const [rows, setRows] = useState<ActivityRow[]>([]);
  const [kind, setKind] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const reqId = useRef(0);

  const load = useCallback(async () => {
    const id = ++reqId.current;
    try {
      const data = await ipc<ActivityRow[]>("activity_list", {
        limit: 200,
        kind: kind || null,
      });
      // Ignore stale responses when kind filter changes mid-flight.
      if (id !== reqId.current) return;
      setRows(data);
      setErr(null);
    } catch (e) {
      if (id !== reqId.current) return;
      setErr(String(e));
    }
  }, [kind]);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="p-6">
      <PageHeader
        suite="Overview"
        title="Activity"
        subtitle="Append-only local ledger of every operation"
        action={
          <div className="flex gap-1.5">
            {KINDS.map((k) => (
              <button
                key={k || "all"}
                onClick={() => setKind(k)}
                className={`rounded-lg px-2.5 py-1.5 text-[12px] ${
                  kind === k ? "bg-line text-fg" : "text-muted hover:bg-line/50"
                }`}
              >
                {k || "All"}
              </button>
            ))}
          </div>
        }
      />
      {err ? <div className="mb-3 text-[12px] text-danger">{err}</div> : null}
      <div className="overflow-hidden rounded-[14px] border border-line bg-card">
        <div className="grid grid-cols-[150px_1fr_140px_70px] border-b border-line px-4 py-2 font-mono text-[9.5px] font-semibold uppercase tracking-[0.1em] text-muted">
          <div>Kind</div>
          <div>Summary</div>
          <div>Time</div>
          <div>Status</div>
        </div>
        {rows.length === 0 ? (
          <EmptyState title="No activity yet" description="Operations will appear here as you use the app." />
        ) : (
          rows.map((a) => (
            <div
              key={a.id}
              className="grid grid-cols-[150px_1fr_140px_70px] items-center gap-2 border-b border-line/60 px-4 py-2.5 text-[13px] last:border-0 hover:bg-line/30"
            >
              <div className="font-mono text-[12px] text-accent">{a.kind}</div>
              <div className="truncate" title={a.summary}>
                {a.summary}
              </div>
              <div className="font-mono text-[10.5px] tabular-nums text-muted">
                {new Date(a.created_at).toLocaleString()}
              </div>
              <div className="flex items-center gap-1.5 text-[12px]">
                <StatusDot ok={!!a.ok} />
                {a.ok ? "OK" : "Fail"}
              </div>
            </div>
          ))
        )}
      </div>
      <div className="mt-3 font-mono text-[9.5px] uppercase tracking-[0.1em] text-muted">{rows.length} records</div>
    </div>
  );
}
