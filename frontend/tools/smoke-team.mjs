// Headless end-to-end smoke test of the TEAM tier UI (proposal 0065 Part F),
// modeled on smoke.mjs: same phone viewport recipe, same fail-on-any-
// console/page-error discipline.
//
// ┌────────────────────────────────────────────────────────────────────────┐
// │ REQUIRES THE BUILT TEAM FRONTEND (0065 Parts B–D: team chip, TeamCard, │
// │ seats meter, seat picker). Do NOT wire this into CI until that UI has  │
// │ landed in frontend/dist and the hub embedding it is rebuilt — against  │
// │ an older bundle every team assertion below fails by construction.     │
// └────────────────────────────────────────────────────────────────────────┘
//
// Unlike smoke.mjs (which targets a running single-tenant hub), this script
// stands up its OWN local multi-tenant hub (no Stripe) and seeds it over the
// same HTTP API scripts/e2e-team.sh exercises: two users (A owner, B member)
// in one 3-seat org, one machine enrolled for A so B holds a team-visibility
// row. Then, in a phone viewport:
//
//   as B (member):  the team-visibility grant is asserted at the API seam
//                   (GET /api/shares/received carries the origin:"team" row) —
//                   the drawer's machine rows/chips only render for machines
//                   with a LIVE agent in the hub roster, and the CLI-enrolled
//                   seed machine never connects one, so the drawer honestly
//                   shows no machine row to badge. The UI surface that row
//                   drives regardless is the ~/shared card's "via your team"
//                   summary. The dashboard shows the ~/team window with the
//                   member list, the PlanCard seats-meter text ("2 / 3"), and
//                   NO admin actions (the member side of the split);
//   as A (owner):   the TeamCard invite form's success state shows a copyable
//                   /org-invite/ link; the seat picker cannot step below 3
//                   (only when billing is configured — without Stripe the
//                   seats-purchase UI deliberately doesn't render).
//
// Env:
//   HUB_BIN   path to a multi-tenant cc-screen-hub binary
//             (default ../target/debug/cc-screen-hub — build with
//              `cargo build -p cc-screen-hub --features multi-tenant`)
//   PORT      hub port (default 18874)
//   CHROME    chromium executable for Playwright (as smoke.mjs)
import { chromium } from "playwright";
import { spawn, execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const hubBin = process.env.HUB_BIN || join(repo, "target", "debug", "cc-screen-hub");
const port = Number(process.env.PORT || 18874);
const base = `http://127.0.0.1:${port}`;

const tmp = mkdtempSync(join(tmpdir(), "smoke-team-"));
const db = join(tmp, "hub.db");
const ts = Date.now();
const pw = `smoke-team-pw-${ts}`;
const A = `smoke-a-${ts}@ccscreen.test`; // org owner
const B = `smoke-b-${ts}@ccscreen.test`; // member
const orgName = `smoke-team-${ts}`;
const machine = `smoke-m1-${ts}`;

// ── seed: start the hub and drive the same API e2e-team.sh uses ─────────────
const hubEnv = {
  ...process.env,
  CCHUB_DATABASE_URL: `sqlite://${db}`,
  CCWEB_CONFIG_DIR: join(tmp, "hub-config"),
  CCHUB_PUBLIC_URL: base,
};
// Team must work without Stripe (0064 graceful absence) — scrub any leaked keys.
for (const k of Object.keys(hubEnv)) if (k.startsWith("STRIPE_")) delete hubEnv[k];
const hub = spawn(hubBin, ["--addr", `127.0.0.1:${port}`], { env: hubEnv, stdio: "ignore" });

function cleanup() {
  try { hub.kill(); } catch {}
  rmSync(tmp, { recursive: true, force: true });
}
process.on("exit", cleanup);

async function waitReady() {
  for (let i = 0; i < 60; i++) {
    try {
      const r = await fetch(`${base}/api/me`);
      if (r.ok) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`hub never answered on ${base}`);
}

// Minimal per-user cookie jar over fetch (the hub's session cookie).
function makeUser() {
  return { cookie: "" };
}
async function api(user, method, path, body) {
  const r = await fetch(base + path, {
    method,
    headers: {
      ...(body ? { "content-type": "application/json" } : {}),
      ...(user.cookie ? { cookie: user.cookie } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const set = r.headers.getSetCookie?.() ?? [r.headers.get("set-cookie")].filter(Boolean);
  if (set.length) user.cookie = set.map((c) => c.split(";")[0]).join("; ");
  let json = null;
  try { json = await r.clone().json(); } catch {}
  return { status: r.status, json };
}
function must(cond, msg) {
  if (!cond) throw new Error("seed failed: " + msg);
}

await waitReady();
const ua = makeUser();
const ub = makeUser();
must((await api(ua, "POST", "/api/signup", { email: A, password: pw })).status === 200, `signup ${A}`);
must((await api(ub, "POST", "/api/signup", { email: B, password: pw })).status === 200, `signup ${B}`);
must((await api(ua, "POST", "/api/orgs", { name: orgName })).status === 200, "org create");
// A machine for A, so B's drawer/received list has a team row to badge.
execFileSync(hubBin, ["user", "agent", A, machine], { env: hubEnv });
// 3 seats via the admin CLI — the no-Stripe path (0063 B4).
execFileSync(hubBin, ["org", "seats", orgName, "3"], { env: hubEnv });
must((await api(ua, "POST", "/api/orgs/invites", { email: B })).status === 200, "invite B");
const inbox = await api(ub, "GET", "/api/orgs/invites/inbox");
const invId = inbox.json?.[0]?.id;
must(invId, "B inbox has the invite");
must((await api(ub, "POST", `/api/orgs/invites/${invId}/accept`)).status === 200, "B accept");
const meB = await api(ub, "GET", "/api/me");
must(meB.json?.plan?.name === "team", "B is pool-governed (plan=team)");
const billingOn = meB.json?.billing === true; // false on this Stripe-less hub
// The team-visibility row itself (0065 Part B), at the API seam: B received a
// materialized grant on A's machine, kind/origin "team". This is the data the
// drawer chip renders from — but the chip's home (a machine row) only exists
// for machines with a live connected agent, which the CLI-enrolled seed
// machine is not, so the row is asserted here and its UI stand-in (the
// ~/shared "via your team" summary) in the browser pass below.
const recvB = await api(ub, "GET", "/api/shares/received");
const teamRow = (recvB.json ?? []).find((r) => r.origin === "team");
must(teamRow, "B's received list carries a team-visibility row");
must(teamRow.kind === "team", "team row kind=team");
must(teamRow.ownerEmail === A, "team row is owned by A");
must(teamRow.orgName === orgName, "team row carries the org name");

// ── Playwright: the phone viewport recipe, verbatim from smoke.mjs ──────────
const browser = await chromium.launch({
  executablePath: process.env.CHROME,
  headless: true,
  args: ["--no-sandbox", "--disable-gpu"],
});

let failed = false;
function fail(msg) {
  console.error("SMOKE-TEAM FAIL:", msg);
  failed = true;
}

// A context signed in as the given seeded user: the API session cookie is
// injected directly (login-form UI is smoke.mjs's territory, not this pass's).
async function signedInPage(user, errors) {
  const ctx = await browser.newContext({
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true,
  });
  await ctx.addCookies(
    user.cookie.split("; ").map((c) => {
      const eq = c.indexOf("=");
      return { name: c.slice(0, eq), value: c.slice(eq + 1), url: base };
    })
  );
  const page = await ctx.newPage();
  page.on("console", (m) => {
    if (m.type() !== "error") return;
    // One known-benign resource error: the drawer's restorable probe. With
    // ZERO connected agents (this seed enrolls a machine but never runs one)
    // the hub's route() has no owning agent and answers 404; the app catches
    // it (fetchRestorable(...).catch(() => [])) — pre-existing fleet-offline
    // behavior, not a team-tier regression. Everything else still fails.
    const url = m.location()?.url ?? "";
    if (m.text().startsWith("Failed to load resource") && url.includes("/api/sessions/restorable"))
      return;
    errors.push("console: " + m.text() + (url ? ` (${url})` : ""));
  });
  page.on("pageerror", (e) => errors.push("pageerror: " + e.message));
  return { ctx, page };
}

// The phone app auto-opens the session drawer when no pane has a session
// (App.tsx: !isDesktop && currentSession === null). Wait for it (fall back to
// the header button if a future change drops the auto-open), then close it via
// its own ✕ so the footer / dashboard entry is unobstructed.
async function openDashboard(page) {
  try {
    await page.getByText("Sessions", { exact: true }).waitFor({ timeout: 5000 });
  } catch {
    await page.getByRole("button", { name: "Open sessions" }).click();
    await page.getByText("Sessions", { exact: true }).waitFor({ timeout: 5000 });
  }
  // The header ✕ (aria-label "Close") — getByLabel, since the empty-state also
  // renders a text "Close" button and getByRole would match both.
  await page.getByLabel("Close", { exact: true }).click();
  await page
    .getByText("Sessions", { exact: true })
    .waitFor({ state: "detached", timeout: 5000 });
  // Footer (phone): "Your machines & account" → the full-screen Dashboard.
  await page.getByRole("button", { name: "Your machines & account" }).click();
  await page.getByText("~/team", { exact: false }).first().waitFor({ timeout: 8000 });
}

try {
  // ── Pass 1 — B, the plain member ──────────────────────────────────────────
  const errsB = [];
  const { ctx: ctxB, page: pageB } = await signedInPage(ub, errsB);
  await pageB.goto(base, { waitUntil: "networkidle" });

  // The drawer auto-opens (no session). Honest drawer state for this seed: the
  // hub roster (/api/machines) only lists machines whose agent has connected,
  // so the CLI-enrolled machine renders NO row here — nothing to chip. The
  // build-aware empty state still offers "New session…".
  await pageB.getByText("Sessions", { exact: true }).waitFor({ timeout: 8000 })
    .catch(() => fail("member drawer never opened"));
  if ((await pageB.getByText(/New session/).count()) === 0)
    fail("member drawer shows no New session action (empty state broken)");

  // The dashboard: the ~/team window (TeamCard) with the member list.
  await openDashboard(pageB).catch(() => fail("member could not open the dashboard's ~/team window"));
  for (const email of [A, B]) {
    if ((await pageB.getByText(email, { exact: false }).count()) === 0)
      fail(`TeamCard member list is missing ${email}`);
  }
  // Seats meter: 2 members of 3 seats → "2 / 3". On a billing hub that's the
  // PlanCard's Seats bar; the PlanCard renders NOTHING when Stripe isn't
  // configured (0058 graceful absence), so there the TeamCard's members
  // counter ("2 / 3") and its "2 members · 3 seats" line are the meter.
  if ((await pageB.getByText(/2\s*\/\s*3/).count()) === 0)
    fail('seats meter does not show "2 / 3"');
  if (billingOn) {
    if ((await pageB.getByText("Seats", { exact: true }).count()) === 0)
      fail("PlanCard shows no Seats meter label");
    // Billing is owner/admin-only — the member gets the pointer note instead.
    if ((await pageB.getByText(/Billing is managed by/).count()) === 0)
      fail('member B misses the "Billing is managed by" note');
  } else {
    if ((await pageB.getByText(/2 members · 3 seats/).count()) === 0)
      fail('TeamCard does not show "2 members · 3 seats"');
    // 0058 graceful absence: no plan/billing card at all without Stripe.
    if ((await pageB.getByText("~/plan", { exact: false }).count()) > 0)
      fail("PlanCard rendered on a hub with billing:false");
  }
  // The team-visibility row's UI stand-in (~/shared card): team-materialized
  // grants are summarized, not listed ("via your team: 1 machine from 1
  // teammate — managed in ~/team").
  if ((await pageB.getByText(/via your team/).count()) === 0)
    fail('~/shared card shows no "via your team" visibility summary');
  // Admin-vs-member split: a plain member gets NO invite form, no role
  // controls, and no billing management (owner/admin-only, 0063 B3).
  if ((await pageB.getByRole("button", { name: /invite/i }).count()) > 0)
    fail("member B sees an invite action (admin-vs-member split broken)");
  if ((await pageB.getByRole("combobox", { name: /Role for/ }).count()) > 0)
    fail("member B sees role controls (admin-vs-member split broken)");
  if ((await pageB.getByRole("button", { name: /Manage billing/ }).count()) > 0)
    fail("member B sees Manage billing (owner/admin-only)");
  if (errsB.length) fail("member pass JS errors: " + errsB.join("; "));
  await ctxB.close();

  // ── Pass 2 — A, the owner ─────────────────────────────────────────────────
  const errsA = [];
  const { ctx: ctxA, page: pageA } = await signedInPage(ua, errsA);
  await pageA.goto(base, { waitUntil: "networkidle" });
  await openDashboard(pageA).catch(() => fail("owner dashboard shows no ~/team window"));

  // Invite form success state: submit an address with no account → the
  // copyable /org-invite/ link is the delivery channel (the hub sends no mail).
  const inviteAddr = `smoke-c-${ts}@ccscreen.test`;
  try {
    await pageA.getByPlaceholder("name@example.com").fill(inviteAddr);
    await pageA.getByRole("button", { name: "Invite", exact: true }).click();
    await pageA.getByText(/\/org-invite\//).first().waitFor({ timeout: 8000 });
    await pageA.getByRole("button", { name: "Copy link" }).waitFor({ timeout: 4000 });
  } catch {
    fail("invite form success state never showed a copyable /org-invite/ link");
  }

  // Seat picker floor: only rendered when Stripe billing is configured — on
  // this Stripe-less hub the purchase UI must be absent (graceful absence).
  if (billingOn) {
    try {
      const seats = pageA.locator('input[type="number"], [data-seats]').first();
      await seats.waitFor({ timeout: 8000 });
      await seats.fill("1");
      const v = await seats.inputValue();
      if (Number(v) < 3) fail(`seat picker accepted ${v} (< the 3-seat floor)`);
    } catch {
      fail("seat picker not found on a billing-enabled hub");
    }
  } else {
    // Stripe-less: no seat purchase UI should render at all (0064 graceful
    // absence) — no SeatPicker stepper, no per-seat price copy.
    const stepper =
      (await pageA.getByRole("button", { name: "Fewer seats" }).count()) +
      (await pageA.getByRole("button", { name: "More seats" }).count());
    const priceCopy = await pageA.getByText(/\/seat\/(mo|yr)/).count();
    if (stepper > 0 || priceCopy > 0)
      fail("seat purchase UI rendered on a hub with billing:false");
  }
  if (errsA.length) fail("owner pass JS errors: " + errsA.join("; "));
  await ctxA.close();

  if (!failed)
    console.log(
      "SMOKE-TEAM PASS (team row + ~/shared summary, TeamCard, seats meter, invite link, action split)"
    );
} catch (e) {
  fail(e.message);
} finally {
  await browser.close();
  cleanup();
  process.exit(failed ? 1 : 0);
}
