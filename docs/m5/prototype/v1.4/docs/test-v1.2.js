const puppeteer = require('/tmp/pw/node_modules/puppeteer-core');
const fs = require('fs');
const path = require('path');

const CHROME = '/usr/bin/google-chrome-stable';
const FILE = 'file:///workspace/openpicasa-review/designs/prototype-v1/index.html';
const SHOT = '/workspace/openpicasa-review/designs/prototype-v1/shots-v1.2';
const REPORT = '/workspace/openpicasa-review/designs/prototype-v1/docs/TEST-RESULTS-v1.2.md';
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
  await new Promise(r => setTimeout(r, 160));
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
  await new Promise(r => setTimeout(r, 60));
}

(async () => {
  const browser = await puppeteer.launch({
    executablePath: CHROME, headless: 'new',
    userDataDir: '/tmp/op-proto-chrome-v12',
    args: ['--no-sandbox', '--disable-gpu', '--allow-file-access-from-files']
  });
  const page = await browser.newPage();
  page.on('pageerror', e => rec('pageerror:' + e.message, false, e.message));
  await page.setViewport({ width: 1400, height: 940, deviceScaleFactor: 2 });

  try {
    await reset(page);

    rec('sidebar has settings group', await page.evaluate(() =>
      !!document.querySelector('a.row[href="#/settings"]') && !!document.querySelector('a.row[href="#/settings/index"]')
    ));

    await page.click('a.status-index');
    await new Promise(r => setTimeout(r, 100));
    rec('status bar opens index manager', await page.evaluate(() =>
      location.hash === '#/settings/index' && document.querySelector('.title-block h1').textContent === '索引管理' && !!document.querySelector('.crumb')
    ));

    rec('index page has back to settings', await page.evaluate(() =>
      !!document.querySelector('.crumb a[href="#/settings"]') && document.querySelector('.crumb a').innerText.indexOf('返回设置') >= 0
    ));
    rec('index page details + sources', await page.evaluate(() => {
      const text = document.body.innerText;
      return text.indexOf('当前任务') >= 0 && text.indexOf('20 / 20') >= 0 && text.indexOf('本机图库') >= 0 &&
        text.indexOf('旅行 SD 卡') >= 0 && text.indexOf('失联') >= 0 && text.indexOf('重建索引（演示）') >= 0 &&
        text.indexOf('不会扫描真实磁盘') >= 0;
    }));

    await page.click('.crumb a[href="#/settings"]');
    await new Promise(r => setTimeout(r, 80));
    rec('crumb returns to settings', await page.evaluate(() =>
      location.hash === '#/settings' && document.querySelector('.title-block h1').textContent === '设置'
    ));
    rec('settings has index entry + summary', await page.evaluate(() =>
      !!document.querySelector('[data-act="go-index"], a.btn[href="#/settings/index"]') &&
      document.body.innerText.indexOf('打开索引管理') >= 0 &&
      !!document.querySelector('.index-detail[data-index-status]')
    ));

    await page.click('a.btn[href="#/settings/index"], [data-act="go-index"]');
    await new Promise(r => setTimeout(r, 80));
    rec('settings button same route', await page.evaluate(() => location.hash === '#/settings/index'));

    await page.click('a.row[href="#/all"]');
    await new Promise(r => setTimeout(r, 80));
    await page.focus('a.status-index');
    await page.keyboard.press('Enter');
    await new Promise(r => setTimeout(r, 100));
    rec('keyboard on status bar opens index', await page.evaluate(() => location.hash === '#/settings/index'));

    await page.click('a.row[href="#/settings/index"]');
    await new Promise(r => setTimeout(r, 80));
    rec('sidebar subitem same route', await page.evaluate(() => location.hash === '#/settings/index'));

    // slider no regression
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await page.evaluate(() => {
      const el = document.querySelector('input.thumb-range');
      el.value = '97';
      el.dispatchEvent(new Event('input', { bubbles: true }));
    });
    rec('slider 97 still exact', await page.evaluate(() => OPApp.thumbSize() === 97 && Math.round(document.querySelector('.cell').getBoundingClientRect().width) === 97));
    await page.evaluate(() => {
      const el = document.querySelector('input.thumb-range');
      el.value = '177';
      el.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await page.goto(FILE + '#/settings/index', { waitUntil: 'networkidle0' });
    rec('slider persists on index page', await page.evaluate(() => OPApp.thumbSize() === 177));
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    rec('slider persists back to library', await page.evaluate(() => Math.round(document.querySelector('.cell').getBoundingClientRect().width) === 177));

    // import then open manager, progress shared and continues
    await page.click('[data-act="import"]');
    await new Promise(r => setTimeout(r, 60));
    await page.click('[data-act="pick-src"][data-id="desk"]');
    await new Promise(r => setTimeout(r, 60));
    await page.click('[data-act="import-next"]');
    await new Promise(r => setTimeout(r, 60));
    await page.click('[data-act="import-run"]');
    await page.waitForFunction(() => {
      const el = document.querySelector('[data-index-status]');
      return el && el.getAttribute('data-index-status') === 'running' && parseInt(el.getAttribute('data-pct') || '0', 10) >= 8;
    }, { timeout: 4000 });

    const midBar = await page.evaluate(() => {
      const a = document.querySelector('a.status-index');
      return { s: a.getAttribute('data-index-status'), p: +a.getAttribute('data-pct'), d: +a.getAttribute('data-done'), t: +a.getAttribute('data-total') };
    });
    await page.click('a.status-index');
    await new Promise(r => setTimeout(r, 100));
    const midPage = await page.evaluate(() => {
      const d = document.querySelector('.set-sec[data-index-status]');
      const a = document.querySelector('a.status-index');
      return {
        hash: location.hash,
        page: { s: d.getAttribute('data-index-status'), p: +d.getAttribute('data-pct'), d: +d.getAttribute('data-done'), t: +d.getAttribute('data-total') },
        bar: { s: a.getAttribute('data-index-status'), p: +a.getAttribute('data-pct') },
        title: document.querySelector('.title-block h1').textContent,
        task: document.body.innerText.indexOf('导入') >= 0
      };
    });
    rec('running job opens manager without reset', midPage.hash === '#/settings/index' && midPage.page.s === 'running' && midPage.bar.s === 'running' && midPage.page.p === midPage.bar.p && midPage.page.t === 8 && midPage.task, JSON.stringify({ midBar, midPage }));

    const p1 = midPage.page.p;
    await new Promise(r => setTimeout(r, 400));
    const p2 = await page.evaluate(() => +document.querySelector('.set-sec[data-index-status]').getAttribute('data-pct'));
    rec('progress continues on manager page', p2 >= p1, 'p1=' + p1 + ' p2=' + p2);

    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    const onSettings = await page.evaluate(() => {
      const a = document.querySelector('a.status-index');
      const d = document.querySelector('.index-detail');
      return { s: a.getAttribute('data-index-status'), bp: +a.getAttribute('data-pct'), dp: +d.getAttribute('data-pct') };
    });
    rec('settings summary matches bar mid-job', onSettings.s === 'running' && onSettings.bp === onSettings.dp, JSON.stringify(onSettings));

    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    rec('return to library mid-job still running', await page.evaluate(() => document.querySelector('a.status-index').getAttribute('data-index-status') === 'running' || document.querySelector('a.status-index').getAttribute('data-index-status') === 'done'));

    await page.waitForFunction(() => {
      const el = document.querySelector('a.status-index');
      return el && el.getAttribute('data-index-status') === 'done';
    }, { timeout: 8000 });
    rec('job completes after navigation', await page.evaluate(() => {
      const el = document.querySelector('a.status-index');
      return el.getAttribute('data-index-status') === 'done' && +el.getAttribute('data-pct') === 100 && el.innerText.indexOf('索引完成') >= 0;
    }));

    // rebuild from manager
    await reset(page);
    await page.goto(FILE + '#/settings/index', { waitUntil: 'networkidle0' });
    await page.click('[data-act="rebuild-index"]');
    await page.waitForFunction(() => {
      const el = document.querySelector('.set-sec[data-index-status]');
      return el && (el.getAttribute('data-index-status') === 'running' || el.getAttribute('data-index-status') === 'done');
    }, { timeout: 3000 });
    rec('rebuild starts from manager', await page.evaluate(() => {
      const s = document.querySelector('.set-sec[data-index-status]').getAttribute('data-index-status');
      return s === 'running' || s === 'done';
    }));
    await page.waitForFunction(() => {
      const el = document.querySelector('.set-sec[data-index-status]');
      return el && el.getAttribute('data-index-status') === 'done' && +el.getAttribute('data-pct') === 100;
    }, { timeout: 8000 });
    rec('rebuild completes on manager', await page.evaluate(() => {
      const el = document.querySelector('.set-sec[data-index-status]');
      const bar = document.querySelector('a.status-index');
      return el.getAttribute('data-index-status') === 'done' && bar.getAttribute('data-index-status') === 'done' &&
        +el.getAttribute('data-done') === +bar.getAttribute('data-done');
    }));

    // themes
    await page.evaluate(() => OPStore.setTheme('dark'));
    await new Promise(r => setTimeout(r, 80));
    rec('dark theme on manager', await page.evaluate(() => document.documentElement.getAttribute('data-theme') === 'dark' && document.querySelector('.title-block h1').textContent === '索引管理'));
    await page.evaluate(() => OPStore.setTheme('light'));
    await new Promise(r => setTimeout(r, 80));
    rec('theme restore light', await page.evaluate(() => document.documentElement.getAttribute('data-theme') === 'light'));

  } catch (e) {
    rec('uncaught ' + e.message, false, e.stack);
  }

  // screenshots
  try {
    async function prep(th) {
      await reset(page);
      await page.evaluate((th) => { OPStore.setTheme(th); OPApp.setThumb(144, true); }, th);
      await page.reload({ waitUntil: 'networkidle0' });
    }
    await prep('light');
    await page.goto(FILE + '#/settings/index', { waitUntil: 'networkidle0' });
    await shot(page, 'v12-light-index-manager.png');
    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await shot(page, 'v12-light-settings-entry.png');
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await page.hover('a.status-index');
    await new Promise(r => setTimeout(r, 80));
    await shot(page, 'v12-light-statusbar-entry.png');

    await prep('dark');
    await page.goto(FILE + '#/settings/index', { waitUntil: 'networkidle0' });
    await shot(page, 'v12-dark-index-manager.png');
    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await shot(page, 'v12-dark-settings-entry.png');
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await page.hover('a.status-index');
    await new Promise(r => setTimeout(r, 80));
    await shot(page, 'v12-dark-statusbar-entry.png');

    await prep('light');
    await page.click('[data-act="import"]');
    await new Promise(r => setTimeout(r, 50));
    await page.click('[data-act="pick-src"][data-id="desk"]');
    await new Promise(r => setTimeout(r, 50));
    await page.click('[data-act="import-next"]');
    await new Promise(r => setTimeout(r, 50));
    await page.click('[data-act="import-run"]');
    await page.waitForFunction(() => {
      const el = document.querySelector('a.status-index');
      return el && el.getAttribute('data-index-status') === 'running' && parseInt(el.getAttribute('data-pct') || '0', 10) >= 15;
    }, { timeout: 4000 });
    await page.goto(FILE + '#/settings/index', { waitUntil: 'networkidle0' });
    await new Promise(r => setTimeout(r, 80));
    await shot(page, 'v12-light-index-manager-running.png');
  } catch (e) {
    rec('screenshot error ' + e.message, false, e.stack);
  }

  await browser.close();
  const pass = results.filter(r => r.pass).length;
  const fail = results.filter(r => !r.pass).length;
  let md = '# 交互测试结果 · OpenPicasa 原型 v1.2\n\n';
  md += '- 日期：2026-11-16（Asia/Shanghai）\n';
  md += '- 环境：puppeteer-core + Chrome，本地 `file://` 打开 `index.html`\n';
  md += '- 本文件为 v1.2「索引管理」页面与入口用例。v1 核心 39 项与 v1.1 滑块/进度 28 项另见对应文档。\n';
  md += '- 合计：' + results.length + ' 项，通过 ' + pass + '，未通过 ' + fail + '\n\n';
  md += '| 结果 | 用例 | 说明 |\n| --- | --- | --- |\n';
  results.forEach(r => { md += '| ' + (r.pass ? '通过' : '未通过') + ' | ' + r.name + ' | ' + String(r.detail || '').replace(/\|/g, '\\|').replace(/\n/g, ' ') + ' |\n'; });
  md += '\n## 说明\n\n';
  md += '- 索引管理路由为 `#/settings/index`，属于设置下级。底栏索引进度整区为链接，可用鼠标或键盘（Tab + Enter）打开。\n';
  md += '- 导入/重建进度与底栏、设置摘要、管理页共用 `indexJob`，切页不会中断。\n';
  md += '- 暂停/继续为管理页提供的演示控制，会真实停止或恢复同一任务。\n';
  fs.writeFileSync(REPORT, md);
  console.log('\nWrote', REPORT);
  console.log('PASS', pass, 'FAIL', fail);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error(e); process.exit(1); });
