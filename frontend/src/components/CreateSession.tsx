import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  createSession,
  fetchDirs,
  fetchTools,
  makeDir,
  searchDirs,
  type DirSearchResult,
  type DirsResp,
  type MachineInfo,
  type PaneRef,
  type Tool,
} from "../api";
import { toolColor } from "../util";

// Search-first create flow (proposal 0016, Part B). Rendered *inside* the
// session drawer body — full-screen on phone (the drawer is full-screen there),
// a 320px column on desktop (the slide-in sidebar) — so creating a session never
// blanks/resizes the terminal. Type a few letters of a project and the recursive
// folder search surfaces it from anywhere under $HOME; ⏎ creates a session there
// with smart defaults (last-used tool, basename name, YOLO on). Tool / name /
// YOLO / extra folders stay reachable but off the hot path.

const LAST_TOOL_KEY = "ccs.lastTool";

interface Props {
  // The hub's roster + whether to show the machine selector. Creating runs on
  // `initialMachine` (the pane the user came from, else the first online agent),
  // changeable in-flow when there's >1 agent.
  machines: MachineInfo[];
  multiMachine: boolean;
  initialMachine: string;
  // Seeded from the sidebar filter when "New session 'q'" was picked, so the
  // folder search starts already filtered (Part A → Part B handoff).
  initialQuery?: string;
  // Folders to surface under "Recent" on an empty query — the cwds of
  // restorable sessions (live-session cwds aren't exposed client-side, but the
  // server-side ranker still floats them when searching).
  recentDirs?: string[];
  onBack: () => void; // back to the session list
  onClose: () => void; // close the drawer entirely
  onCreated: (ref: PaneRef) => void;
}

function basename(p: string): string {
  const parts = p.split("/").filter(Boolean);
  return parts[parts.length - 1] || "/";
}

// A row the cursor can land on: a folder (⏎ creates a session in it, → descends
// into it) or the "create folder" affordance (⏎ makes it, then creates there).
type Row =
  | { kind: "dir"; path: string; rel: string; name: string; here?: boolean; recent?: boolean }
  | { kind: "mkdir"; folder: string; root: string; rootRel: string };

