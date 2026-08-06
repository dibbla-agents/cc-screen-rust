// File browser → find-file "App" → src/App.tsx open in the editor, on the
// phone viewport. The find-file box is more robust than expanding tree rows
// and shows the real search affordance.
export default async (page) => {
  await page.locator("text=Web app").first().click();
  await page.waitForTimeout(2000);
  await page.locator('[aria-label="Browse, view and download files"]').click();
  await page.waitForTimeout(2000);
  await page.locator('input[placeholder^="Find file"]').fill("App");
  await page.waitForTimeout(1200);
  await page.locator("text=App.tsx").first().click();
  await page.waitForTimeout(2500);
};
