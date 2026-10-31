const { chromium } = require('playwright');
const path = require('path');
const fs = require('fs');

(async () => {
  const base = 'http://127.0.0.1:8765';
  const dir = __dirname;
  const out = path.join(dir, 'shots');
  fs.mkdirSync(out, { recursive: true });
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1500, height: 1100 }, deviceScaleFactor: 1.5 });
  const boards = [
    ['b00','00-cover'],['b01','01-design-system'],['b02','02-overview'],['b03','03-usage'],
    ['b04','04-prompts'],['b05','05-access'],['b06','06-access-detail'],['b07','07-add-access'],
    ['b08','08-logs'],['b09','09-config-center'],['b10','10-models'],['b11','11-pricing'],
    ['b12','12-entry-routes'],['b13','13-pools'],['b14','14-resources'],['b15','15-settings'],
    ['b16','16-states'],['b17','17-mobile']
  ];
  for (const [id,name] of boards) {
    await page.goto(`${base}/index.html?single=1&shot=1&board=${id}`);
    await page.evaluate(() => document.fonts && document.fonts.ready);
    await page.waitForTimeout(400);
    const el = await page.$('.board.on');
    if (!el) { console.log('missing', id); continue; }
    await el.screenshot({ path: path.join(out, name + '.png') });
    console.log('shot', name);
  }
  await page.goto(`${base}/index.html?single=1&shot=1&board=b02&theme=light`);
  await page.evaluate(() => document.fonts && document.fonts.ready);
  await page.waitForTimeout(400);
  const light = await page.$('.board.on');
  if (light) await light.screenshot({ path: path.join(out, '02-overview-light.png') });
  await browser.close();
})();
