// Attach the Web app session, then open the file browser from the phone
// footer — the tree lands in the session's own directory (web-app).
export default async (page) => {
  await page.locator("text=Web app").first().click();
  await page.waitForTimeout(2000);
  await page.locator('[aria-label="Browse, view and download files"]').click();
  await page.waitForTimeout(2500);
};
