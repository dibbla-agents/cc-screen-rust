// Proposal 0091 — the Update dialog can replace the cc-screen agent itself.
// Drives the real component against a mocked api module: the "also update
// cc-screen" box appears only for a machine whose agent can do it, the request
// carries `updateAgent`, the few seconds the machine is offline while its new
// agent starts are not shown as an error, and the agent row reports the result.

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MachineInfo, UpdateJob } from "../api";
import UpdateAssistants from "./UpdateAssistants";

vi.mock("../api", async (importOriginal) => {
  const mod = await importOriginal<typeof import("../api")>();
  return {
    ...mod,
    fetchUpdateJob: vi.fn(),
    fetchTools: vi.fn().mockResolvedValue([]),
    fetchInstallPlan: vi.fn().mockResolvedValue({ items: [] }),
    startAssistantUpdate: vi.fn(),
  };
});
import { fetchUpdateJob, startAssistantUpdate } from "../api";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const idle: UpdateJob = { phase: "idle" };
const agentRow = (state: string, extra: Partial<NonNullable<UpdateJob["agent"]>> = {}) => ({
  tool: "cc-screen",
  label: "cc-screen agent",
  state,
  from: "0.5.9",
  ...extra,
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "setTimeout", "clearTimeout", "Date"] });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
  vi.clearAllMocks();
});

async function open(machines: MachineInfo[]) {
  await act(async () =>
    root.render(
      <UpdateAssistants open machines={machines} sessions={[]} onClose={() => {}} onDone={() => {}} />
    )
  );
  // Let the per-machine probes resolve.
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

const button = (label: RegExp) =>
  [...container.querySelectorAll("button")].find((b) => label.test(b.textContent ?? ""))!;

describe("UpdateAssistants — the agent itself (proposal 0091)", () => {
  it("offers to update cc-screen only where the agent can, and asks for it", async () => {
    vi.mocked(fetchUpdateJob).mockResolvedValue(idle);
    vi.mocked(startAssistantUpdate).mockResolvedValue({ id: "j1", phase: "updating", agent: agentRow("pending") });
    await open([
      { machine: "mac-studio", hostname: "mac-studio", online: true, agentVersion: "0.5.9", agentUpdate: true },
      { machine: "pine", hostname: "pine", online: true, agentVersion: "0.5.9" },
    ]);

    expect(container.textContent).toContain("Also update cc-screen itself on 1 of 2 machines");
    expect(container.textContent).toContain("cc-screen 0.5.9");

    await act(async () => button(/Update & restart/).click());
    const calls = vi.mocked(startAssistantUpdate).mock.calls;
    const byMachine = Object.fromEntries(calls.map(([m, o]) => [m, o?.updateAgent]));
    expect(byMachine["mac-studio"]).toBe(true);
    // Never asked of an agent that can't — the hub would 501.
    expect(byMachine["pine"]).toBe(false);
  });

  it("has no agent box when no machine's agent can update itself", async () => {
    vi.mocked(fetchUpdateJob).mockResolvedValue(idle);
    await open([{ machine: "pine", hostname: "pine", online: true }]);
    expect(container.textContent).not.toContain("Also update cc-screen itself");
    expect(container.textContent).toContain("Update coding assistants");
  });

  it("rides out the restart gap and shows the version change", async () => {
    vi.mocked(fetchUpdateJob).mockResolvedValue(idle);
    vi.mocked(startAssistantUpdate).mockResolvedValue({ id: "j1", phase: "updating", agent: agentRow("pending") });
    await open([{ machine: "mac-studio", hostname: "mac-studio", online: true, agentUpdate: true }]);
    await act(async () => button(/Update & restart/).click());

    // The old agent reports it is restarting into the new release…
    vi.mocked(fetchUpdateJob).mockResolvedValueOnce({
      id: "j1",
      phase: "updating",
      agent: agentRow("restarting", { to: "0.5.10" }),
    });
    await act(async () => {
      vi.advanceTimersByTime(1500);
    });
    expect(container.textContent).toContain("restarting into 0.5.10");

    // …then the machine is unreachable for a few polls. Not an error.
    vi.mocked(fetchUpdateJob).mockRejectedValue(new Error("machine offline"));
    for (let i = 0; i < 4; i++) {
      await act(async () => {
        vi.advanceTimersByTime(1500);
      });
    }
    expect(container.textContent).not.toContain("machine offline");

    // The new agent comes back and finishes the same job.
    vi.mocked(fetchUpdateJob).mockResolvedValue({
      id: "j1",
      phase: "done",
      agent: agentRow("updated", { to: "0.5.10" }),
    });
    await act(async () => {
      vi.advanceTimersByTime(1500);
    });
    expect(container.textContent).toContain("0.5.9 → 0.5.10");
    expect(container.textContent).toContain("Finished.");
  });

  it("says so when the machine never comes back", async () => {
    vi.mocked(fetchUpdateJob).mockResolvedValue(idle);
    vi.mocked(startAssistantUpdate).mockResolvedValue({ id: "j1", phase: "updating", agent: agentRow("pending") });
    await open([{ machine: "mac-studio", hostname: "mac-studio", online: true, agentUpdate: true }]);
    await act(async () => button(/Update & restart/).click());
    vi.mocked(fetchUpdateJob).mockResolvedValueOnce({
      id: "j1",
      phase: "updating",
      agent: agentRow("restarting", { to: "0.5.10" }),
    });
    await act(async () => {
      vi.advanceTimersByTime(1500);
    });
    vi.mocked(fetchUpdateJob).mockRejectedValue(new Error("machine offline"));
    await act(async () => {
      vi.advanceTimersByTime(125_000);
    });
    expect(container.textContent).toContain("hasn’t come back since its agent restarted");
  });
});
