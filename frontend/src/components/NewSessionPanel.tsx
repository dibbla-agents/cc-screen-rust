import { useEffect, useRef, useState } from "react";
import {
  createSession,
  fetchDirs,
  fetchTools,
  makeDir,
  removeDir,
  type DirsResp,
  type MachineInfo,
  type PaneRef,
  type Tool,
} from "../api";
import { toolColor } from "../util";

interface Props {
  open: boolean;
  // The hub's machine roster + whether to show the machine selector. Single
  // machine / standalone agent hides it. The session is created on
  // `selectedMachine`, which starts at `initialMachine` (the pane the user came
  // from, else the first online agent).
  machines: MachineInfo[];
  multiMachine: boolean;
  initialMachine: string;
  onClose: () => void;
  onCreated: (ref: PaneRef) => void;
}

function basename(p: string): string {
  const parts = p.split("/").filter(Boolean);
  return parts[parts.length - 1] || "/";
}

// Full-screen, thumb-friendly flow to start a new session: browse the home
// directory tree (tap a folder to descend, ⬆︎ to go up), pick a tool, name it,
// Create. The session launches in the browsed folder, exactly like `cc` would.
export default function NewSessionPanel({
  open,
  machines,
  multiMachine,
  initialMachine,
  onClose,
  onCreated,
}: Props) {
  const [tools, setTools] = useState<Tool[]>([]);
  const [tool, setTool] = useState<string>("");
  // Which machine to create on. Adopted from initialMachine on open; changing it
  // re-fetches tools + re-browses dirs (both are per-machine).
  const [selectedMachine, setSelectedMachine] = useState(initialMachine);
  const [dirs, setDirs] = useState<DirsResp | null>(null);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const nameEdited = useRef(false);
  // Folder management within the browser.
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [confirmDel, setConfirmDel] = useState<string | null>(null);
  const [opErr, setOpErr] = useState<string | null>(null);
  // Extra workspace folders are picked in a separate browser so the main
  // project folder does not jump around while adding siblings/shared dirs.
  const [extraDirs, setExtraDirs] = useState<string[]>([]);
  const [extraPickerOpen, setExtraPickerOpen] = useState(false);
  const [extraPickerDirs, setExtraPickerDirs] = useState<DirsResp | null>(null);
  const [extraPickerErr, setExtraPickerErr] = useState<string | null>(null);

  const go = async (path?: string) => {
    setErr(null);
    setOpErr(null);
    setConfirmDel(null);
    setCreating(false);
    try {
      const d = await fetchDirs(path, selectedMachine);
      setDirs(d);
      if (!nameEdited.current) setName(d.atHome ? "" : basename(d.path));
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
  };

  const createFolder = async () => {
    if (!dirs || !newName.trim()) return;
    try {
      await makeDir(dirs.path, newName.trim(), selectedMachine);
      setCreating(false);
      setNewName("");
      go(dirs.path);
    } catch (e) {
      setOpErr(e instanceof Error ? e.message : String(e));
    }
  };

  const deleteFolder = async (path: string) => {
    try {
      await removeDir(path, false, selectedMachine);
      setConfirmDel(null);
      go(dirs!.path);
    } catch (e) {
      setConfirmDel(null);
      setOpErr(e instanceof Error ? e.message : String(e));
    }
  };

  const goExtra = async (path?: string) => {
    setExtraPickerErr(null);
    try {
      setExtraPickerDirs(await fetchDirs(path, selectedMachine));
    } catch (e) {
      setExtraPickerErr(e instanceof Error ? e.message : String(e));
    }
  };

  // Adopt the caller's initial machine each time the panel opens.
  useEffect(() => {
    if (open) setSelectedMachine(initialMachine);
  }, [open, initialMachine]);

  // (Re)load tools + the dir browser whenever the panel opens or the chosen
  // machine changes — both are per-machine ($HOME and tools.conf live on the
  // agent).
  useEffect(() => {
    if (!open) return;
    nameEdited.current = false;
    setName("");
    setErr(null);
    setExtraDirs([]);
    setExtraPickerOpen(false);
    setExtraPickerDirs(null);
    setExtraPickerErr(null);
    fetchTools(selectedMachine)
      .then((ts) => {
        setTools(ts);
        setTool((cur) => cur || ts[0]?.cmd || "");
      })
      .catch(() => {});
    go();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, selectedMachine]);

  useEffect(() => {
    if (!dirs) return;
    setExtraDirs((prev) => prev.filter((p) => p !== dirs.path));
  }, [dirs?.path]);

  if (!open) return null;

  const selectedTool = tools.find((t) => t.cmd === tool || t.prefix === tool);
  const extraSupport = selectedTool?.extraDirs;
  const extraLimit = extraSupport?.max || 0;
  const extraOverLimit = extraLimit > 0 && extraDirs.length > extraLimit;

  const rel = (p: string) => {
    const home = dirs?.home || extraPickerDirs?.home;
    if (!home) return p;
    if (p === home) return "~";
    if (p.startsWith(home + "/")) return "~" + p.slice(home.length);
    return p;
  };
  const here = dirs ? (dirs.atHome ? "~" : basename(dirs.path)) : "…";
  const extraHere = extraPickerDirs
    ? extraPickerDirs.atHome
      ? "~"
      : basename(extraPickerDirs.path)
    : "…";

  const openExtraPicker = () => {
    if (!dirs || !extraSupport) return;
    setExtraPickerOpen(true);
    goExtra(dirs.atHome ? undefined : dirs.parent);
  };

  const toggleExtraDir = (path: string) => {
    if (!dirs || !extraSupport) return;
    setExtraPickerErr(null);
    if (path === dirs.path) {
      setExtraPickerErr("That is the project folder");
      return;
    }
    if (!extraDirs.includes(path) && extraLimit > 0 && extraDirs.length >= extraLimit) {
      setExtraPickerErr(`${selectedTool?.prefix || "tool"} allows ${extraLimit} extra folders`);
      return;
    }
    setExtraDirs((prev) => {
      if (prev.includes(path)) return prev.filter((p) => p !== path);
      return [...prev, path];
    });
  };

  const create = async () => {
    if (!dirs || !tool) return;
    if (extraOverLimit) {
      setErr(`${selectedTool?.prefix || "tool"} allows ${extraLimit} extra folders`);
      return;
    }
    setBusy(true);
    setErr(null);
    try {
      const ref = await createSession(
        tool,
        name,
        dirs.path,
        extraSupport ? extraDirs : [],
        selectedMachine
      );
      onCreated(ref);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="absolute inset-0 z-50 flex flex-col bg-bar pt-safe">
      <div className="flex items-center gap-3 border-b border-edge px-4 py-3">
        <span className="flex-1 text-lg font-semibold text-slate-100">New session</span>
        <button
          onClick={onClose}
          className="rounded-md bg-panel px-3 py-2 text-sm text-slate-300 active:bg-edge"
        >
          ✕
        </button>
      </div>

      {/* Machine picker — only with a hub fronting >1 agent. Switching re-loads
          the tool list + dir browser for the chosen machine. */}
      {multiMachine && (
        <div className="flex items-center gap-2 border-b border-edge/60 px-3 py-2">
          <span className="text-slate-500">🖥</span>
          <span className="shrink-0 text-sm text-slate-400">Machine</span>
          <select
            value={selectedMachine}
            onChange={(e) => setSelectedMachine(e.target.value)}
            className="min-w-0 flex-1 rounded-md bg-panel px-3 py-2 text-sm text-slate-100"
          >
            {machines.map((m) => (
              <option key={m.machine} value={m.machine} disabled={!m.online}>
                {(m.hostname || m.machine) + (m.online ? "" : " (offline)")}
              </option>
            ))}
          </select>
        </div>
      )}

      {/* current folder + up + new folder */}
      <div className="flex items-center gap-2 border-b border-edge/60 px-3 py-2">
        <button
          onClick={() => dirs && go(dirs.parent)}
          disabled={!dirs || dirs.atHome}
          className="rounded-md bg-panel px-3 py-2 text-sm text-slate-300 active:bg-edge disabled:opacity-30"
        >
          ⬆︎
        </button>
        <span className="text-slate-500">📂</span>
        <span className="min-w-0 flex-1 truncate font-medium text-slate-100">{here}</span>
        <button
          onClick={() => {
            setOpErr(null);
            setNewName("");
            setCreating(true);
          }}
          className="shrink-0 rounded-md bg-panel px-3 py-2 text-sm text-slate-300 active:bg-edge"
        >
          ＋📁
        </button>
      </div>

      {opErr && <div className="border-b border-edge/40 px-4 py-2 text-sm text-red-400">{opErr}</div>}

      {creating && (
        <div className="flex items-center gap-2 border-b border-edge/40 bg-panel/40 px-3 py-2">
          <span className="text-slate-500">📁</span>
          <input
            autoFocus
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && createFolder()}
            placeholder="new folder name"
            className="min-w-0 flex-1 rounded-md border border-edge bg-bar px-2 py-2 text-sm text-slate-100 outline-none focus:border-accent"
          />
          <button
            onClick={() => {
              setCreating(false);
              setNewName("");
            }}
            className="px-2 py-2 text-slate-400"
          >
            ✕
          </button>
          <button
            onClick={createFolder}
            disabled={!newName.trim()}
            className="rounded-md bg-accent px-3 py-2 text-xs font-semibold text-bar disabled:opacity-40"
          >
            Add
          </button>
        </div>
      )}

      {/* subfolders */}
      <div className="flex-1 overflow-y-auto">
        {dirs && dirs.dirs.length === 0 && !creating && (
          <div className="px-4 py-8 text-center text-sm text-slate-500">
            No subfolders — create the session here, make a folder, or go up.
          </div>
        )}
        {dirs?.dirs.map((d) => (
          <div key={d.path} className="flex items-center border-b border-edge/40">
            <button
              data-folder={d.name}
              onClick={() => go(d.path)}
              className="flex min-w-0 flex-1 items-center gap-3 px-4 py-3 text-left active:bg-panel"
            >
              <span className="text-slate-500">📁</span>
              <span className="min-w-0 flex-1 truncate text-slate-100">{d.name}</span>
              <span className="text-slate-600">›</span>
            </button>
            {confirmDel === d.path ? (
              <div className="flex items-center gap-1 pr-2">
                <button
                  onClick={() => setConfirmDel(null)}
                  className="rounded-md px-2 py-2 text-xs text-slate-400 active:bg-edge"
                >
                  Cancel
                </button>
                <button
                  onClick={() => deleteFolder(d.path)}
                  className="rounded-md bg-red-500/80 px-3 py-2 text-xs font-semibold text-bar active:opacity-80"
                >
                  Delete
                </button>
              </div>
            ) : (
              <button
                onClick={() => {
                  setOpErr(null);
                  setConfirmDel(d.path);
                }}
                aria-label={`Delete folder ${d.name}`}
                className="px-3 py-3 text-lg text-slate-600 active:text-red-400"
              >
                🗑
              </button>
            )}
          </div>
        ))}
      </div>

      {/* create bar */}
      <div className="shrink-0 border-t border-edge bg-panel p-3 pb-safe">
        <div className="mb-2 flex gap-1.5 overflow-x-auto">
          {tools.map((t) => (
            <button
              key={t.cmd}
              onClick={() => setTool(t.cmd)}
              className={`shrink-0 rounded-full px-3 py-1.5 text-xs font-semibold uppercase tracking-wide ${
                tool === t.cmd ? `${toolColor(t.prefix)} text-bar` : "bg-bar text-slate-400"
              }`}
            >
              {t.prefix}
            </button>
          ))}
        </div>
        {extraSupport && (
          <div className="mb-2 rounded-lg border border-edge/70 bg-bar/70 px-3 py-2">
            <div className="flex items-center gap-2">
              <div className="min-w-0 flex-1">
                <div className="text-xs font-semibold uppercase tracking-wide text-slate-400">
                  Extra folders
                </div>
                <div className={`mt-0.5 text-xs ${extraOverLimit ? "text-red-400" : "text-slate-500"}`}>
                  {extraDirs.length === 0
                    ? "None"
                    : `${extraDirs.length}${extraLimit ? `/${extraLimit}` : ""} selected`}
                </div>
              </div>
              <button
                onClick={openExtraPicker}
                className="rounded-md bg-panel px-3 py-2 text-xs font-semibold text-slate-200 active:bg-edge"
              >
                {extraDirs.length ? "Manage" : "Add"}
              </button>
            </div>
            {extraDirs.length > 0 && (
              <div className="mt-2 flex gap-1.5 overflow-x-auto pb-0.5">
                {extraDirs.map((p) => (
                  <span
                    key={p}
                    className="inline-flex max-w-[13rem] shrink-0 items-center gap-1 rounded-md border border-edge bg-panel px-2 py-1 text-xs text-slate-300"
                  >
                    <span className="truncate">{rel(p)}</span>
                    <button
                      onClick={() => setExtraDirs((prev) => prev.filter((x) => x !== p))}
                      aria-label={`Remove extra folder ${rel(p)}`}
                      className="text-slate-500 active:text-red-400"
                    >
                      ✕
                    </button>
                  </span>
                ))}
              </div>
            )}
          </div>
        )}
        {err && <div className="mb-2 text-sm text-red-400">{err}</div>}
        <div className="flex gap-2">
          <input
            value={name}
            onChange={(e) => {
              nameEdited.current = true;
              setName(e.target.value);
            }}
            placeholder={here === "~" ? "session name" : here}
            className="min-w-0 flex-1 rounded-lg border border-edge bg-bar px-3 py-3 font-mono text-[15px] text-slate-100 outline-none focus:border-accent"
          />
          <button
            onClick={create}
            disabled={busy || !dirs || !tool || extraOverLimit}
            className="rounded-lg bg-accent px-4 py-3 text-sm font-semibold text-bar active:opacity-80 disabled:opacity-40"
          >
            {busy ? "…" : "Create"}
          </button>
        </div>
        <div className="mt-1.5 truncate text-xs text-slate-500">
          Creates in {dirs ? rel(dirs.path) : "…"}
          {extraSupport && extraDirs.length > 0 ? ` + ${extraDirs.length} extra` : ""}
        </div>
      </div>

      {extraPickerOpen && extraSupport && (
        <div className="absolute inset-0 z-[70] flex flex-col bg-bar pt-safe">
          <div className="flex items-center gap-3 border-b border-edge px-4 py-3">
            <span className="min-w-0 flex-1 text-lg font-semibold text-slate-100">
              Extra folders
            </span>
            <span className="rounded-md bg-panel px-2 py-1 text-xs font-semibold text-slate-400">
              {extraDirs.length}
              {extraLimit ? `/${extraLimit}` : ""}
            </span>
            <button
              onClick={() => setExtraPickerOpen(false)}
              className="rounded-md bg-panel px-3 py-2 text-sm text-slate-300 active:bg-edge"
            >
              Done
            </button>
          </div>

          <div className="flex items-center gap-2 border-b border-edge/60 px-3 py-2">
            <button
              onClick={() => extraPickerDirs && goExtra(extraPickerDirs.parent)}
              disabled={!extraPickerDirs || extraPickerDirs.atHome}
              className="rounded-md bg-panel px-3 py-2 text-sm text-slate-300 active:bg-edge disabled:opacity-30"
            >
              ⬆︎
            </button>
            <span className="text-slate-500">📂</span>
            <span className="min-w-0 flex-1 truncate font-medium text-slate-100">{extraHere}</span>
            <button
              onClick={() => extraPickerDirs && toggleExtraDir(extraPickerDirs.path)}
              disabled={!extraPickerDirs || extraPickerDirs.path === dirs?.path}
              className={`shrink-0 rounded-md px-3 py-2 text-xs font-semibold active:opacity-80 disabled:opacity-35 ${
                extraPickerDirs && extraDirs.includes(extraPickerDirs.path)
                  ? "bg-accent text-bar"
                  : "bg-panel text-slate-200"
              }`}
            >
              {extraPickerDirs?.path === dirs?.path
                ? "Project"
                : extraPickerDirs && extraDirs.includes(extraPickerDirs.path)
                  ? "Added"
                  : "Add"}
            </button>
          </div>

          {extraPickerErr && (
            <div className="border-b border-edge/40 px-4 py-2 text-sm text-red-400">
              {extraPickerErr}
            </div>
          )}

          {extraDirs.length > 0 && (
            <div className="flex gap-1.5 overflow-x-auto border-b border-edge/40 px-3 py-2">
              {extraDirs.map((p) => (
                <span
                  key={p}
                  className="inline-flex max-w-[14rem] shrink-0 items-center gap-1 rounded-md border border-edge bg-panel px-2 py-1 text-xs text-slate-300"
                >
                  <span className="truncate">{rel(p)}</span>
                  <button
                    onClick={() => setExtraDirs((prev) => prev.filter((x) => x !== p))}
                    aria-label={`Remove extra folder ${rel(p)}`}
                    className="text-slate-500 active:text-red-400"
                  >
                    ✕
                  </button>
                </span>
              ))}
            </div>
          )}

          <div className="flex-1 overflow-y-auto">
            {extraPickerDirs && extraPickerDirs.dirs.length === 0 && (
              <div className="px-4 py-8 text-center text-sm text-slate-500">
                No subfolders
              </div>
            )}
            {extraPickerDirs?.dirs.map((d) => {
              const selected = extraDirs.includes(d.path);
              const isProject = d.path === dirs?.path;
              return (
                <div key={d.path} className="flex items-center border-b border-edge/40">
                  <button
                    onClick={() => goExtra(d.path)}
                    className="flex min-w-0 flex-1 items-center gap-3 px-4 py-3 text-left active:bg-panel"
                  >
                    <span className="text-slate-500">📁</span>
                    <span className="min-w-0 flex-1 truncate text-slate-100">{d.name}</span>
                    <span className="text-slate-600">›</span>
                  </button>
                  <button
                    onClick={() => toggleExtraDir(d.path)}
                    disabled={isProject}
                    aria-label={`${selected ? "Remove" : "Add"} extra folder ${d.name}`}
                    className={`mr-2 min-w-12 rounded-md px-3 py-2 text-xs font-semibold active:opacity-80 disabled:opacity-35 ${
                      selected ? "bg-accent text-bar" : "bg-panel text-slate-200"
                    }`}
                  >
                    {isProject ? "Project" : selected ? "Added" : "Add"}
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
