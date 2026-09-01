import { describe, expect, it } from "vitest";
import {
  assistantColor,
  assistantInstallSelection,
  assistantLabel,
  assistantShortLabel,
  BUILTIN_ASSISTANTS,
  BUILTIN_ASSISTANT_PREFIXES,
} from "./assistants";

describe("built-in assistant catalogue (proposal 0088/0089)", () => {
  it("makes all six visible in stable consent order", () => {
    expect(BUILTIN_ASSISTANT_PREFIXES).toEqual([
      "claude",
      "codex",
      "gemini",
      "kimi",
      "opencode",
      "grok",
    ]);
    expect(BUILTIN_ASSISTANTS.at(-1)).toEqual({
      prefix: "grok",
      label: "Grok",
      shortLabel: "Grok",
      color: "bg-grok",
    });
  });

  it("provides full/short labels and static Tailwind colours with honest fallbacks", () => {
    expect(assistantLabel("claude")).toBe("Claude Code");
    expect(assistantShortLabel("codex")).toBe("Codex");
    expect(assistantLabel("opencode")).toBe("OpenCode");
    expect(assistantColor("opencode")).toBe("bg-opencode");
    expect(assistantLabel("grok")).toBe("Grok");
    expect(assistantColor("grok")).toBe("bg-grok");
    expect(assistantLabel("my-tool")).toBe("my-tool");
    expect(assistantShortLabel("my-tool")).toBe("my-tool");
    expect(assistantColor("my-tool")).toBeUndefined();
  });

  it("uses shorthand only for all six visible choices", () => {
    expect(assistantInstallSelection(true, BUILTIN_ASSISTANT_PREFIXES)).toEqual({
      shellArg: "--assistants",
      query: "&assistants=all",
    });
    expect(assistantInstallSelection(true, BUILTIN_ASSISTANT_PREFIXES.slice(0, 5))).toEqual({
      shellArg: "--assistants=claude,codex,gemini,kimi,opencode",
      query: "&assistants=claude%2Ccodex%2Cgemini%2Ckimi%2Copencode",
    });
  });

  it("normalizes toggled subsets and represents disabled/empty consent as no argument", () => {
    expect(assistantInstallSelection(true, ["opencode", "claude", "codex"])).toEqual({
      shellArg: "--assistants=claude,codex,opencode",
      query: "&assistants=claude%2Ccodex%2Copencode",
    });
    expect(assistantInstallSelection(false, BUILTIN_ASSISTANT_PREFIXES)).toEqual({
      shellArg: "",
      query: "",
    });
    expect(assistantInstallSelection(true, [])).toEqual({ shellArg: "", query: "" });
  });
});
