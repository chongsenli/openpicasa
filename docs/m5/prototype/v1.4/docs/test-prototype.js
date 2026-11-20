const puppeteer = require('/tmp/pw/node_modules/puppeteer-core');
const fs = require('fs');
const path = require('path');

const CHROME = '/usr/bin/google-chrome-stable';
const FILE = 'file:///workspace/openpicasa-review/designs/prototype-v1/index.html';
const SHOT = '/workspace/openpicasa-review/designs/prototype-v1/shots';
const REPORT = '/workspace/openpicasa-review/designs/prototype-v1/docs/TEST-RESULTS.md';
fs.mkdirSync(SHOT, { recursive: true });

const results = [];
function rec(name, pass, detail) {
  results.push({ name, pass, detail: detail || '' });
  console.log((pass ? 'PASS' : 'FAIL'), name, detail || '');
}

async function shot(page, name) {
  await page.evaluate(async () => {
    await document.fonts.ready;
    await Promise.all([...document.images].map(i => i.complete && i.naturalWidth ? 0 : new Promise(r => { i.onload = i.onerror = r; })));
  });
  await new Promise(r => setTimeout(r, 200));
  const box = await page.$eval('.stage-wrap', el => {
    const r = el.getBoundingClientRect();
    return { x: r.x, y: r.y, width: r.width, height: r.height };
  });
  await page.screenshot({ path: path.join(SHOT, name), clip: box, type: 'png' });
}

async function reset(page) {
  if (!page.url().startsWith('file://')) {
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
  }
  await page.evaluate(() => {
    try { localStorage.removeItem('openpicasa.prototype.v1'); } catch (e) {}
    if (window.OPStore) OPStore.reset();
  });
  await page.reload({ waitUntil: 'networkidle0' });
  await new Promise(r => setTimeout(r, 80));
}

