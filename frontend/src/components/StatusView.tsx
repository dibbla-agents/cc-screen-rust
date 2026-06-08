// Proposal 0022 — the searchable session-status view.
//
// A filterable table, Session × Latest status, listing every session across
// every machine. Each row shows the LLM `headline` (falling back to the bare
// `preview`); the fuller `detail` is shown inline (clamped) and in the row's
// title on hover. It reads straight off the existing /api/sessions poll the rest
// of the app already runs — no extra fetch per row — so it reflects the last
// cached summary (≤ ~5 min, or instant on an attention edge).
//
// Search reuses the same fuzzy filter as the search-first sidebar (0016); the
// status dot + ordering reuse the drawer's helpers so the two surfaces agree.

import { useEffect, useMemo, useRef, useState } from "react";
import type { MachineInfo, Session } from "../api";
import { ago, agentStatus, fuzzyScore, statusDot, statusTitle, toolColor } from "../util";
import { XIcon } from "../icons";
import SummaryTip from "./SummaryTip";

interface Props {
  open: boolean;
  sessions: Session[];
  machines: MachineInfo[];
  multiMachine: boolean;
  onClose: () => void;
  // Mount the chosen session (same path as a drawer pick).
  onPick: (s: Session) => void;
}

export default function StatusView({ open, sessions, machines, multiMachine, onClose, onPick }: Props) {
  const [query, setQuery] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);

  // Focus the search on open; reset the query on close.
  useEffect(() => {
    if (open) {
      searchRef.current?.focus({ preventScroll: true });
    } else {
      setQuery("");
    }
  }, [open]);

  // Esc closes.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  // Ordering mirrors the drawer: by machine, then waiting floats to the top, then
  // most-recent activity.
  const ordered = useMemo(
    () =>
      [...sessions].sort((a, b) => {
        const ma = a.machine ?? "";
        const mb = b.machine ?? "";
        if (ma !== mb) return ma < mb ? -1 : 1;
        if (a.waiting !== b.waiting) return a.waiting ? -1 : 1;
        return b.activity - a.activity;
      }),
    [sessions]
  );

  // Same fuzzy fields as the sidebar (0016) plus the summary text.
  const q = query.trim();
  const view = useMemo(() => {
    if (!q) return ordered;
    return ordered
      .map((s) => {
        const fields = [s.short, s.headline ?? "", s.detail ?? "", s.preview, s.tool, s.machine ?? ""];
        let best: number | null = null;
        for (const f of fields) {
          const sc = fuzzyScore(q, f);
          if (sc !== null) best = best === null ? sc : Math.max(best, sc);
        }
        return best === null ? null : { s, score: best };
      })
      .filter((x): x is { s: Session; score: number } => x !== null)
      .sort((a, b) => b.score - a.score)
      .map((x) => x.s);
  }, [ordered, q]);

  return (
    <div
      className={`absolute inset-0 z-40 flex flex-col transition-opacity duration-150 ${
        open ? "opacity-100" : "pointer-events-none opacity-0"
      }`}
    >
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />
      <div className="relative z-10 m-auto flex max-h-[92%] min-h-0 w-full max-w-3xl flex-col rounded-2xl border border-edge bg-panel shadow-xl">
        <div className="flex shrink-0 items-center gap-2 border-b border-edge px-3 py-3">
          <span className="text-sm font-semibold text-slate-200">Status</span>
          <input
            ref={searchRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search sessions + status…"
            className="min-w-0 flex-1 rounded-lg border border-edge bg-bar px-3 py-2 text-sm text-slate-100 outline-none focus:border-accent"
          />
          <button
            onClick={onClose}
            aria-label="Close status view"
            className="flex flex-none items-center rounded-lg bg-edge px-2.5 py-2 text-slate-400 active:opacity-80"
          >
            <XIcon className="h-4 w-4" />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain p-2">
          {view.length === 0 ? (
            <div className="px-3 py-8 text-center text-sm text-slate-500">
              {sessions.length === 0 ? "No sessions." : "No matches."}
            </div>
          ) : (
            <ul className="flex flex-col gap-1">
              {view.map((s) => {
                const status = agentStatus(s.waiting, undefined);
                const machineLabel =
                  multiMachine && s.machine
                    ? machines.find((m) => m.machine === s.machine)?.hostname || s.machine
                    : null;
                return (
                  <li key={`${s.machine ?? ""}/${s.name}`}>
                    <button
                      type="button"
                      onClick={() => onPick(s)}
                      className="flex w-full items-start gap-2.5 rounded-lg px-2.5 py-2 text-left hover:bg-bar active:bg-edge"
                    >
                      <span
                        className={`mt-1 h-2.5 w-2.5 flex-none rounded-full ${statusDot(status)}`}
                        title={statusTitle(status)}
                      />
                      <span className="flex min-w-0 flex-1 flex-col">
                        <span className="flex items-center gap-2">
                          <span
                            className={`rounded px-1 py-px text-[9px] font-bold uppercase text-bar ${toolColor(
                              s.tool
                            )}`}
                          >
                            {s.tool}
                          </span>
                          <span className="truncate text-sm font-medium text-slate-100">{s.short}</span>
                          {machineLabel && (
                            <span className="shrink-0 rounded bg-edge/60 px-1 py-px text-[9px] text-slate-400">
                              {machineLabel}
                            </span>
                          )}
                          <span className="ml-auto shrink-0 pl-2 text-[10px] tabular-nums text-slate-500">
                            {ago(s.activity)}
                          </span>
                        </span>
                        {/* Latest status: the LLM headline, else the preview.
                            Full summary on hover (desktop) / long-press (touch). */}
                        <SummaryTip text={s.detail || s.headline || undefined} className="mt-0.5 block min-w-0">
                          <span
                            className={`block truncate text-[12px] leading-tight ${
                              s.headline ? "text-slate-300" : "font-mono text-slate-500"
                            }`}
                          >
                            {s.headline || s.preview || "—"}
                          </span>
                        </SummaryTip>
                        {/* The fuller detail inline (clamped); long-press/hover the
                            line above for the complete text. */}
                        {s.detail && (
                          <span className="mt-0.5 line-clamp-2 text-[11px] leading-snug text-slate-500">
                            {s.detail}
                          </span>
                        )}
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
}
