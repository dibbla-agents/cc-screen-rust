import { useCallback, useEffect, useRef, useState } from "react";
import {
  fetchTools,
  fetchUpdateJob,
  jobRunning,
  startAssistantUpdate,
  type MachineInfo,
  type Session,
  type Tool,
  type UpdateJob,
} from "../api";

// UpdateAssistants — the one action that keeps the coding assistants current
// (proposal 0049): update the CLIs on the chosen machines, then restart the
// sessions that use them so each comes back with its conversation resumed.
//
// ONE surface, two states. It opens as a confirmation dialog that names the
// blast radius (which machines, which CLIs, how many sessions will restart),
// and — on confirm — becomes the live progress panel for the same job rather
// than handing off to a second overlay you'd have to go find.
//
// Closing does NOT cancel: the job runs on the agent, so closing just stops
// watching. Re-opening (or reloading the page, or unlocking a phone that slept
// through it) resumes watching, because every status here is server state.

// A machine's slot in this dialog. `key` is the `?machine=` value — "" for a
// standalone agent with no hub, where the whole roster collapses to one row.
interface Slot {
  key: string;
  label: string;
  online: boolean;
  // The last-known job on that machine (its `tools[]` carries the versions we
  // show before starting), or the reason it can't take part.
  job?: UpdateJob;
  tools?: Tool[];
  blocked?: string;
}

const TOOL_LABELS: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex CLI",
  gemini: "Gemini CLI",
  kimi: "Kimi CLI",
};
const toolLabel = (prefix: string) => TOOL_LABELS[prefix] ?? prefix;

// Row glyphs. Every state is carried by text as well — the glyph and colour are
// reinforcement, never the only signal.
const GLYPH: Record<string, string> = {
  pending: "·",
  updating: "⟳",
  stopping: "⟳",
  starting: "⟳",
  updated: "✓",
  current: "✓",
  resumed: "✓",
  failed: "✗",
  skipped: "–",
};
const TONE: Record<string, string> = {
  pending: "text-slate-500",
  updating: "text-accent",
  stopping: "text-accent",
  starting: "text-accent",
  updated: "text-amber",
  current: "text-slate-300",
  resumed: "text-amber",
  failed: "text-claude",
  skipped: "text-slate-500",
};

function StateRow({
  state,
  title,
  detail,
}: {
  state: string;
  title: string;
  detail?: string;
}) {
  return (
    <div className="flex items-start gap-2 py-1 text-[13px]">
      <span aria-hidden className={`w-4 shrink-0 text-center ${TONE[state] ?? "text-slate-400"}`}>
        {GLYPH[state] ?? "·"}
      </span>
      <span className="min-w-0 flex-1 truncate text-slate-200" title={title}>
        {title}
      </span>
      <span className={`shrink-0 text-right text-[11px] ${TONE[state] ?? "text-slate-400"}`}>
        {detail || state}
      </span>
    </div>
  );
}

// The two-step phase strip — the headline of the progress view.
function PhaseStrip({ phase }: { phase: string }) {
  const step = phase === "updating" ? 1 : phase === "restarting" ? 2 : phase === "done" ? 3 : 0;
  const lit = (n: number) =>
    step === n ? "text-accent" : step > n ? "text-slate-300" : "text-slate-600";
  return (
    <div className="sticky top-0 z-10 flex items-center gap-2 border-b border-edge/60 bg-bar px-4 py-2 text-[11px] uppercase tracking-wider">
      <span className={lit(1)}>① Updating CLIs</span>
      <span aria-hidden className="text-slate-700">──▶</span>
      <span className={lit(2)}>② Restarting sessions</span>
      {step === 3 && <span className="ml-auto text-amber">done</span>}
    </div>
  );
}

