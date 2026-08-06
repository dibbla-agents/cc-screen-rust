// Coworking: the editor overlay (tree + open file) with the live agent in
// the right-hand mirror column, at a compact desktop size.
export default async (page) => {
  await page.locator("text=Web app").first().click();
  await page.waitForTimeout(2500);
  // Summon the collapsed desktop header, then open Files.
  await page.mouse.move(425, 2);
  await page.waitForTimeout(600);
  await page.locator('[title="Files — browse, view, edit, download"]').first().click();
  await page.waitForTimeout(2000);
  // Scope to the editor overlay — xterm renders terminal text into the DOM,
  // so a bare text= match can land on "README.md" inside the terminal.
  await page.locator(".fixed.inset-0 >> text=README.md").first().click();
  await page.waitForTimeout(2500);
};
