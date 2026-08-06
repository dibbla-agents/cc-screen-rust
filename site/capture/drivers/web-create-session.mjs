// Choreography for the C4 clip (proposal 0061): New session → directory
// search → the session opens with the assistant running, all against the live
// demo scene on pine. The fresh context has no attached session, so the app
// opens on the session picker — the natural first frame.
//
// Story: New session… → search "web" → the search lands on ~/web-app →
// name it "api" → Create → the pane mounts and Claude starts up in it; the
// clip ends on Claude's prompt.
//
// Side effect: a real session claude-api on pine. The scene is restored
// afterwards with `node stage.mjs` (it removes non-scene sessions).

export default async (page, _ctx) => {
  // The picker with the three staged sessions is the establishing shot.
  await page.getByText("New session…").first().waitFor({ timeout: 10000 });
  await page.waitForTimeout(900);
  await page.getByText("New session…").first().click();

  // Directory search: type like a human, watch the results narrow.
  const search = page.locator('input[placeholder="Search folders…"]');
  await search.waitFor({ timeout: 5000 });
  await page.waitForTimeout(600);
  await search.pressSequentially("web", { delay: 150 });
  // The cursor lands on ~/web-app; the name field seeds from it.
  await page.getByText("Creates in ~/web-app").waitFor({ timeout: 5000 });
  await page.waitForTimeout(700);

  // A tidy name of our own.
  const name = page.locator('input[placeholder="session name"]');
  await name.click();
  await name.fill("");
  await name.pressSequentially("api", { delay: 140 });
  await page.waitForTimeout(500);

  await page.getByRole("button", { name: "Create", exact: true }).click();

  // The pane mounts and Claude boots; hold until its prompt has settled.
  // (The terminal is a canvas — timing calibrated by rehearsal, not queried.)
  await page.waitForTimeout(6500);
};
