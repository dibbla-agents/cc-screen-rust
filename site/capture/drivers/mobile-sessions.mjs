// The session switcher on a phone, showing the `Recent` section (proposal
// 0078) — which only exists once you have actually worked in a session, so the
// shot earns it rather than staging it: attach to two sessions in turn (past
// the 1s dwell gate each time), then reopen the drawer. The session you end up
// in is deliberately NOT in Recent (it is on screen), so the section shows the
// one before it, which is the whole point of the feature.
export default async (page) => {
  const open = async () => {
    await page.locator("[data-drawer] [data-session-row]").first().waitFor({ timeout: 15000 });
  };
  await open();
  await page.locator("text=Docs site").first().click();
  await page.waitForTimeout(2500);
  // Back to the switcher (the phone's ☰ / header button), then into Web app.
  await page.locator('[aria-label="Open sessions"]').first().click();
  await open();
  await page.locator("text=Web app").first().click();
  await page.waitForTimeout(2500);
  await page.locator('[aria-label="Open sessions"]').first().click();
  await page.locator("[data-recent-section]").first().waitFor({ timeout: 10000 });
  await page.waitForTimeout(800);
};
