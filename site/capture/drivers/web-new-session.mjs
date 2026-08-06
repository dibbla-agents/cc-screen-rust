// Open the desktop sidebar switcher via the header's ☰ button (the Ctrl+B
// prefix is swallowed while the empty-pane switcher's autofocused search box
// has focus), then enter its search-first create flow and search for the
// project folder — the frame shows the directory search doing its job.
export default async (page) => {
  await page.locator('[aria-label="Open sessions"]').click();
  await page.waitForTimeout(500);
  const sidebar = page.locator(".inset-y-0.left-0");
  await sidebar.locator("text=New session").first().click();
  await page.waitForTimeout(1200);
  await sidebar.locator('input[placeholder^="Search folders"]').fill("web");
  await page.waitForTimeout(1200);
};