export default function CreateSession({
  machines,
  multiMachine,
  initialMachine,
  initialQuery = "",
  recentDirs = [],
  onBack,
  onClose,
  onCreated,
}: Props) {
  const [tools, setTools] = useState<Tool[]>([]);
  const [tool, setTool] = useState<string>("");
  const [selectedMachine, setSelectedMachine] = useState(initialMachine);
  // The search root (default $HOME); → on a result re-roots here for deep browse.
  const [root, setRoot] = useState<string>("");
  const [query, setQuery] = useState(initialQuery);
  const [results, setResults] = useState<DirSearchResult[]>([]);
  const [browse, setBrowse] = useState<DirsResp | null>(null);
  const [home, setHome] = useState<string>("");
  const [cursor, setCursor] = useState(0);
  const [name, setName] = useState("");
  const nameEdited = useRef(false);
  const [skipPermissions, setSkipPermissions] = useState(true);
  const [extraDirs, setExtraDirs] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const searchRef = useRef<HTMLInputElement>(null);
  const nameRef = useRef<HTMLInputElement>(null);
  const rowRefs = useRef<(HTMLElement | null)[]>([]);

  const selectedTool = tools.find((t) => t.cmd === tool || t.prefix === tool);
  const extraSupport = selectedTool?.extraDirs;
  const extraLimit = extraSupport?.max || 0;
  const extraOverLimit = extraLimit > 0 && extraDirs.length > extraLimit;

  const rel = useCallback(
    (p: string) => {
      if (!home) return p;
      if (p === home) return "~";
      if (p.startsWith(home + "/")) return "~" + p.slice(home.length);
      return p;
    },
    [home]
  );

  // (Re)load tools when the panel mounts or the machine changes. Tool defaults to
  // the last-used one (persisted), falling back to the first tool.
  useEffect(() => {
    fetchTools(selectedMachine)
      .then((ts) => {
        setTools(ts);
        const last = localStorage.getItem(LAST_TOOL_KEY) || "";
        setTool((cur) => {
          if (cur && ts.some((t) => t.cmd === cur)) return cur;
          if (last && ts.some((t) => t.cmd === last)) return last;
          return ts[0]?.cmd || "";
        });
      })
      .catch(() => {});
  }, [selectedMachine]);

  // Browse the current root (empty-query fallback + descend target). Resets the
  // search root's listing whenever `root`/machine changes.
  useEffect(() => {
    let alive = true;
    fetchDirs(root || undefined, selectedMachine)
      .then((d) => {
        if (!alive) return;
        setBrowse(d);
        setHome(d.home);
        if (!root) setRoot(d.path); // adopt the resolved $HOME as the root
      })
      .catch((e) => alive && setErr(e instanceof Error ? e.message : String(e)));
    return () => {
      alive = false;
    };
  }, [root, selectedMachine]);

  // Debounced recursive search whenever the query (or root/machine) changes.
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setResults([]);
      return;
    }
    const id = setTimeout(() => {
      searchDirs(q, root || undefined, selectedMachine)
        .then((r) => setResults(r.results))
        .catch((e) => setErr(e instanceof Error ? e.message : String(e)));
    }, 120);
    return () => clearTimeout(id);
  }, [query, root, selectedMachine]);

  // Build the navigable row list from the current mode (search vs browse).
  const rows = useMemo<Row[]>(() => {
    const q = query.trim();
    if (q) {
      const out: Row[] = results.map((r) => ({
        kind: "dir" as const,
        path: r.path,
        rel: r.rel,
        name: r.name,
      }));
      // Always offer "create folder ‹q›" as the last row — the one-step new
      // project path (and the only result when nothing matches).
      if (/^[^/.][^/]*$/.test(q)) {
        out.push({ kind: "mkdir", folder: q, root, rootRel: rel(root) });
      }
      return out;
    }
    // Empty query → one-level browse: create-here, then recents, then subdirs.
    const out: Row[] = [];
    if (browse) {
      out.push({
        kind: "dir",
        path: browse.path,
        rel: rel(browse.path),
        name: browse.atHome ? "" : basename(browse.path),
        here: true,
      });
    }
    const seen = new Set([browse?.path]);
    for (const d of recentDirs) {
      if (!d || seen.has(d)) continue;
      seen.add(d);
      out.push({ kind: "dir", path: d, rel: rel(d), name: basename(d), recent: true });
    }
    for (const d of browse?.dirs || []) {
      if (seen.has(d.path)) continue;
      out.push({ kind: "dir", path: d.path, rel: rel(d.path), name: d.name });
    }
    return out;
  }, [query, results, browse, recentDirs, root, rel]);

  // Keep the cursor in range and parked on the top row as the list changes.
  useEffect(() => {
    setCursor((c) => (c >= rows.length ? 0 : c));
  }, [rows.length]);
  useEffect(() => {
    setCursor(0);
  }, [query]);
  useEffect(() => {
    rowRefs.current[cursor]?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  // Sync the auto-derived name to the highlighted folder's basename until the
  // user edits it. (Create-here at $HOME has no basename → blank, user types.)
  const activeRow = rows[cursor];
  useEffect(() => {
    if (nameEdited.current) return;
    if (activeRow?.kind === "dir") setName(activeRow.name);
    else if (activeRow?.kind === "mkdir") setName(activeRow.folder);
  }, [activeRow]);

  // Focus the search box on mount so typing filters immediately.
  useEffect(() => {
    const id = requestAnimationFrame(() => searchRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, []);

  const targetDir =
    activeRow?.kind === "dir"
      ? activeRow.path
      : activeRow?.kind === "mkdir"
        ? `${activeRow.root}/${activeRow.folder}`
        : "";

  const doCreate = useCallback(
    async (dir: string, sessionName: string) => {
      if (!tool || !dir) return;
      if (extraOverLimit) {
        setErr(`${selectedTool?.prefix || "tool"} allows ${extraLimit} extra folders`);
        return;
      }
      setBusy(true);
      setErr(null);
      try {
        localStorage.setItem(LAST_TOOL_KEY, tool);
        const ref = await createSession(
          tool,
          sessionName,
          dir,
          extraSupport ? extraDirs : [],
          selectedMachine,
          skipPermissions
        );
        onCreated(ref);
      } catch (e) {
        setErr(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(false);
      }
    },
    [tool, extraOverLimit, selectedTool, extraLimit, extraSupport, extraDirs, selectedMachine, skipPermissions, onCreated]
  );

  // Activate the highlighted row: a folder creates a session there; the mkdir row
  // makes the folder then creates in it.
  const activate = useCallback(
    async (row: Row | undefined) => {
      if (!row) return;
      if (row.kind === "mkdir") {
        try {
          await makeDir(row.root, row.folder, selectedMachine);
        } catch (e) {
          // Already-exists is fine — fall through and create the session in it.
          const msg = e instanceof Error ? e.message : String(e);
          if (!/exist/i.test(msg)) {
            setErr(msg);
            return;
          }
        }
        void doCreate(`${row.root}/${row.folder}`, name.trim() || row.folder);
        return;
      }
      void doCreate(row.path, name.trim());
    },
    [doCreate, name, selectedMachine]
  );

  // Descend into a folder row (re-root the recursive search there, clear query).
  const descend = useCallback(
    (row: Row | undefined) => {
      if (row?.kind !== "dir") return;
      nameEdited.current = false;
      setQuery("");
      setRoot(row.path);
    },
    []
  );

  const goUp = useCallback(() => {
    if (!browse || browse.atHome) return;
    nameEdited.current = false;
    setQuery("");
    setRoot(browse.parent);
  }, [browse]);

  // Capture-phase keyboard contract — mirrors SessionDrawer/NewSessionPanel: the
  // terminal's xterm textarea may still hold focus underneath and would swallow
  // arrows/Enter/Esc via stopPropagation, so we intercept in capture phase.
  // Printable keys still type into the focused input natively.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // Let the name field own its keys (Enter creates, arrows move the caret),
      // except Esc which is layered below.
      const onName = document.activeElement === nameRef.current;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (query) {
          setQuery("");
          searchRef.current?.focus();
        } else {
          onBack();
        }
        return;
      }
      if (onName) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        e.stopPropagation();
        setCursor((i) => Math.min(rows.length - 1, i + 1));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        setCursor((i) => Math.max(0, i - 1));
      } else if (e.key === "ArrowRight") {
        // → descends into the highlighted folder for deep browse (Tab is left
        // native so it can reach the name field / tool pills).
        if (activeRow?.kind === "dir") {
          e.preventDefault();
          e.stopPropagation();
          descend(activeRow);
        }
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        e.stopPropagation();
        goUp();
      } else if (e.key === "Enter") {
        e.preventDefault();
        e.stopPropagation();
        void activate(activeRow);
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [rows.length, activeRow, query, onBack, activate, descend, goUp]);

  const canCreate = !busy && !!tool && !!targetDir && (name.trim().length > 0 || activeRow?.kind === "mkdir");

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* Header: back to list, title, machine selector, close. */}
      <div className="flex items-center gap-2 border-b border-edge/80 px-2 py-2">
        <button
          onClick={onBack}
          aria-label="Back to sessions"
          className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-slate-400 hover:bg-edge/60 hover:text-slate-100"
        >
          ‹
        </button>
        <span className="text-[13px] font-semibold tracking-wide text-slate-100">New session</span>
        {multiMachine && (
          <select
            value={selectedMachine}
            onChange={(e) => {
              nameEdited.current = false;
              setSelectedMachine(e.target.value);
              setRoot("");
              setQuery("");
            }}
            className="ml-auto min-w-0 max-w-[40%] truncate rounded-md bg-panel px-1.5 py-1 text-[11px] text-slate-200"
            title="Machine"
          >
            {machines.map((m) => (
              <option key={m.machine} value={m.machine} disabled={!m.online}>
                {(m.hostname || m.machine) + (m.online ? "" : " (offline)")}
              </option>
            ))}
          </select>
        )}
        <button
          onClick={onClose}
          aria-label="Close"
          className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-slate-400 hover:bg-edge/60 hover:text-slate-100 ${
            multiMachine ? "" : "ml-auto"
          }`}
        >
          ✕
        </button>
      </div>

      {/* Folder search box + current root chip + up. */}
      <div className="flex items-center gap-1.5 border-b border-edge/60 px-2 py-1.5">
        <span className="text-slate-500">🔎</span>
        <input
          ref={searchRef}
          value={query}
          onChange={(e) => {
            nameEdited.current = false;
            setQuery(e.target.value);
          }}
          placeholder="Search folders…"
          className="min-w-0 flex-1 bg-transparent text-[13px] text-slate-100 placeholder:text-slate-600 outline-none"
        />
        <button
          onClick={goUp}
          disabled={!browse || browse.atHome}
          title={browse && !browse.atHome ? `Up to ${rel(browse.parent)}` : "At home"}
          className="shrink-0 rounded bg-panel px-1.5 py-0.5 text-[11px] text-slate-400 hover:bg-edge disabled:opacity-30"
        >
          {rel(root || home || "~")} ⬆︎
        </button>
      </div>

      {err && <div className="border-b border-edge/40 px-3 py-1.5 text-[12px] text-red-400">{err}</div>}

      {/* Result / browse list. */}
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-1">
        {rows.length === 0 && (
          <div className="px-3 py-8 text-center text-[12px] text-slate-600">
            {query.trim() ? "No matching folders." : "No folders here."}
          </div>
        )}
        {rows.map((row, i) => {
          const focused = i === cursor;
          const ring = focused ? "bg-edge/70 ring-1 ring-inset ring-accent/40" : "hover:bg-edge/40";
          if (row.kind === "mkdir") {
            return (
              <button
                key="__mkdir"
                ref={(el) => {
                  rowRefs.current[i] = el;
                }}
                onClick={() => void activate(row)}
                className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors ${ring}`}
              >
                <span className="text-accent">＋</span>
                <span className="min-w-0 flex-1 truncate text-[13px] text-slate-200">
                  Create folder “{row.folder}” in {row.rootRel}
                </span>
              </button>
            );
          }
          return (
            <button
              key={row.path}
              ref={(el) => {
                rowRefs.current[i] = el;
              }}
              onClick={() => void activate(row)}
              onDoubleClick={() => descend(row)}
              className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors ${ring}`}
            >
              <span className="text-slate-500">{row.here ? "◉" : "📁"}</span>
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[13px] text-slate-100">
                  {row.here ? `Create here · ${row.rel}` : row.rel}
                </span>
                {row.recent && (
                  <span className="block text-[10px] uppercase tracking-wide text-slate-600">recent</span>
                )}
              </span>
              {!row.here && (
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    descend(row);
                  }}
                  aria-label={`Open ${row.name}`}
                  className="shrink-0 px-1 text-slate-600 hover:text-slate-300"
                >
                  ›
                </button>
              )}
            </button>
          );
        })}
      </div>

      {/* Footer: tool, YOLO, extra folders, name + Create. */}
      <div className="shrink-0 border-t border-edge/80 bg-panel/60 p-2 pb-safe">
        <div className="mb-1.5 flex gap-1 overflow-x-auto">
          {tools.map((t) => (
            <button
              key={t.cmd}
              onClick={() => setTool(t.cmd)}
              className={`shrink-0 rounded-full px-2.5 py-1 text-[11px] font-semibold uppercase tracking-wide ${
                tool === t.cmd ? `${toolColor(t.prefix)} text-bar` : "bg-bar text-slate-400"
              }`}
            >
              {t.prefix}
            </button>
          ))}
        </div>

        <div className="mb-1.5 flex items-center gap-2">
          <button
            type="button"
            role="switch"
            aria-checked={skipPermissions}
            onClick={() => setSkipPermissions((v) => !v)}
            className="flex items-center gap-1.5 rounded-md px-1.5 py-1 text-[11px] text-slate-300 hover:bg-edge/50"
            title={
              skipPermissions
                ? "YOLO: runs tools without asking"
                : "Pauses for approval on risky actions"
            }
          >
            <span className={`relative h-4 w-7 shrink-0 rounded-full transition-colors ${skipPermissions ? "bg-amber" : "bg-edge"}`}>
              <span className={`absolute top-0.5 h-3 w-3 rounded-full bg-slate-100 transition-all ${skipPermissions ? "left-[0.875rem]" : "left-0.5"}`} />
            </span>
            <span className="font-semibold">YOLO</span>
          </button>
          {extraSupport && (
            <span className="ml-auto text-[11px] text-slate-500">
              {extraDirs.length > 0 ? `${extraDirs.length} extra folder${extraDirs.length > 1 ? "s" : ""}` : ""}
            </span>
          )}
        </div>

        {extraSupport && (
          <ExtraDirs
            machine={selectedMachine}
            projectDir={targetDir}
            home={home}
            limit={extraLimit}
            value={extraDirs}
            onChange={setExtraDirs}
          />
        )}

        <div className="flex gap-1.5">
          <input
            ref={nameRef}
            value={name}
            onChange={(e) => {
              nameEdited.current = true;
              setName(e.target.value);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void activate(activeRow);
              }
            }}
            placeholder="session name"
            className="min-w-0 flex-1 rounded-md border border-edge bg-bar px-2 py-2 font-mono text-[13px] text-slate-100 outline-none focus:border-accent"
          />
          <button
            onClick={() => void activate(activeRow)}
            disabled={!canCreate}
            className="rounded-md bg-accent px-3 py-2 text-[12px] font-semibold text-bar active:opacity-80 disabled:opacity-40"
          >
            {busy ? "…" : "Create"}
          </button>
        </div>
        <div className="mt-1 truncate text-[11px] text-slate-500">
          {targetDir ? `Creates in ${rel(targetDir)}` : "Pick a folder"}
          {extraSupport && extraDirs.length > 0 ? ` + ${extraDirs.length} extra` : ""}
        </div>
      </div>
    </div>
  );
}

// ── Extra-folders disclosure (off the hot path) ─────────────────────────────
// A collapsible folder browser to add extra workspace dirs for tools that
// support them (claude --add-dir, etc.). Ported from NewSessionPanel; only the
// chrome is compacted for the narrow sidebar column.
function ExtraDirs({
  machine,
  projectDir,
  home,
  limit,
  value,
  onChange,
}: {
  machine: string;
  projectDir: string;
  home: string;
  limit: number;
  value: string[];
  onChange: (v: string[]) => void;
}) {
  const [open, setOpen] = useState(false);
  const [dirs, setDirs] = useState<DirsResp | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const rel = (p: string) => {
    if (!home) return p;
    if (p === home) return "~";
    if (p.startsWith(home + "/")) return "~" + p.slice(home.length);
    return p;
  };

  const go = async (path?: string) => {
    setErr(null);
    try {
      setDirs(await fetchDirs(path, machine));
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
  };

  useEffect(() => {
    if (open && !dirs) void go();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const toggle = (path: string) => {
    setErr(null);
    if (path === projectDir) {
      setErr("That is the project folder");
      return;
    }
    if (!value.includes(path) && limit > 0 && value.length >= limit) {
      setErr(`Up to ${limit} extra folders`);
      return;
    }
    onChange(value.includes(path) ? value.filter((p) => p !== path) : [...value, path]);
  };

  return (
    <div className="mb-1.5 rounded-md border border-edge/70 bg-bar/60">
      <button
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center gap-2 px-2 py-1.5 text-left text-[11px] text-slate-300"
      >
        <span className="text-slate-500">{open ? "▾" : "▸"}</span>
        <span className="flex-1">Extra folders{limit ? ` (max ${limit})` : ""}</span>
        <span className="text-slate-500">{value.length || ""}</span>
      </button>
      {value.length > 0 && (
        <div className="flex flex-wrap gap-1 px-2 pb-1.5">
          {value.map((p) => (
            <span key={p} className="inline-flex max-w-full items-center gap-1 rounded border border-edge bg-panel px-1.5 py-0.5 text-[10px] text-slate-300">
              <span className="truncate">{rel(p)}</span>
              <button onClick={() => onChange(value.filter((x) => x !== p))} aria-label={`Remove ${rel(p)}`} className="text-slate-500 hover:text-red-400">
                ✕
              </button>
            </span>
          ))}
        </div>
      )}
      {open && (
        <div className="border-t border-edge/50">
          <div className="flex items-center gap-1.5 px-2 py-1">
            <button
              onClick={() => dirs && go(dirs.parent)}
              disabled={!dirs || dirs.atHome}
              className="rounded bg-panel px-1.5 py-0.5 text-[11px] text-slate-400 hover:bg-edge disabled:opacity-30"
            >
              ⬆︎
            </button>
            <span className="min-w-0 flex-1 truncate text-[11px] text-slate-400">{dirs ? rel(dirs.path) : "…"}</span>
            <button
              onClick={() => dirs && toggle(dirs.path)}
              disabled={!dirs || dirs.path === projectDir}
              className={`rounded px-1.5 py-0.5 text-[10px] font-semibold disabled:opacity-30 ${
                dirs && value.includes(dirs.path) ? "bg-accent text-bar" : "bg-panel text-slate-300"
              }`}
            >
              {dirs && value.includes(dirs.path) ? "Added" : "Add here"}
            </button>
          </div>
          {err && <div className="px-2 pb-1 text-[10px] text-red-400">{err}</div>}
          <div className="max-h-40 overflow-y-auto">
            {dirs?.dirs.map((d) => {
              const selected = value.includes(d.path);
              return (
                <div key={d.path} className="flex items-center gap-1 px-2 py-1">
                  <button onClick={() => go(d.path)} className="flex min-w-0 flex-1 items-center gap-1.5 text-left text-[12px] text-slate-200">
                    <span className="text-slate-500">📁</span>
                    <span className="min-w-0 flex-1 truncate">{d.name}</span>
                  </button>
                  <button
                    onClick={() => toggle(d.path)}
                    disabled={d.path === projectDir}
                    className={`rounded px-1.5 py-0.5 text-[10px] font-semibold disabled:opacity-30 ${
                      selected ? "bg-accent text-bar" : "bg-panel text-slate-300"
                    }`}
                  >
                    {selected ? "Added" : "Add"}
                  </button>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
