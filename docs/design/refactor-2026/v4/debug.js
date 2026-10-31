const { chromium } = require('playwright');
(async () => {
  const base = 'http://127.0.0.1:8765';
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1440, height: 1024 } });
  page.on('pageerror', e => console.log('PAGEERROR', e.message));
  page.on('console', m => console.log('CONSOLE', m.type(), m.text()));
  await page.goto(`${base}/index.html?single=1&shot=1&board=b02`);
  await page.waitForTimeout(1200);
  console.log('title', await page.title());
  console.log('types', await page.evaluate(() => [typeof NAV, typeof sCover, typeof sOverview, typeof sEntryRoutes, typeof sMobile].join(',')));
  console.log('scripts', await page.evaluate(() => [...document.scripts].map(s => ({src:s.src, len:s.src ? null : s.text.length}))));
  await browser.close();
})();
