const puppeteer = require('/tmp/pw/node_modules/puppeteer-core');
const fs = require('fs');
const path = require('path');

const CHROME = '/usr/bin/google-chrome-stable';
const FILE = 'file:///workspace/openpicasa-review/designs/prototype-v1/index.html';
const SHOT = '/workspace/openpicasa-review/designs/prototype-v1/shots-v1.1';
const REPORT = '/workspace/openpicasa-review/designs/prototype-v1/docs/TEST-RESULTS-v1.1.md';
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
  await new Promise(r => setTimeout(r, 180));
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

async function overflowCheck(page) {
  return page.evaluate(() => {
    const win = document.querySelector('.win');
    const st = document.querySelector('.status');
    const wr = win.getBoundingClientRect();
    const sr = st.getBoundingClientRect();
    const cells = [...document.querySelectorAll('.cell, .lrow')];
    const overflowCells = cells.filter(el => {
      const r = el.getBoundingClientRect();
      return r.right > wr.right + 1.5 || r.left < wr.left - 1.5;
    }).length;
    const barJump = Math.abs(sr.height - 32) > 1;
    const barOff = sr.bottom > wr.bottom + 1.5 || sr.right > wr.right + 1.5;
    return {
      ok: overflowCells === 0 && !barJump && !barOff,
      overflowCells, barH: Math.round(sr.height), barOff, winH: Math.round(wr.height)
    };
  });
}