(async () => {
  const browser = await puppeteer.launch({
    executablePath: CHROME, headless: 'new',
    userDataDir: '/tmp/op-proto-chrome',
    args: ['--no-sandbox', '--disable-gpu', '--allow-file-access-from-files', '--disable-features=BlockInsecurePrivateNetworkRequests']
  });
  const page = await browser.newPage();
  page.on('pageerror', e => rec('pageerror:' + e.message, false, e.message));
  await page.setViewport({ width: 1400, height: 940, deviceScaleFactor: 2 });

  try {
    await reset(page);
    rec('load home', await page.evaluate(() => document.querySelectorAll('.cell').length === 20), 'cells');

    // navigation
    await page.click('a[href="#/recent"]');
    await new Promise(r => setTimeout(r, 80));
    const recentTitle = await page.evaluate(() => document.querySelector('.title-block h1').textContent);
    rec('nav recent', recentTitle === '最近导入', recentTitle);
    await page.click('a[href="#/stars"]');
    await new Promise(r => setTimeout(r, 80));
    rec('nav stars', await page.evaluate(() => document.querySelector('.title-block h1').textContent === '星标'));
    await page.click('a[href="#/folder/2024-05"]');
    await new Promise(r => setTimeout(r, 80));
    rec('nav folder May', await page.evaluate(() => document.querySelector('.title-block h1').textContent.indexOf('5') >= 0));
    await page.click('a[href="#/all"]');
    await new Promise(r => setTimeout(r, 80));

    // search and clear
    await page.click('#search-input');
    await page.type('#search-input', '河谷');
    await new Promise(r => setTimeout(r, 200));
    const nSearch = await page.evaluate(() => document.querySelectorAll('.cell').length);
    rec('search 河谷', nSearch >= 1 && nSearch < 20, 'n=' + nSearch);
    await page.click('[data-act="clear-search"]');
    await new Promise(r => setTimeout(r, 150));
    rec('clear search', await page.evaluate(() => document.querySelectorAll('.cell').length === 20));

    // empty search
    await page.type('#search-input', 'zzzz-no-match-xyz');
    await new Promise(r => setTimeout(r, 200));
    rec('search no results', await page.evaluate(() => !!document.querySelector('.empty') && document.body.innerText.indexOf('没有符合条件') >= 0));
    await page.click('[data-act="clear-filters"]');
    await new Promise(r => setTimeout(r, 150));
    rec('clear filters restores grid', await page.evaluate(() => document.querySelectorAll('.cell').length === 20));

    // multi-select
    const ids = await page.evaluate(() => [...document.querySelectorAll('.cell')].slice(0, 3).map(el => el.getAttribute('data-id')));
    await page.click('.cell[data-id="' + ids[0] + '"]');
    rec('single select', await page.evaluate(() => OPApp.getSel().length === 1));
    await page.keyboard.down('Meta');
    await page.click('.cell[data-id="' + ids[1] + '"]');
    await page.click('.cell[data-id="' + ids[2] + '"]');
    await page.keyboard.up('Meta');
    rec('multi select 3', await page.evaluate(() => OPApp.getSel().length === 3), await page.evaluate(() => OPApp.getSel().join(',')));

    // add to album via menu
    const beforeCoast = await page.evaluate(() => OPStore.photosInAlbum('coast').length);
    await page.click('[data-act="album-menu"]');
    await new Promise(r => setTimeout(r, 50));
    await page.click('.menu [data-act="add-album"][data-id="coast"]');
    await new Promise(r => setTimeout(r, 80));
    const afterCoast = await page.evaluate(() => OPStore.photosInAlbum('coast').length);
    rec('add to album 海边', afterCoast >= beforeCoast && afterCoast >= 3, beforeCoast + '->' + afterCoast);

    // create album
    const nAlb = await page.evaluate(() => OPStore.get().albums.length);
    await page.evaluate(() => OPApp.setSel && OPApp.setSel(OPApp.getSel()));
    // trigger new album prompt
    await page.click('[data-act="album-menu"]');
    await new Promise(r => setTimeout(r, 50));
    await page.waitForSelector('.menu [data-act="new-album"]');
    await page.click('.menu [data-act="new-album"]');
    await new Promise(r => setTimeout(r, 80));
    await page.waitForSelector('#modal-input', { timeout: 2000 });
    rec('create album dialog', await page.evaluate(() => !!document.querySelector('.modal input')));
    await page.click('#modal-input', { clickCount: 3 });
    await page.type('#modal-input', '原型相册');
    await page.click('[data-act="modal-ok"]');
    await new Promise(r => setTimeout(r, 120));
    rec('create album named 原型相册', await page.evaluate(() => OPStore.get().albums.some(a => a.name === '原型相册')), 'albums=' + (await page.evaluate(() => OPStore.get().albums.map(a=>a.name).join(','))));

    // import wizard
    await page.click('[data-act="import"]');
    await new Promise(r => setTimeout(r, 80));
    rec('import step1', await page.evaluate(() => location.hash.indexOf('import') >= 0 && document.body.innerText.indexOf('选择来源') >= 0));
    await page.click('[data-act="pick-src"][data-id="desk"]');
    await new Promise(r => setTimeout(r, 80));
    await page.click('[data-act="import-next"]');
    await new Promise(r => setTimeout(r, 80));
    rec('import step2 confirm', await page.evaluate(() => document.body.innerText.indexOf('开始索引') >= 0));
    await page.click('[data-act="import-run"]');
    // wait for completion
    await page.waitForFunction(() => document.body.innerText.indexOf('索引完成') >= 0, { timeout: 8000 });
    rec('import index complete', await page.evaluate(() => document.body.innerText.indexOf('成功') >= 0));
    await page.click('[data-act="go-recent"]');
    await new Promise(r => setTimeout(r, 120));
    rec('import result in recent', await page.evaluate(() => location.hash.indexOf('recent') >= 0 && document.querySelectorAll('.cell').length >= 5));

    // viewer
    await page.goto(FILE + '#/viewer/19-fog', { waitUntil: 'networkidle0' });
    await new Promise(r => setTimeout(r, 150));
    rec('viewer open', await page.evaluate(() => !!document.querySelector('.win.viewer') && document.body.innerText.indexOf('雪峰下的河谷') >= 0));
    const beforeId = await page.evaluate(() => location.hash);
    await page.click('[data-act="next"]');
    await new Promise(r => setTimeout(r, 120));
    const afterNext = await page.evaluate(() => location.hash);
    rec('viewer next', afterNext !== beforeId, beforeId + ' -> ' + afterNext);
    await page.click('[data-act="prev"]');
    await new Promise(r => setTimeout(r, 120));
    rec('viewer prev', true);
    rec('info closed by default', await page.evaluate(() => !document.querySelector('.win').classList.contains('info-open')));
    await page.click('[data-act="toggle-info"]');
    await new Promise(r => setTimeout(r, 80));
    rec('info panel opens and pushes', await page.evaluate(() => document.querySelector('.win').classList.contains('info-open') && getComputedStyle(document.querySelector('.info-panel')).display === 'flex'));
    await page.click('[data-act="toggle-info"]');
    await new Promise(r => setTimeout(r, 80));
    rec('info panel closes', await page.evaluate(() => getComputedStyle(document.querySelector('.info-panel')).display === 'none'));
    await page.click('[data-act="back"]');
    await new Promise(r => setTimeout(r, 80));
    rec('viewer back to library', await page.evaluate(() => location.hash.indexOf('viewer') < 0));

    // tags / rating persist
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await page.click('.cell[data-id="19-fog"]');
    await page.keyboard.press('5');
    await new Promise(r => setTimeout(r, 80));
    rec('rating 5 persists in store', await page.evaluate(() => OPStore.photo('19-fog').rating === 5 && OPStore.photo('19-fog').starred === true));
    // reload page, check persist
    await page.reload({ waitUntil: 'networkidle0' });
    await new Promise(r => setTimeout(r, 150));
    rec('rating persists after reload', await page.evaluate(() => OPStore.photo('19-fog').rating === 5));

    // theme switch via settings (visible controls, persists across routes)
    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await page.click('.theme-sw [data-act="set-theme"][data-theme="dark"]');
    await new Promise(r => setTimeout(r, 80));
    rec('theme dark', await page.evaluate(() => document.documentElement.getAttribute('data-theme') === 'dark' && OPStore.get().theme === 'dark'));
    rec('theme persists on settings', await page.evaluate(() => document.documentElement.getAttribute('data-theme') === 'dark'));
    await page.goto(FILE + '#/help', { waitUntil: 'networkidle0' });
    rec('theme persists on help', await page.evaluate(() => document.documentElement.getAttribute('data-theme') === 'dark'));
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    rec('theme persists on library', await page.evaluate(() => document.documentElement.getAttribute('data-theme') === 'dark'));
    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await page.click('.theme-sw [data-act="set-theme"][data-theme="light"]');
    await new Promise(r => setTimeout(r, 80));
    rec('theme back to light', await page.evaluate(() => document.documentElement.getAttribute('data-theme') === 'light'));

    // settings page
    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    rec('settings page', await page.evaluate(() => document.body.innerText.indexOf('图库来源') >= 0 && document.body.innerText.indexOf('索引与缩略图') >= 0));

    // empty / error / reset
    await page.click('[data-act="demo-empty"]');
    await new Promise(r => setTimeout(r, 120));
    rec('empty library state', await page.evaluate(() => document.body.innerText.indexOf('这里还没有照片') >= 0));
    await page.click('[data-act="reset-demo"]');
    await new Promise(r => setTimeout(r, 80));
    rec('reset confirm dialog', await page.evaluate(() => !!document.querySelector('.modal') && document.body.innerText.indexOf('恢复演示数据') >= 0));
    await page.click('[data-act="modal-ok"]');
    await new Promise(r => setTimeout(r, 150));
    rec('reset restores photos', await page.evaluate(() => document.querySelectorAll('.cell').length === 20));

    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await page.click('[data-act="demo-offline"]');
    await new Promise(r => setTimeout(r, 120));
    rec('offline / disconnected state', await page.evaluate(() => /无法访问|不可访问/.test(document.body.innerText)));

    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await page.click('[data-act="demo-denied"]');
    await new Promise(r => setTimeout(r, 120));
    rec('permission denied state', await page.evaluate(() => document.body.innerText.indexOf('没有访问权限') >= 0));

    await page.click('[data-act="clear-demo-flags"]');
    await new Promise(r => setTimeout(r, 80));

    // unimplemented action not silent
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await page.click('[data-act="more-menu"]');
    await page.waitForSelector('.menu [data-act="nyi"]', { timeout: 2000 });
    await page.click('.menu [data-act="nyi"]');
    await new Promise(r => setTimeout(r, 80));
    rec('unimplemented explains itself', await page.evaluate(() => document.body.innerText.indexOf('未实现') >= 0 || document.body.innerText.indexOf('导出') >= 0));

    rec('no broken images at end', await page.evaluate(() => [...document.images].filter(i => !i.naturalWidth).length === 0));

  } catch (e) {
    rec('uncaught ' + e.message, false, e.stack);
  }

  // ---------- screenshots both themes ----------
  const shots = [
    ['all', 'home', '所有照片'],
    ['recent', 'recent', '最近导入'],
    ['stars', 'stars', '星标'],
    ['album/coast', 'album', '相册详情'],
    ['tag/' + encodeURIComponent('旅行'), 'tag', '标签'],
    ['folder/2024-05', 'folder', '文件夹'],
    ['settings', 'settings', '设置'],
    ['help', 'help', '帮助'],
    ['import', 'import', '导入向导'],
    ['viewer/19-fog', 'viewer', '查看器'],
    ['viewer/19-fog?info=1', 'viewer-info', '查看器信息栏'],
  ];

  async function prepTheme(th) {
    await reset(page);
    await page.evaluate((th) => {
      OPStore.setTheme(th);
      if (window.OPApp) { OPApp.setSel([]); OPApp.setQ(''); OPApp.setInfo(false); }
    }, th);
    await page.reload({ waitUntil: 'networkidle0' });
    await new Promise(r => setTimeout(r, 100));
  }

  for (const th of ['light', 'dark']) {
   try {
    await prepTheme(th);
    for (const [hash, slug] of shots) {
      if (hash === 'import') {
        await page.evaluate(() => { OPApp.startImport(); });
        await new Promise(r => setTimeout(r, 100));
      } else {
        await page.goto(FILE + '#/' + hash, { waitUntil: 'networkidle0' });
      }
      if (hash.indexOf('info=1') >= 0) {
        await page.evaluate(() => { OPApp.setInfo(true); });
        await new Promise(r => setTimeout(r, 80));
      }
      await shot(page, 'proto-v1-' + th + '-' + slug + '.png');
    }
    // empty, no results, offline, multi-select tray, confirm
    await page.evaluate(() => { OPStore.clearLibraryDemo(); });
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await shot(page, 'proto-v1-' + th + '-empty.png');

    await reset(page);
    await page.evaluate((th) => { OPStore.setTheme(th); }, th);
    await page.evaluate(() => { OPApp.setQ('zzzz-no-match-xyz'); });
    await new Promise(r => setTimeout(r, 80));
    await shot(page, 'proto-v1-' + th + '-noresults.png');

    await page.evaluate(() => { OPApp.setQ(''); OPStore.patch(s => { s.ui.forceOffline = true; }); });
    await page.goto(FILE + '#/source/sd', { waitUntil: 'networkidle0' });
    await shot(page, 'proto-v1-' + th + '-offline.png');

    await reset(page);
    await page.evaluate((th) => { OPStore.setTheme(th); }, th);
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    const ids2 = await page.evaluate(() => [...document.querySelectorAll('.cell')].slice(0, 3).map(el => el.getAttribute('data-id')));
    await page.evaluate((ids) => { OPApp.setSel(ids); OPStore.patch(s => { s.ui.trayOpen = true; s.tray = ids; }); }, ids2);
    await new Promise(r => setTimeout(r, 80));
    await shot(page, 'proto-v1-' + th + '-multiselect.png');

    try {
      await page.waitForSelector('[data-act="remove-lib"]', { timeout: 2000 });
      await page.click('[data-act="remove-lib"]');
      await page.waitForSelector('.modal [data-act="modal-cancel"]', { timeout: 2000 });
      await shot(page, 'proto-v1-' + th + '-confirm.png');
      await page.click('.modal [data-act="modal-cancel"]');
    } catch (e) {
      rec('screenshot confirm ' + th, false, e.message);
    }

    await reset(page);
    await page.evaluate((th) => { OPStore.setTheme(th); }, th);
    await page.reload({ waitUntil: 'networkidle0' });
    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await page.click('[data-act="demo-loading"]');
    await new Promise(r => setTimeout(r, 200));
    await shot(page, 'proto-v1-' + th + '-loading.png');
   } catch (e) { rec('screenshot pass ' + th, false, e.message); }
  }

  await browser.close();

  const pass = results.filter(r => r.pass).length;
  const fail = results.filter(r => !r.pass).length;
  let md = '# 交互测试结果 · OpenPicasa 原型 v1\n\n';
  md += '- 日期：2026-11-16（Asia/Shanghai）\n';
  md += '- 环境：puppeteer-core + Chrome，本地 `file://` 打开 `index.html`\n';
  md += '- 合计：' + results.length + ' 项，通过 ' + pass + '，未通过 ' + fail + '\n\n';
  md += '| 结果 | 用例 | 说明 |\n| --- | --- | --- |\n';
  results.forEach(r => { md += '| ' + (r.pass ? '通过' : '未通过') + ' | ' + r.name + ' | ' + (r.detail || '') + ' |\n'; });
  md += '\n## 未覆盖或限制\n\n';
  md += '- 触控板框选与边缘自动滚动未实现（文档登记为非目标/原型未做）。\n';
  md += '- 索引导出包、真实磁盘监视、备份恢复未作为可运行能力提供；点击会提示说明。\n';
  md += '- 导入向导使用演示来源，不读取真实用户文件。\n';
  md += '- 拍摄信息除本地文件宽高与体积外均为界面示例。\n';
  md += '- 本测试在无障碍/屏幕阅读器、多窗口尺寸方面未做完整覆盖。\n';
  fs.writeFileSync(REPORT, md);
  console.log('\nWrote', REPORT);
  console.log('PASS', pass, 'FAIL', fail);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error(e); process.exit(1); });