export default function UpdateAssistants({
  open,
  machines,
  sessions,
  scopeMachine,
  onClose,
  onDone,
  onBusyChange,
}: {
  open: boolean;
  machines: MachineInfo[];
  sessions: Session[];
  /// Pre-scope to one machine (the per-machine entry point in the dashboard).
  scopeMachine?: string;
  onClose: () => void;
  /// Called when every watched job reaches `done`, so the app can re-poll its
  /// sessions and tools promptly rather than waiting out the normal interval.
  onDone: () => void;
  /// Mirrors "a job is moving" out to the header button, which keeps a live
  /// indicator while this panel is closed.
  onBusyChange?: (busy: boolean) => void;
}) {
  const [slots, setSlots] = useState<Slot[]>([]);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [restartAll, setRestartAll] = useState(false);
  const [running, setRunning] = useState(false);
  const [jobs, setJobs] = useState<Record<string, UpdateJob>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  const doneFired = useRef(false);

  // Which machines are candidates. With no hub the roster is empty and the
  // single implicit "" machine stands in — the dialog then reads as a plain
  // confirm with no list.
  const roster: MachineInfo[] = scopeMachine
    ? machines.filter((m) => m.machine === scopeMachine)
    : machines;

  // Probe every candidate when the dialog opens: the GET both fetches the last
  // job (whose rows carry the versions we display) and tells us whether this
  // client may act at all — 403 for a machine that's only shared with you,
  // 501 for an agent too old to self-update. Blocked machines are listed
  // DISABLED WITH THE REASON, never hidden: hiding them would make a fleet
  // action quietly partial.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    const targets: Slot[] =
      roster.length === 0 && !scopeMachine
        ? [{ key: "", label: "this machine", online: true }]
        : roster.map((m) => ({ key: m.machine, label: m.machine, online: m.online }));
    setSlots(targets);
    setPicked(new Set(targets.filter((t) => t.online).map((t) => t.key)));
    Promise.all(
      targets.map(async (t): Promise<Slot> => {
        if (!t.online) return { ...t, blocked: "offline — will be skipped" };
        try {
          const [job, tools] = await Promise.all([
            fetchUpdateJob(t.key),
            fetchTools(t.key).catch(() => [] as Tool[]),
          ]);
          return { ...t, job, tools: tools.filter((x) => !x.unavailable) };
        } catch (e) {
          return { ...t, blocked: reasonOf(e) };
        }
      })
    ).then((resolved) => {
      if (cancelled) return;
      setSlots(resolved);
      setPicked(new Set(resolved.filter((s) => !s.blocked).map((s) => s.key)));
      // Re-attach to a job that's still running from an earlier visit (or from
      // another device) instead of offering to start a second one.
      const live = resolved.filter((s) => jobRunning(s.job));
      if (live.length) {
        setJobs(Object.fromEntries(live.map((s) => [s.key, s.job as UpdateJob])));
        setRunning(true);
      }
    });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, scopeMachine, machines.map((m) => `${m.machine}:${m.online}`).join(",")]);

  // Esc closes (watching only — the job runs on).
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onClose]);

  // Poll each watched machine while anything is still moving.
  const anyRunning = Object.values(jobs).some(jobRunning);
  useEffect(() => {
    if (!open || !running || !anyRunning) return;
    const id = window.setInterval(() => {
      Object.keys(jobs).forEach((key) => {
        fetchUpdateJob(key)
          .then((j) => setJobs((prev) => ({ ...prev, [key]: j })))
          .catch((e) => setErrors((prev) => ({ ...prev, [key]: reasonOf(e) })));
      });
    }, 1500);
    return () => window.clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, running, anyRunning, Object.keys(jobs).join(",")]);

  // Once every watched job is done, let the app re-poll (panes re-attach to the
  // restarted sessions promptly rather than after the normal interval).
  useEffect(() => {
    if (!running || anyRunning) return;
    if (!Object.keys(jobs).length || doneFired.current) return;
    doneFired.current = true;
    onDone();
  }, [running, anyRunning, jobs, onDone]);

  useEffect(() => {
    onBusyChange?.(running && anyRunning);
  }, [running, anyRunning, onBusyChange]);

  const start = useCallback(async () => {
    doneFired.current = false;
    setRunning(true);
    setErrors({});
    const targets = slots.filter((s) => picked.has(s.key) && !s.blocked);
    await Promise.all(
      targets.map(async (t) => {
        try {
          const j = await startAssistantUpdate(t.key, { restart: restartAll ? "all" : "updated" });
          setJobs((prev) => ({ ...prev, [t.key]: j }));
        } catch (e) {
          // A per-machine failure is a row, never fatal to the rest.
          setErrors((prev) => ({ ...prev, [t.key]: reasonOf(e) }));
        }
      })
    );
  }, [slots, picked, restartAll]);

  if (!open) return null;

  const sessionCount = (key: string) =>
    sessions.filter((s) => s.tool !== "shell" && (key === "" || s.machine === key)).length;
  const chosen = slots.filter((s) => picked.has(s.key) && !s.blocked);
  const totalSessions = chosen.reduce((n, s) => n + sessionCount(s.key), 0);
  const phase = running
    ? anyRunning
      ? Object.values(jobs).some((j) => j.phase === "updating")
        ? "updating"
        : "restarting"
      : "done"
    : "idle";

  return (
    <>
      <div className="fixed inset-0 z-[85] bg-black/50" onClick={onClose} aria-hidden="true" />
      <div
        role="dialog"
        aria-label="Update coding assistants"
        className="fixed inset-x-0 bottom-0 z-[86] flex max-h-[85vh] flex-col overflow-hidden rounded-t-2xl border border-edge bg-bar pb-safe shadow-2xl sm:inset-x-auto sm:bottom-auto sm:left-1/2 sm:top-1/2 sm:w-[min(94vw,34rem)] sm:-translate-x-1/2 sm:-translate-y-1/2 sm:rounded-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="shrink-0 border-b border-edge px-4 py-3">
          <div className="text-sm font-semibold text-slate-100">
            {running ? "Updating coding assistants" : "Update coding assistants"}
          </div>
          <div className="text-xs text-slate-400">
            {running
              ? "This runs on the machine — closing this panel doesn’t stop it."
              : "Updates the CLIs, then restarts the sessions that use them — each resumes its conversation."}
          </div>
        </div>

        {running && <PhaseStrip phase={phase} />}
        <div aria-live="polite" className="sr-only">
          {phase === "updating"
            ? "Updating CLIs"
            : phase === "restarting"
              ? "Restarting sessions"
              : phase === "done"
                ? "Update finished"
                : ""}
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
          {!running &&
            slots.map((s) => {
              const disabled = !!s.blocked;
              const versions = (s.job?.tools ?? [])
                .filter((t) => t.to)
                .map((t) => `${t.tool} ${t.to}`)
                .join(" · ");
              const available = (s.tools ?? []).map((t) => t.prefix).join(" · ");
              return (
                <label
                  key={s.key || "self"}
                  className={`flex items-start gap-3 rounded-lg px-2 py-2.5 ${
                    disabled ? "opacity-50" : "hover:bg-edge/50"
                  }`}
                >
                  <input
                    type="checkbox"
                    className="mt-0.5 h-4 w-4 shrink-0 accent-amber"
                    disabled={disabled}
                    checked={picked.has(s.key)}
                    onChange={(e) =>
                      setPicked((prev) => {
                        const next = new Set(prev);
                        if (e.target.checked) next.add(s.key);
                        else next.delete(s.key);
                        return next;
                      })
                    }
                  />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-mono text-sm text-slate-100">{s.label}</span>
                    <span className="block truncate text-[11px] text-slate-500">
                      {s.blocked || versions || available || "no assistants detected"}
                    </span>
                  </span>
                  {!disabled && (
                    <span className="shrink-0 text-[11px] text-slate-400">
                      {sessionCount(s.key)} sess.
                    </span>
                  )}
                </label>
              );
            })}

          {running &&
            slots
              .filter((s) => jobs[s.key] || errors[s.key])
              .map((s) => {
                const j = jobs[s.key];
                const err = errors[s.key];
                return (
                  <div key={s.key || "self"} className="mb-3">
                    <div className="mb-1 flex items-baseline gap-2">
                      <span className="font-mono text-xs font-semibold text-slate-100">{s.label}</span>
                      {j && <span className="text-[11px] text-slate-500">{j.phase}</span>}
                    </div>
                    {err && <div className="py-1 text-[12px] text-claude">{err}</div>}
                    {(j?.tools ?? []).map((t) => (
                      <StateRow
                        key={t.tool}
                        state={t.state}
                        title={t.label || toolLabel(t.tool)}
                        detail={
                          t.state === "updated"
                            ? `${t.from ?? "?"} → ${t.to ?? "?"}`
                            : t.state === "current"
                              ? `already current${t.to ? ` (${t.to})` : ""}`
                              : t.message || undefined
                        }
                      />
                    ))}
                    {(j?.sessions ?? []).map((r) => (
                      <StateRow
                        key={r.session}
                        state={r.state}
                        title={r.session}
                        detail={r.message || undefined}
                      />
                    ))}
                  </div>
                );
              })}
          {running && !Object.keys(jobs).length && !Object.keys(errors).length && (
            <div className="py-6 text-center text-xs text-slate-500">Starting…</div>
          )}
        </div>

        <div className="shrink-0 border-t border-edge px-4 py-3">
          {!running && (
            <label className="mb-3 flex items-center gap-2 text-[12px] text-slate-400">
              <input
                type="checkbox"
                className="h-4 w-4 accent-amber"
                checked={restartAll}
                onChange={(e) => setRestartAll(e.target.checked)}
              />
              Also restart sessions whose CLI didn’t change
            </label>
          )}
          <div className="flex items-center gap-2">
            <span className="min-w-0 flex-1 truncate text-[11px] text-slate-500">
              {running
                ? anyRunning
                  ? "Running — safe to close."
                  : "Finished."
                : `${chosen.length} machine${chosen.length === 1 ? "" : "s"} · ${totalSessions} session${
                    totalSessions === 1 ? "" : "s"
                  } will restart`}
            </span>
            <button
              onClick={onClose}
              className="min-h-[44px] rounded-lg px-3 text-sm text-slate-300 hover:text-slate-100"
            >
              {running ? "Close" : "Cancel"}
            </button>
            {!running && (
              <button
                onClick={start}
                disabled={chosen.length === 0}
                className="min-h-[44px] rounded-lg bg-amber px-3 text-sm font-semibold text-bar disabled:opacity-40"
              >
                Update &amp; restart
              </button>
            )}
          </div>
        </div>
      </div>
    </>
  );
}

function reasonOf(e: unknown): string {
  const msg = e instanceof Error ? e.message : String(e);
  return msg.replace(/^Error:\s*/, "") || "failed";
}