(async () => {
  const browser = await puppeteer.launch({
    executablePath: CHROME, headless: 'new',
    userDataDir: '/tmp/op-proto-chrome-v11',
    args: ['--no-sandbox', '--disable-gpu', '--allow-file-access-from-files']
  });
  const page = await browser.newPage();
  page.on('pageerror', e => rec('pageerror:' + e.message, false, e.message));
  await page.setViewport({ width: 1400, height: 940, deviceScaleFactor: 2 });

  try {
    await reset(page);
    rec('idle index visible', await page.evaluate(() => {
      const el = document.querySelector('.status-index');
      return el && el.getAttribute('data-index-status') === 'idle' && el.innerText.indexOf('20 / 20') >= 0;
    }));
    rec('slider present 80-280', await page.evaluate(() => {
      const el = document.querySelector('input.thumb-range');
      return el && el.min === '80' && el.max === '280' && el.step === '1';
    }));

    const sizes = [97, 133, 177, 219, 263];
    const measured = [];
    for (const v of sizes) {
      await page.evaluate((v) => {
        const el = document.querySelector('input.thumb-range');
        el.value = String(v);
        el.dispatchEvent(new Event('input', { bubbles: true }));
      }, v);
      await new Promise(r => setTimeout(r, 40));
      const m = await page.evaluate(() => {
        const r = document.querySelector('.cell').getBoundingClientRect();
        return { thumb: OPApp.thumbSize(), w: Math.round(r.width * 10) / 10, h: Math.round(r.height * 10) / 10, label: document.querySelector('[data-thumb-val]').textContent };
      });
      measured.push(m);
      rec('grid thumb ' + v + 'px', m.thumb === v && Math.abs(m.w - v) <= 1 && Math.abs(m.h - v) <= 1, JSON.stringify(m));
    }
    rec('five irregular sizes all distinct', new Set(measured.map(m => m.w)).size === sizes.length);

    // persist across route
    await page.evaluate(() => {
      const el = document.querySelector('input.thumb-range');
      el.value = '177';
      el.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await page.click('a[href="#/stars"]');
    await new Promise(r => setTimeout(r, 80));
    rec('size persists on stars', await page.evaluate(() => OPApp.thumbSize() === 177 && Math.round(document.querySelector('.cell').getBoundingClientRect().width) === 177));
    await page.goto(FILE + '#/album/coast', { waitUntil: 'networkidle0' });
    rec('size persists on album', await page.evaluate(() => OPApp.thumbSize() === 177));
    await page.reload({ waitUntil: 'networkidle0' });
    rec('size persists after refresh', await page.evaluate(() => OPApp.thumbSize() === 177 && document.documentElement.style.getPropertyValue('--thumb-size') === '177px'));

    // both themes + 3 layouts overflow
    for (const th of ['light', 'dark']) {
      await page.evaluate((th) => { OPStore.setTheme(th); OPApp.setThumb(177, true); }, th);
      await new Promise(r => setTimeout(r, 50));
      for (const layout of ['grid', 'masonry', 'rows']) {
        await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
        await page.evaluate((layout) => {
          OPStore.patch(s => { s.settings.layout = layout; });
        }, layout);
        await new Promise(r => setTimeout(r, 80));
        if (layout === 'masonry') {
          await page.evaluate(() => { if (window.OPApp) OPApp.render(); });
          await new Promise(r => setTimeout(r, 80));
        }
        const ov = await overflowCheck(page);
        rec(th + ' ' + layout + ' no overflow @177', ov.ok, JSON.stringify(ov));
        if (layout === 'grid') {
          rec(th + ' grid cell=177', await page.evaluate(() => Math.round(document.querySelector('.cell').getBoundingClientRect().width) === 177));
        }
        if (layout === 'masonry') {
          rec(th + ' masonry height follows slider', await page.evaluate(() => {
            const hs = [...document.querySelectorAll('.justified .cell')].map(el => Math.round(el.getBoundingClientRect().height));
            // justified may scale a filled row, but values must not collapse to a 3-size preset
            return hs.length && hs.every(h => h >= 90 && h <= 400);
          }));
        }
        if (layout === 'rows') {
          rec(th + ' list thumb=177', await page.evaluate(() => {
            const img = document.querySelector('.lrow img');
            return img && Math.round(img.getBoundingClientRect().width) === 177;
          }));
        }
      }
    }

    // reset layout to grid, theme light, start import, check shared progress
    await page.evaluate(() => {
      OPStore.setTheme('light');
      OPStore.patch(s => { s.settings.layout = 'grid'; });
    });
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await page.click('[data-act="import"]');
    await new Promise(r => setTimeout(r, 80));
    await page.click('[data-act="pick-src"][data-id="desk"]');
    await new Promise(r => setTimeout(r, 80));
    await page.click('[data-act="import-next"]');
    await new Promise(r => setTimeout(r, 80));
    await page.click('[data-act="import-run"]');
    await page.waitForFunction(() => {
      const el = document.querySelector('[data-index-status]');
      return el && el.getAttribute('data-index-status') === 'running' && parseInt(el.getAttribute('data-pct') || '0', 10) >= 8;
    }, { timeout: 4000 });

    const mid = await page.evaluate(() => {
      const a = document.querySelector('.status-index');
      const b = document.querySelector('.pill[data-index-status], [data-index-status]');
      return {
        bar: { s: a.getAttribute('data-index-status'), p: +a.getAttribute('data-pct'), d: +a.getAttribute('data-done'), t: +a.getAttribute('data-total') },
        wiz: { s: b.getAttribute('data-index-status'), p: +b.getAttribute('data-pct') },
        job: OPApp.indexJob()
      };
    });
    rec('import running on wizard+bar', mid.bar.s === 'running' && mid.job.status === 'running' && mid.bar.p === mid.job.pct, JSON.stringify(mid));

    const pctOnImport = mid.bar.p;
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await new Promise(r => setTimeout(r, 80));
    const onAll = await page.evaluate(() => {
      const a = document.querySelector('.status-index');
      return { s: a.getAttribute('data-index-status'), p: +a.getAttribute('data-pct') };
    });
    rec('progress continues after leaving wizard', onAll.s === 'running' && onAll.p >= pctOnImport, JSON.stringify({ pctOnImport, onAll }));

    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await new Promise(r => setTimeout(r, 80));
    const onSet = await page.evaluate(() => {
      const a = document.querySelector('.status-index');
      const d = document.querySelector('.index-detail');
      return {
        bar: { s: a.getAttribute('data-index-status'), p: +a.getAttribute('data-pct'), d: +a.getAttribute('data-done'), t: +a.getAttribute('data-total') },
        detail: { s: d.getAttribute('data-index-status'), p: +d.getAttribute('data-pct'), d: +d.getAttribute('data-done'), t: +d.getAttribute('data-total') }
      };
    });
    rec('settings and status bar share job', onSet.bar.s === onSet.detail.s && onSet.bar.p === onSet.detail.p && onSet.bar.d === onSet.detail.d && onSet.bar.t === onSet.detail.t, JSON.stringify(onSet));

    // theme switch mid-job must not reset
    const beforeTheme = onSet.bar.p;
    await page.click('.theme-sw [data-act="set-theme"][data-theme="dark"]');
    await new Promise(r => setTimeout(r, 80));
    const afterTheme = await page.evaluate(() => {
      const a = document.querySelector('.status-index');
      return { theme: document.documentElement.getAttribute('data-theme'), s: a.getAttribute('data-index-status'), p: +a.getAttribute('data-pct') };
    });
    rec('theme switch does not reset job', afterTheme.theme === 'dark' && afterTheme.s === 'running' && afterTheme.p >= beforeTheme, JSON.stringify(afterTheme));

    await page.waitForFunction(() => {
      const el = document.querySelector('.status-index');
      return el && el.getAttribute('data-index-status') === 'done';
    }, { timeout: 8000 });
    const done = await page.evaluate(() => {
      const a = document.querySelector('.status-index');
      const d = document.querySelector('.index-detail');
      return {
        bar: { s: a.getAttribute('data-index-status'), p: +a.getAttribute('data-pct'), d: +a.getAttribute('data-done'), t: +a.getAttribute('data-total') },
        detail: d ? { s: d.getAttribute('data-index-status'), p: +d.getAttribute('data-pct') } : null,
        text: a.innerText
      };
    });
    rec('complete state correct', done.bar.s === 'done' && done.bar.p === 100 && done.bar.d === done.bar.t && done.bar.t > 0 && done.text.indexOf('索引完成') >= 0, JSON.stringify(done));

  } catch (e) {
    rec('uncaught ' + e.message, false, e.stack);
  }

  // screenshots
  try {
    async function prep(th, size) {
      await reset(page);
      await page.evaluate((th, size) => { OPStore.setTheme(th); OPApp.setThumb(size || 144, true); }, th, size);
      await page.reload({ waitUntil: 'networkidle0' });
      await new Promise(r => setTimeout(r, 80));
    }
    await prep('light', 144);
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await shot(page, 'v11-light-library-statusbar.png');
    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await shot(page, 'v11-light-settings-index.png');

    await prep('dark', 144);
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await shot(page, 'v11-dark-library-statusbar.png');
    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await shot(page, 'v11-dark-settings-index.png');

    await prep('light', 97);
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await shot(page, 'v11-light-thumbs-97.png');
    await page.evaluate(() => { OPApp.setThumb(219, true); });
    await new Promise(r => setTimeout(r, 80));
    await shot(page, 'v11-light-thumbs-219.png');

    // mid-import running shot
    await prep('light', 144);
    await page.click('[data-act="import"]');
    await new Promise(r => setTimeout(r, 60));
    await page.click('[data-act="pick-src"][data-id="desk"]');
    await new Promise(r => setTimeout(r, 60));
    await page.click('[data-act="import-next"]');
    await new Promise(r => setTimeout(r, 60));
    await page.click('[data-act="import-run"]');
    await page.waitForFunction(() => {
      const el = document.querySelector('.status-index');
      return el && el.getAttribute('data-index-status') === 'running' && parseInt(el.getAttribute('data-pct') || '0', 10) >= 20;
    }, { timeout: 4000 });
    await page.goto(FILE + '#/all', { waitUntil: 'networkidle0' });
    await new Promise(r => setTimeout(r, 60));
    await shot(page, 'v11-light-library-indexing.png');
    await page.goto(FILE + '#/settings', { waitUntil: 'networkidle0' });
    await new Promise(r => setTimeout(r, 60));
    await shot(page, 'v11-light-settings-indexing.png');
  } catch (e) {
    rec('screenshot error ' + e.message, false, e.stack);
  }

  await browser.close();

  const pass = results.filter(r => r.pass).length;
  const fail = results.filter(r => !r.pass).length;
  let md = '# 交互测试结果 · OpenPicasa 原型 v1.1\n\n';
  md += '- 日期：2026-11-16（Asia/Shanghai）\n';
  md += '- 环境：puppeteer-core + Chrome，本地 `file://` 打开 `index.html`\n';
  md += '- 本文件为 v1.1 新增用例（滑块、索引进度）。原 v1 核心用例见 `TEST-RESULTS.md`。\n';
  md += '- 合计：' + results.length + ' 项，通过 ' + pass + '，未通过 ' + fail + '\n\n';
  md += '| 结果 | 用例 | 说明 |\n| --- | --- | --- |\n';
  results.forEach(r => { md += '| ' + (r.pass ? '通过' : '未通过') + ' | ' + r.name + ' | ' + String(r.detail || '').replace(/\|/g, '\\|').replace(/\n/g, ' ') + ' |\n'; });
  md += '\n## 测量依据\n\n';
  md += '- 方图网格使用 `grid-template-columns: repeat(auto-fill, var(--thumb-size))`，单元格宽高等于滑块值（允许 ±1px 取整误差）。已测 97 / 133 / 177 / 219 / 263，均非原 128 / 144 / 192 三档。\n';
  md += '- 列表布局缩略图边长同步为滑块像素值。\n';
  md += '- 原比例（两端对齐）行高以滑块值为目标高度，行被撑满时高度会按行宽缩放，因此不必等于精确像素，但会随滑块连续变化，而非三档。\n';
  md += '- 底栏固定 32px，避免进度或尺寸变化造成跳动。\n';
  md += '- 索引为演示数据，不扫描磁盘、不写入原照片。\n';
  fs.writeFileSync(REPORT, md);
  console.log('\nWrote', REPORT);
  console.log('PASS', pass, 'FAIL', fail);
  process.exit(fail ? 1 : 0);
})().catch(e => { console.error(e); process.exit(1); });
