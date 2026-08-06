// Demo-account staging (proposal 0061 A4).
//
//   node stage.mjs               assert/repair the staged scene, refresh login
//   node stage.mjs --check       assert only, change nothing
//   node stage.mjs --enroll M    run the device flow to enroll machine M
//                                (prints the uplink token — feed it to the
//                                demo agent as CCWEB_HUB_TOKEN)
//
// The demo account is the ONLY identity that ever appears in a captured
// frame. The scene it asserts, stable across runs:
//   machines  pine      Linux, online (a real agent process holds the uplink)
//             harebell  Windows, offline (enrolled once, never connects —
//                       enrollment is pure HTTP, see --enroll)
//   sessions  on pine: claude/web-app "Web app" (teal),
//             claude/docs-site "Docs site" (violet), shell/deploy (slate)
//
// Credentials: CAPTURE_DEMO_EMAIL / CAPTURE_DEMO_PASSWORD env, or
// .auth/credentials.json {"email":…,"password":…} — .auth/ is gitignored.
// On success the login is persisted as .auth/state.json in Playwright
// storageState shape, which capture.mjs feeds to auth:'demo' entries.

import {
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { randomBytes } from "node:crypto";

const HERE = dirname(fileURLToPath(import.meta.url));
const AUTH_DIR = join(HERE, ".auth");
const HUB = process.env.CAPTURE_HUB || "https://app.ccscreen.dev";
const HOST = new URL(HUB).host;

const CHECK = process.argv.includes("--check");
const enrollIdx = process.argv.indexOf("--enroll");
const ENROLL = enrollIdx >= 0 ? process.argv[enrollIdx + 1] : null;

// The scene. Session identity is `<toolPrefix>-<short>`; label is display-only.
const MACHINES = [
  { machine: "pine", online: true },
  { machine: "harebell", online: false },
];
// Dirs must be absolute — the agent's confine guard rejects relative paths
// (the web UI always sends the absolute path the dir search returned).
const DEMO_HOME = process.env.CAPTURE_DEMO_AGENT_HOME || "/home/erik/ccs-demo-home";
const SESSIONS = [
  { machine: "pine", tool: "claude", short: "web-app", name: "web-app", dir: `${DEMO_HOME}/web-app`, label: "Web app", color: "teal" },
  { machine: "pine", tool: "claude", short: "docs-site", name: "docs-site", dir: `${DEMO_HOME}/docs-site`, label: "Docs site", color: "violet" },
  { machine: "pine", tool: "shell", short: "deploy", name: "deploy", dir: `${DEMO_HOME}/web-app`, label: null, color: "slate" },
];

function credentials() {
  const file = join(AUTH_DIR, "credentials.json");
  const fromFile = existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : {};
  const email = process.env.CAPTURE_DEMO_EMAIL || fromFile.email;
  const password = process.env.CAPTURE_DEMO_PASSWORD || fromFile.password;
  if (!email || !password) {
    console.error(
      "no demo credentials: set CAPTURE_DEMO_EMAIL/CAPTURE_DEMO_PASSWORD or write .auth/credentials.json",
    );
    process.exit(2);
  }
  return { email, password };
}

let cookie = null;
async function api(method, path, body, opts = {}) {
  const res = await fetch(`${HUB}${path}`, {
    method,
    headers: {
      ...(body ? { "content-type": "application/json" } : {}),
      ...(cookie ? { cookie } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const setCookie = res.headers.get("set-cookie");
  if (setCookie?.startsWith("ccs_session=") && !setCookie.includes("Max-Age=0"))
    cookie = setCookie.split(";")[0];
  if (!res.ok && !opts.okStatuses?.includes(res.status))
    throw new Error(`${method} ${path} → ${res.status} ${await res.text()}`);
  const text = await res.text();
  return { status: res.status, json: text ? JSON.parse(text) : null };
}

async function login() {
  const { email, password } = credentials();
  // Signup first; 409 means the account exists (the API deliberately doesn't
  // distinguish) — fall through to login. Password field is named `secret`.
  const s = await api("POST", "/api/signup", { email, password }, { okStatuses: [409, 403, 429] });
  if (!cookie) await api("POST", "/api/login", { email, secret: password });
  if (!cookie) throw new Error("login gave no session cookie");
  const me = (await api("GET", "/api/me")).json;
  if (!me.authenticated) throw new Error("/api/me says unauthenticated");
  console.log(
    `✓ logged in as ${me.email} (plan ${me.plan?.name}: ${me.plan?.agents}/${me.plan?.maxAgents} machines, cap ${me.plan?.maxSessions} sessions)`,
  );
  // Persist as Playwright storageState so capture.mjs can reuse the login.
  const [, value] = cookie.split("=");
  mkdirSync(AUTH_DIR, { recursive: true });
  writeFileSync(
    join(AUTH_DIR, "state.json"),
    JSON.stringify(
      {
        cookies: [
          {
            name: "ccs_session",
            value,
            domain: HOST,
            path: "/",
            expires: Math.floor(Date.now() / 1000) + 13 * 86400,
            httpOnly: true,
            secure: true,
            sameSite: "Lax",
          },
        ],
        origins: [],
      },
      null,
      2,
    ),
  );
  return me;
}

// RFC-8628 device flow, both sides: request a code as the box would, approve
// it as the logged-in browser would, poll the token. The machine row persists
// in the DB whether or not an agent ever connects — that's how `harebell`
// stays a stable offline Windows machine.
async function enroll(machineId) {
  const deviceId = randomBytes(16).toString("hex");
  const code = (
    await api("POST", "/api/device/code", { device_id: deviceId, machine_id: machineId })
  ).json;
  console.log(`  device code ${code.user_code} → approving as the demo account…`);
  const approve = await api(
    "POST",
    "/api/device/approve",
    { user_code: code.user_code },
    { okStatuses: [402] },
  );
  if (approve.status === 402)
    throw new Error(`machine cap reached — unlink something first (${JSON.stringify(approve.json)})`);
  const token = (
    await api("POST", "/api/device/token", { device_code: code.device_code })
  ).json;
  if (!token.uplink_token) throw new Error(`token poll: ${JSON.stringify(token)}`);
  const out = join(AUTH_DIR, `uplink-${machineId}.json`);
  writeFileSync(out, JSON.stringify({ machine: machineId, ...token }, null, 2));
  console.log(`✓ enrolled ${machineId}; uplink token saved to ${out}`);
  console.log(
    `  run the demo agent with: CCWEB_HUB_URL=${HUB} CCWEB_HUB_TOKEN=<token> cc-screen-rust --hub-only --machine-id ${machineId}`,
  );
}

async function stage() {
  const problems = [];
  const agents = (await api("GET", "/api/agents")).json;
  for (const want of MACHINES) {
    const got = agents.find((a) => a.machine === want.machine);
    if (!got) {
      problems.push(
        `machine '${want.machine}' not enrolled — run: node stage.mjs --enroll ${want.machine}`,
      );
      continue;
    }
    if (got.online !== want.online)
      problems.push(
        `machine '${want.machine}' is ${got.online ? "online" : "offline"}, expected ${want.online ? "online (start the demo agent)" : "offline (stop its agent)"}`,
      );
    console.log(
      `✓ machine ${want.machine}: ${got.online ? "online" : "offline"}${got.missing?.length ? ` (missing: ${got.missing.join(", ")})` : ""}`,
    );
  }

  const sessions = (await api("GET", "/api/sessions")).json;
  const wantNames = new Set();
  for (const want of SESSIONS) {
    const got = sessions.find(
      (s) => s.machine === want.machine && s.short === want.short && s.tool === want.tool,
    );
    if (got) wantNames.add(got.name);
    if (!got) {
      if (CHECK) {
        problems.push(`session ${want.machine}/${want.tool}-${want.short} missing`);
        continue;
      }
      const machineUp = agents.find((a) => a.machine === want.machine)?.online;
      if (!machineUp) {
        problems.push(
          `cannot create ${want.tool}-${want.short}: machine '${want.machine}' is offline`,
        );
        continue;
      }
      const created = await api(
        "POST",
        `/api/session?machine=${want.machine}`,
        { tool: want.tool, name: want.name, dir: want.dir, skipPermissions: true },
        { okStatuses: [409, 424] },
      );
      if (created.status === 424) {
        problems.push(
          `cannot create ${want.tool}-${want.short}: ${JSON.stringify(created.json)}`,
        );
        continue;
      }
      const name = created.json?.name ?? `${want.tool}-${want.short}`;
      wantNames.add(name);
      console.log(`✓ created ${want.machine}/${name}`);
    }
    const name = got?.name ?? [...wantNames].pop();
    if (!CHECK && want.label && got?.label !== want.label)
      await api("POST", `/api/session/label?machine=${want.machine}`, {
        session: name,
        label: want.label,
      });
    if (!CHECK && want.color && got?.color !== want.color)
      await api("POST", `/api/session/color?machine=${want.machine}`, {
        session: name,
        color: want.color,
      });
  }

  // Extra sessions crowd the 5-session Free cap and dirty the scene.
  for (const s of sessions) {
    const wanted = SESSIONS.some(
      (w) => w.machine === s.machine && w.short === s.short && w.tool === s.tool,
    );
    if (wanted) continue;
    if (CHECK) problems.push(`extra session ${s.machine ?? "?"}/${s.name}`);
    else {
      await api("POST", `/api/session/delete?machine=${s.machine}`, {
        session: s.name,
        mode: "kill",
      });
      console.log(`✓ removed extra session ${s.machine}/${s.name}`);
    }
  }

  if (problems.length) {
    console.error(`\nstage: ${problems.length} problem(s):`);
    for (const p of problems) console.error(`  ✗ ${p}`);
    process.exit(1);
  }
  console.log("\nstage: scene matches");
}

await login();
if (ENROLL) await enroll(ENROLL);
else await stage();
