// Choreography for the C3 clip (proposal 0061): the activation handshake,
// end-to-end and REAL — a pending device code is minted over the same HTTP
// the box's --enroll would use, typed on /activate, approved, and then a real
// agent process (harebell) picks the rotated uplink token up and dials in so
// the dashboard dot visibly flips online. Nothing in frame is mocked.
//
// The only runtime shim: the dashboard's 8 s live-status poll is clamped to
// 2 s (pure pacing — the data flow is untouched) so the flip lands inside the
// 15 s clip budget instead of 8 s of dead air.
//
// Side effects (the scene is restored around the run):
//   .auth/uplink-harebell.json  overwritten — approving rotates the row token
//   /tmp/ccs-harebell-home      scratch $HOME for the short-lived agent
//   out/harebell-agent.pid      the agent's pid; it is killed ~5 s after the
//                               driver returns (a timer outlives the recording
//                               so the dot can't flip back on camera), leaving
//                               harebell offline again. Verify with stage.mjs.

import { mkdirSync, openSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const HUB = process.env.CAPTURE_HUB || "https://app.ccscreen.dev";
const MACHINE = "harebell";
const AGENT_BIN =
  process.env.CAPTURE_AGENT_BIN || "/tmp/ccs-0061-target/release/cc-screen-rust";
const AGENT_HOME = "/tmp/ccs-harebell-home";

async function post(path, body) {
  const res = await fetch(`${HUB}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`POST ${path} → ${res.status} ${await res.text()}`);
  return res.json();
}

export default async (page, _ctx) => {
  // A real pending device code, minted exactly as the box would (no auth).
  const code = await post("/api/device/code", {
    device_id: randomBytes(16).toString("hex"),
    machine_id: MACHINE,
  });

  // Pacing shim: the dashboard polls /api/agents on an 8 s interval; clamp it
  // to 2 s so the (real) online flip fits the clip budget.
  await page.evaluate(() => {
    const orig = window.setInterval.bind(window);
    window.setInterval = (fn, ms, ...args) =>
      orig(fn, ms === 8000 ? 2000 : ms, ...args);
  });

  // Type the code like a human would, then approve.
  await page.waitForTimeout(700);
  const input = page.locator("input").first();
  await input.click();
  await input.pressSequentially(code.user_code, { delay: 120 });
  await page.waitForTimeout(500);
  await page.locator('button[type="submit"]', { hasText: "Approve" }).click();
  await page.getByText(`${MACHINE} connected`).waitFor({ timeout: 8000 });

  // The row token just rotated — persist it for the staging tooling, exactly
  // as the box's polling loop would collect it.
  const token = await post("/api/device/token", { device_code: code.device_code });
  if (!token.uplink_token) throw new Error(`device/token: ${JSON.stringify(token)}`);
  writeFileSync(
    join(HERE, "..", ".auth", `uplink-${MACHINE}.json`),
    JSON.stringify({ machine: MACHINE, ...token }, null, 2),
  );

  // Let the success card read, then pan to the machines dashboard.
  await page.waitForTimeout(1100);
  await page.getByText("Go to my machines").click();
  await page.locator(`li:has-text("${MACHINE}")`).first().waitFor({ timeout: 8000 });
  await page.waitForTimeout(1600); // the offline (grey-dot) beat

  // Start the real agent with the token the approval just issued.
  mkdirSync(AGENT_HOME, { recursive: true });
  const log = openSync(join(HERE, "..", "out", "harebell-agent.log"), "w");
  const child = spawn(AGENT_BIN, ["--hub-only", "--machine-id", MACHINE], {
    env: {
      HOME: AGENT_HOME,
      CCWEB_HOME: AGENT_HOME,
      PATH: "/usr/bin:/bin",
      CCWEB_HUB_URL: HUB,
      CCWEB_HUB_TOKEN: token.uplink_token,
    },
    detached: true,
    stdio: ["ignore", log, log],
  });
  writeFileSync(join(HERE, "..", "out", "harebell-agent.pid"), String(child.pid));

  // The dot flips online (bg-amber mt-dot-on) on the next 2 s poll.
  await page
    .locator(`li:has-text("${MACHINE}") .mt-dot-on`)
    .waitFor({ timeout: 12000 });
  await page.waitForTimeout(2200); // hold the settled online state

  // Kill the agent only after the recording has closed (the runner's settle +
  // ctx.close take ~2 s) so a poll can't flip the dot back on camera. harebell
  // must END the run offline — the scene is offline-by-contract.
  setTimeout(() => {
    try {
      child.kill("SIGTERM");
    } catch {
      /* already gone */
    }
  }, 5000);
};
