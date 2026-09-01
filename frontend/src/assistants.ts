// Built-in assistant display/consent catalogue (proposal 0088).
//
// The Rust registry remains authoritative for launch/install behavior and the
// create picker stays driven by /api/tools. These six rows are the bounded PWA
// duplicate needed before the Add-machine shorthand `--assistants` may mean
// "all": every registry assistant must be visible consent in the same build.
export const BUILTIN_ASSISTANTS = [
  { prefix: "claude", label: "Claude Code", shortLabel: "Claude", color: "bg-claude" },
  { prefix: "codex", label: "Codex CLI", shortLabel: "Codex", color: "bg-codex" },
  { prefix: "gemini", label: "Gemini CLI", shortLabel: "Gemini", color: "bg-gemini" },
  { prefix: "kimi", label: "Kimi CLI", shortLabel: "Kimi", color: "bg-kimi" },
  { prefix: "opencode", label: "OpenCode", shortLabel: "OpenCode", color: "bg-opencode" },
  { prefix: "grok", label: "Grok", shortLabel: "Grok", color: "bg-grok" },
] as const;

export type BuiltinAssistantPrefix = (typeof BUILTIN_ASSISTANTS)[number]["prefix"];

export const BUILTIN_ASSISTANT_PREFIXES = BUILTIN_ASSISTANTS.map(
  ({ prefix }) => prefix
) as BuiltinAssistantPrefix[];

const byPrefix = new Map<string, (typeof BUILTIN_ASSISTANTS)[number]>(
  BUILTIN_ASSISTANTS.map((assistant) => [assistant.prefix, assistant])
);

export function assistantLabel(prefix: string): string {
  return byPrefix.get(prefix)?.label ?? prefix;
}

export function assistantShortLabel(prefix: string): string {
  return byPrefix.get(prefix)?.shortLabel ?? prefix;
}

export function assistantColor(prefix: string): string | undefined {
  return byPrefix.get(prefix)?.color;
}

export interface AssistantInstallSelection {
  shellArg: string;
  query: string;
}

// Keep explicit subsets in catalogue order even after a checkbox is toggled off
// and back on. Bare `--assistants` / `assistants=all` is emitted only when every
// one of the six visible choices is selected.
export function assistantInstallSelection(
  enabled: boolean,
  picked: readonly string[]
): AssistantInstallSelection {
  if (!enabled) return { shellArg: "", query: "" };

  const selected = BUILTIN_ASSISTANT_PREFIXES.filter((prefix) => picked.includes(prefix));
  if (selected.length === BUILTIN_ASSISTANT_PREFIXES.length) {
    return { shellArg: "--assistants", query: "&assistants=all" };
  }
  if (selected.length === 0) return { shellArg: "", query: "" };

  const value = selected.join(",");
  return {
    shellArg: `--assistants=${value}`,
    query: `&assistants=${encodeURIComponent(value)}`,
  };
}
