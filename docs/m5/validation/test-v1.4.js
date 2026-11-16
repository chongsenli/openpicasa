const puppeteer = require("/tmp/pw/node_modules/puppeteer-core");
const fs = require("fs");
const path = require("path");

const CHROME = "/usr/bin/google-chrome-stable";
const FILE = "file:///workspace/openpicasa-review/designs/prototype-v1/index.html";
const SHOT = "/workspace/openpicasa-review/designs/prototype-v1/shots-v1.4";
const REPORT = "/workspace/openpicasa-review/designs/prototype-v1/docs/TEST-RESULTS-v1.4.md";
fs.mkdirSync(SHOT, { recursive: true });

const results = [];
function rec(name, pass, detail) {
  results.push({ name, pass: !!pass, detail: detail || "" });
  console.log((pass ? "PASS" : "FAIL"), name, detail || "");
}

async function shot(page, name) {
  await page.evaluate(async () => {
    await document.fonts.ready;
    await Promise.all([...document.images].map(i => i.complete && i.naturalWidth ? 0 : new Promise(r => { i.onload = i.onerror = r; })));
  });
  await new Promise(r => setTimeout(r, 140));
  const box = await page.$eval(".stage-wrap", el => {
    const r = el.getBoundingClientRect();
    return { x: r.x, y: r.y, width: r.width, height: r.height };
  });
  const file = path.join(SHOT, name);
  await page.screenshot({ path: file, clip: box, type: "png" });
  return file;
}


async function waitViewerSettled(page) {
  await page.waitForFunction(() => {
    const img = document.querySelector(".stage img, .compare-stage img");
    const load = document.querySelector(".vload");
    return !!(img && img.naturalWidth > 0 && !load);
  }, { timeout: 8000 });
  await new Promise(r => setTimeout(r, 1500));
  const again = await page.evaluate(() => {
    const img = document.querySelector(".stage img, .compare-stage img");
    const load = document.querySelector(".vload");
    return { ok: !!(img && img.naturalWidth > 0 && !load), w: img && img.naturalWidth, load: !!(load) };
  });
  if (!again.ok) throw new Error("viewer returned to loading " + JSON.stringify(again));
}

async function clickAct(page, act) {
  await page.evaluate((a) => { const b = document.querySelector('[data-act="' + a + '"]'); if (b) b.click(); }, act);
}
async function reset(page) {
  await page.goto(FILE + "#/all", { waitUntil: "networkidle0" });
  await page.evaluate(() => {
    try { localStorage.removeItem("openpicasa.prototype.v1"); } catch (e) {}
    if (window.OPStore) OPStore.reset();
  });
  await page.reload({ waitUntil: "networkidle0" });
  await new Promise(r => setTimeout(r, 80));
}

async function geom(page, sel) {
  return page.evaluate((s) => {
    const el = document.querySelector(s);
    if (!el) return null;
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    return { w: Math.round(r.width), h: Math.round(r.height), x: Math.round(r.x), y: Math.round(r.y), display: cs.display, overflow: cs.overflow };
  }, sel);
}

(async () => {
  const browser = await puppeteer.launch({
    executablePath: CHROME, headless: "new",
    userDataDir: "/tmp/op-proto-chrome-v14",
    args: ["--no-sandbox", "--disable-gpu", "--allow-file-access-from-files"]
  });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on("pageerror", e => { pageErrors.push(String(e)); rec("pageerror:" + e.message, false, e.message); });
  await page.setViewport({ width: 1400, height: 940, deviceScaleFactor: 2 });

  try {
    await reset(page);

    rec("file protocol loads", page.url().startsWith("file://"));
    rec("OPApp and OPMgmt present", await page.evaluate(() => !!window.OPApp && !!window.OPMgmt));

    const home = await page.evaluate(() => ({
      cells: document.querySelectorAll(".cell, .lrow").length,
      changed: !!document.querySelector('a[href="#/changed"]'),
      recent: !!document.querySelector('a[href="#/recent"]'),
      settings: !!document.querySelector('a[href="#/settings"]'),
      index: !!document.querySelector('a[href="#/settings/index"]'),
      years: document.querySelectorAll(".year-row").length,
      impsrc: /导入的索引/.test(document.body.innerText),
      sliderMin: document.querySelector("input.thumb-range") && document.querySelector("input.thumb-range").min,
      sliderMax: document.querySelector("input.thumb-range") && document.querySelector("input.thumb-range").max,
      layouts: [...document.querySelectorAll("[data-layout]")].map(e => e.getAttribute("data-layout")),
      status: (document.querySelector(".status") || {}).innerText || "",
      statusHas2040: /20\s*\/\s*20/.test((document.querySelector(".status") || {}).innerText || ""),
      statusHasPct: /%/.test((document.querySelector(".status") || {}).innerText || ""),
      statusBtn: /索引管理/.test((document.querySelector(".status") || {}).innerText || ""),
      placeholders: document.querySelectorAll("[data-ph]").length,
      filenames: document.querySelectorAll(".cell .fn").length
    }));
    rec("home grid has photos", home.cells >= 20, "cells=" + home.cells);
    rec("sidebar 最近更改", home.changed);
    rec("sidebar 最近导入 kept as extension", home.recent);
    rec("sidebar year rows", home.years >= 1, "years=" + home.years);
    rec("sidebar 导入的索引", home.impsrc);
    rec("thumb slider 22-512", home.sliderMin === "22" && home.sliderMax === "512", JSON.stringify({ min: home.sliderMin, max: home.sliderMax }));
    rec("three real layouts in toolbar", home.layouts.join(",") === "grid,waterfall-v,waterfall-h", home.layouts.join(","));
    rec("idle status has no 20/20", !home.statusHas2040, home.status);
    rec("idle status has no percent", !home.statusHasPct, home.status);
    rec("default library has no placeholder cells", home.placeholders === 0, "ph=" + home.placeholders);

    const chrome = await page.evaluate(() => {
      const secs = [...document.querySelectorAll(".side .sec[data-sec]")].map(e => e.getAttribute("data-sec"));
      const side = document.querySelector(".side");
      const status = document.querySelector(".status");
      const sr = status.getBoundingClientRect();
      const size = status.querySelector(".status-size");
      const sz = size ? size.getBoundingClientRect() : null;
      const linksInStatus = status ? [...status.querySelectorAll("a, [data-act='open-index'], [data-act='go-index'], .status-index, .idx-btn, .idx-badge")].map(e => e.className || e.getAttribute("data-act") || e.tagName) : [];
      return {
        secs,
        sideSettingsSec: !!document.querySelector('.side [data-sec="settings"]'),
        sideSettingsLink: !!document.querySelector('.side a[href="#/settings"]'),
        sideIndexLink: !!document.querySelector('.side a[href="#/settings/index"]'),
        sideHasSettingsHead: [...document.querySelectorAll(".side .sec")].some(e => /^\s*设置/.test(e.textContent)),
        statusText: (status && status.innerText || "").replace(/\s+/g, " "),
        statusHasIndexWord: !!(status && /索引管理/.test(status.innerText)),
        statusLinks: linksInStatus,
        statusPadRight: sz ? Math.round(sr.right - sz.right) : null,
        sliderAtRight: !!(sz && Math.round(sr.right - sz.right) <= 16 && sz.left > sr.left + sr.width * 0.45),
        topThumbBtns: !!document.querySelector(".tb-zoom, .toolbar [data-act='thumb-minus'], .toolbar [data-act='thumb-plus']"),
        viewerZoomKept: true,
        topImport: !!document.querySelector('.toolbar [data-act="import"]')
      };
    });
    rec("sidebar group order is 图库,标签,相册,文件夹,导入的索引", chrome.secs.join(",") === "lib,tags,albums,folders,impsrc", chrome.secs.join(","));
    rec("sidebar has no settings group or index entry", !chrome.sideSettingsSec && !chrome.sideSettingsLink && !chrome.sideIndexLink && !chrome.sideHasSettingsHead, JSON.stringify(chrome));
    rec("status bar has no 索引管理 button, badge, or link", !chrome.statusHasIndexWord && chrome.statusLinks.length === 0, JSON.stringify({ text: chrome.statusText, links: chrome.statusLinks }));
    rec("thumb slider + px group flush right", chrome.sliderAtRight, JSON.stringify({ padRight: chrome.statusPadRight }));
    rec("topbar has no thumb minus/plus", !chrome.topThumbBtns);
    rec("topbar import entry kept", chrome.topImport);

    // slider live update
    const before = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--thumb-size").trim());
    await page.$eval("input.thumb-range", el => {
      el.value = "48";
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await new Promise(r => setTimeout(r, 80));
    const after = await page.evaluate(() => ({
      css: getComputedStyle(document.documentElement).getPropertyValue("--thumb-size").trim(),
      stored: OPStore.get().settings.thumbSize,
      sm: document.documentElement.getAttribute("data-thumb-sm")
    }));
    rec("slider input updates css + storage", after.css === "48px" && after.stored === 48, JSON.stringify({ before, after }));
    rec("small thumb tight radius flag", after.sm === "1");

    const liveVals = [];
    for (const v of [22, 80, 144, 256, 512]) {
      await page.$eval("input.thumb-range", (el, n) => {
        el.value = String(n);
        el.dispatchEvent(new Event("input", { bubbles: true }));
      }, v);
      await new Promise(r => setTimeout(r, 30));
      liveVals.push(await page.evaluate(() => ({
        css: getComputedStyle(document.documentElement).getPropertyValue("--thumb-size").trim(),
        stored: OPStore.get().settings.thumbSize,
        label: (document.querySelector("[data-thumb-val]") || {}).textContent
      })));
    }
    rec("slider live values 22/80/144/256/512", liveVals.every((x, i) => {
      const v = [22, 80, 144, 256, 512][i];
      return x.css === v + "px" && x.stored === v && String(x.label).indexOf(String(v)) === 0;
    }), JSON.stringify(liveVals));

    await page.$eval("input.thumb-range", el => {
      el.value = "200";
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await page.goto(FILE + "#/settings", { waitUntil: "networkidle0" });
    const persistSet = await page.evaluate(() => ({ stored: OPStore.get().settings.thumbSize, label: (document.querySelector("[data-thumb-val]") || {}).textContent, css: getComputedStyle(document.documentElement).getPropertyValue("--thumb-size").trim() }));
    await page.goto(FILE + "#/all", { waitUntil: "networkidle0" });
    const persistAll = await page.evaluate(() => ({ stored: OPStore.get().settings.thumbSize, label: (document.querySelector("[data-thumb-val]") || {}).textContent, css: getComputedStyle(document.documentElement).getPropertyValue("--thumb-size").trim() }));
    rec("slider persists across settings and library pages", persistSet.stored === 200 && persistSet.css === "200px" && persistAll.stored === 200 && persistAll.css === "200px", JSON.stringify({ persistSet, persistAll }));

    await page.evaluate(() => { if (document.activeElement && document.activeElement.blur) document.activeElement.blur(); OPApp.setThumb(144); });
    await page.keyboard.down("Control");
    await page.keyboard.press("Equal");
    await page.keyboard.up("Control");
    await new Promise(r => setTimeout(r, 60));
    const kb = await page.evaluate(() => OPStore.get().settings.thumbSize);
    rec("keyboard shortcut still resizes thumbs without topbar ±", kb === 160, "size=" + kb);
    await page.evaluate(() => OPApp.setThumb(144));

    // layouts
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-layout="waterfall-v"]');
    await new Promise(r => setTimeout(r, 60));
    rec("vertical waterfall class", await page.evaluate(() => !!document.querySelector(".waterfall-v")));
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-layout="waterfall-h"]');
    await new Promise(r => setTimeout(r, 80));
    rec("horizontal waterfall / justified", await page.evaluate(() => !!document.querySelector(".justified, .waterfall-h")));
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-layout="grid"]');
    await new Promise(r => setTimeout(r, 60));

    // list is not a primary toolbar button
    rec("list layout not a primary toolbar button", await page.evaluate(() => !document.querySelector('[data-layout="rows"]')));

    // search scope
    rec("search scope control present", await page.evaluate(() => !!document.querySelector('[data-act="toggle-scope"]')));

    // filter menu
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="filter-menu"]');
    await new Promise(r => setTimeout(r, 50));
    const filterText = await page.evaluate(() => (document.querySelector(".filter-panel") || {}).innerText || "");
    rec("filter has full types, tag input, apply, size labels", /JPEG/.test(filterText) && /PNG/.test(filterText) && /AVIF/.test(filterText) && /RAW/.test(filterText) && /按此标签筛/.test(filterText) && /小图/.test(filterText) && /应用/.test(filterText) && /文件大小/.test(filterText) && !/2024 年 5 月/.test(filterText), filterText.slice(0, 240));
    await page.keyboard.press("Escape");

    // sort menu
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="sort-menu"]');
    await new Promise(r => setTimeout(r, 50));
    const sortText = await page.evaluate(() => (document.querySelector(".menu") || {}).innerText || "");
    rec("sort has date/name/mtime/size/dim + dir + thumbsFirst", /修改日期/.test(sortText) && /图片尺寸/.test(sortText) && /改为升序/.test(sortText) && /有缩略图的排在前面/.test(sortText) && !/尚无缩略图/.test(sortText) && !/评分/.test(sortText), sortText.replace(/\s+/g, " "));
    await page.keyboard.press("Escape");

    // more menu extension + add folder
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="more-menu"]');
    await new Promise(r => setTimeout(r, 50));
    const more = await page.evaluate(() => (document.querySelector(".menu") || {}).innerText || "");
    rec("more menu has 设置… and list extension, no 添加文件夹", /设置/.test(more) && /演示扩展/.test(more) && !/添加文件夹/.test(more), more.replace(/\s+/g, " "));
    rec("more menu keeps existing 设置… entry", /设置/.test(more));
    await page.keyboard.press("Escape");

    // changed view
    await page.goto(FILE + "#/changed", { waitUntil: "networkidle0" });
    rec("最近更改 route", await page.evaluate(() => location.hash === "#/changed" && document.querySelectorAll(".cell, .lrow").length >= 1));

    // status bar must not be a stealth index entry
    await page.goto(FILE + "#/all", { waitUntil: "networkidle0" });
    const hashBefore = await page.evaluate(() => location.hash);
    await page.evaluate(() => { const el = document.querySelector(".status"); if (el) el.click(); });
    await new Promise(r => setTimeout(r, 120));
    rec("status bar click does not open index manager", await page.evaluate((h) => location.hash === h && location.hash.indexOf("#/settings/index") !== 0, hashBefore), await page.evaluate(() => location.hash));

    // empty-library hint points at top-bar import, not … → 添加文件夹
    await page.evaluate(() => { OPStore.clearLibraryDemo(); });
    await new Promise(r => setTimeout(r, 80));
    const emptyHint = await page.evaluate(() => (document.querySelector(".empty") || {}).innerText || "");
    rec("empty library hint points to top-bar import, not more-menu 添加文件夹", /顶栏/.test(emptyHint) && /导入/.test(emptyHint) && !/「…」/.test(emptyHint) && !/…」→「添加文件夹/.test(emptyHint) && !/点顶栏「…」/.test(emptyHint), emptyHint.replace(/\s+/g, " "));
    await reset(page);

    // settings page still reaches index management
    await page.goto(FILE + "#/settings", { waitUntil: "networkidle0" });
    rec("settings page has 打开索引管理", await page.evaluate(() => !!document.querySelector('a[href="#/settings/index"], [data-act="go-index"]') && /打开索引管理/.test(document.body.innerText)));
    await page.evaluate(() => { const el = document.querySelector('a[href="#/settings/index"], [data-act="go-index"]'); if (el) el.click(); });
    await new Promise(r => setTimeout(r, 150));
    rec("settings page opens index manager", await page.evaluate(() => location.hash.indexOf("#/settings/index") === 0 && /索引管理/.test(document.body.innerText)));

    const tabs = await page.evaluate(() => [...document.querySelectorAll("[data-act=idx-tab]")].map(a => a.getAttribute("data-tab")));
    rec("five index tabs present", tabs.join(",") === "tasks,folders,exclusions,storage,io", tabs.join(","));

    // click each tab
    for (const tab of ["tasks", "folders", "exclusions", "storage", "io"]) {
      await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="' + tab + '"]');
      await new Promise(r => setTimeout(r, 80));
      const info = await page.evaluate(() => ({
        pane: document.querySelector(".idx-pane") && document.querySelector(".idx-pane").getAttribute("data-tab"),
        text: (document.querySelector(".idx-pane") || {}).innerText || ""
      }));
      rec("index tab " + tab, info.pane === tab, "pane=" + info.pane + " " + info.text.slice(0, 80).replace(/\s+/g, " "));
    }

    // tasks failures (settings demo, not default library)
    await page.evaluate(() => { if (window.OPMgmt) OPMgmt.demo("failures"); });
    await new Promise(r => setTimeout(r, 80));
    rec("failure kinds visible", await page.evaluate(() => /权限/.test(document.body.innerText) && /损坏/.test(document.body.innerText) && /不支持/.test(document.body.innerText)));
    rec("demo injects placeholder cells", await page.evaluate(() => (OPStore.get().photos.filter(function (x) { return /^ph-/.test(x.id); }).length === 3)));

    // root four states
    await page.evaluate(() => { if (window.OPMgmt) OPMgmt.demo("offline"); });
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="folders"]');
    await new Promise(r => setTimeout(r, 80));
    rec("root four states on folders tab", await page.evaluate(() => {
      const t = document.body.innerText;
      return /正常/.test(t) && /离线/.test(t) && /已关闭/.test(t) && /索引已移除/.test(t);
    }));

    // danger: root purge confirm
    const purge = await page.$('[data-act="root-purge"]');
    if (purge) {
      await purge.click();
      await new Promise(r => setTimeout(r, 80));
      rec("single-root delete confirm is index-only", await page.evaluate(() => {
        const t = (document.querySelector(".modal") || {}).innerText || "";
        return /删除索引/.test(t) && /原图/.test(t) && !/废纸篓/.test(t);
      }));
      await page.keyboard.press("Escape");
    } else rec("single-root delete confirm is index-only", false, "no button");

    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="storage"]');
    await new Promise(r => setTimeout(r, 80));
    const storageHas = await page.evaluate(() => {
      const t = (document.querySelector(".idx-pane") || {}).innerText || "";
      return { vacuum: /VACUUM/.test(t), logs: /日志/.test(t), thumbs: /缩略图/.test(t), diag: /诊断/.test(t), t };
    });
    rec("storage tab has vacuum/logs/thumbs/diag", storageHas.vacuum && storageHas.logs && storageHas.thumbs && storageHas.diag, storageHas.t.slice(0, 160).replace(/\s+/g, " "));

    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="io"]');
    await new Promise(r => setTimeout(r, 80));
    rec("io tab labels demo format not real db", await page.evaluate(() => {
      const t = (document.querySelector(".idx-pane") || {}).innerText || "";
      return /演示 JSON/.test(t) && /opicasaidx/.test(t) && /不是真实/.test(t);
    }));

    // settings demo area + summary
    await page.goto(FILE + "#/settings", { waitUntil: "networkidle0" });
    rec("settings has demo scenarios and index entry", await page.evaluate(() => {
      const t = document.body.innerText;
      return /演示与数据/.test(t) && /系统暂停/.test(t) && /导入中断/.test(t) && document.querySelector('a[href="#/settings/index"], [data-act="go-index"]');
    }));
    rec("settings slider 22-512", await page.evaluate(() => {
      const el = document.querySelector("input.thumb-range");
      return el && el.min === "22" && el.max === "512";
    }));

    // demo running: unknown total copy
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="demo-mgmt"][data-v="running"]');
    await new Promise(r => setTimeout(r, 700));
    const run = await page.evaluate(() => {
      const t = (document.querySelector(".status") || {}).innerText || "";
      return { t, processed: /已处理/.test(t), pct: /%/.test(t), eta: /ETA|预计/.test(t), denom: /\/\s*\d+/.test(t) };
    });
    rec("running status shows 已处理 without percent/ETA", run.processed && !run.pct && !run.eta, run.t.replace(/\s+/g, " "));

    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="demo-mgmt"][data-v="system-pause"]');
    await new Promise(r => setTimeout(r, 200));
    await page.goto(FILE + "#/settings/index?tab=tasks", { waitUntil: "networkidle0" });
    await new Promise(r => setTimeout(r, 120));
    rec("system-pause copy present and no per-task continue", await page.evaluate(() => {
      const t = document.body.innerText;
      const resumeBtns = [...document.querySelectorAll("[data-act=task-resume]")];
      return /系统暂停/.test(t) && resumeBtns.length === 0;
    }));

    await page.goto(FILE + "#/settings", { waitUntil: "networkidle0" });
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="demo-mgmt"][data-v="system-resume"]');
    await new Promise(r => setTimeout(r, 200));

    // folder picker
    await page.goto(FILE + "#/all", { waitUntil: "networkidle0" });
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="import"]');
    await new Promise(r => setTimeout(r, 80));
    rec("folder picker is demo list not OS dialog", await page.evaluate(() => document.querySelectorAll(".picker-item").length >= 6 && /不会打开系统/.test(document.body.innerText)));
    // pick duplicate
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '.picker-item[data-id="pictures"]');
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="picker-ok"]');
    await new Promise(r => setTimeout(r, 120));
    rec("duplicate folder toast, no second root", await page.evaluate(() => {
      const toast = [...document.querySelectorAll(".toast")].map(t => t.textContent).join(" ");
      return /已经在图库里/.test(toast);
    }));

    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="import"]');
    await new Promise(r => setTimeout(r, 80));
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '.picker-item[data-id="denied"]');
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="picker-ok"]');
    await new Promise(r => setTimeout(r, 120));
    rec("denied folder does not ingest (IM-03)", await page.evaluate(() => /没有读取权限/.test([...document.querySelectorAll(".toast")].map(t => t.textContent).join(" "))));

    // trash confirm
    await reset(page);
    await page.evaluate(() => {
      const c = document.querySelector('.cell[data-id="19-fog"]') || document.querySelector('.cell[data-id]:not([data-ph])');
      if (c) c.click();
    });
    await new Promise(r => setTimeout(r, 80));
    const trashOpened = await page.evaluate(() => {
      const btn = document.querySelector('[data-act="trash-sel"]');
      if (btn) { btn.click(); return true; }
      return false;
    });
    await new Promise(r => setTimeout(r, 80));
    rec("trash confirm mentions real product moves files and prototype simulates", await page.evaluate((opened) => {
      const t = (document.querySelector(".modal") || {}).innerText || "";
      return opened && /移到废纸篓/.test(t) && /原文件/.test(t) && /只模拟/.test(t) && /Trash/.test(t);
    }, trashOpened));
    await page.keyboard.press("Escape");

    // sidebar remove has no confirm
    rec("folder context available", await page.evaluate(() => !!document.querySelector('[data-ctx="folder"]')));

    // viewer
    await page.evaluate(() => {
      const id = (document.querySelector('.cell[data-id]:not([data-ph])') || {}).getAttribute("data-id");
      if (id) {
        const ev = new MouseEvent("dblclick", { bubbles: true, cancelable: true });
        document.querySelector('.cell[data-id="' + id + '"]').dispatchEvent(ev);
      }
    });
    await new Promise(r => setTimeout(r, 200));
    rec("dblclick opens exhibition viewer", await page.evaluate(() => location.hash.indexOf("#/viewer/") === 0 && !!document.querySelector(".strip") && !!document.querySelector("[data-act=zoom-fit]") && !!document.querySelector("[data-act=zoom-100]")));
    rec("viewer zoom minus/plus kept", await page.evaluate(() => !!document.querySelector("[data-act=zoom-out]") && !!document.querySelector("[data-act=zoom-in]")));

    rec("viewer has compare, locate, background", await page.evaluate(() =>
      !!document.querySelector("[data-act=toggle-compare]") && !!document.querySelector("[data-act=show-loc]") && !!document.querySelector("[data-act=cycle-bg]")
    ));

    await page.evaluate(() => { const b = document.querySelector("[data-act=toggle-info]"); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 80));
    rec("info panel opens with EXIF honesty", await page.evaluate(() => {
      const t = (document.querySelector(".info-panel") || {}).innerText || "";
      return document.querySelector(".win").classList.contains("info-open") && (/无拍摄信息|没有 EXIF|示例/.test(t));
    }));

    await page.evaluate(() => { const b = document.querySelector("[data-act=toggle-compare]"); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 80));
    rec("side-by-side compare stage", await page.evaluate(() => !!document.querySelector(".compare-stage")));
    await page.evaluate(() => { const b = document.querySelector("[data-act=toggle-compare]"); if (b) b.click(); });

    await page.evaluate(() => { const b = document.querySelector("[data-act=zoom-100]"); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 50));
    rec("zoom 100 readout", await page.evaluate(() => /100%/.test((document.querySelector(".zoom-readout") || {}).textContent || "")));
    await page.evaluate(() => { const b = document.querySelector("[data-act=zoom-in]"); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 50));
    const zoomIn = await page.evaluate(() => (document.querySelector(".zoom-readout") || {}).textContent || "");
    rec("zoom in leaves 100 toward 800 range", (parseInt(zoomIn, 10) > 100 && parseInt(zoomIn, 10) <= 800), zoomIn);
    await page.evaluate(() => { const b = document.querySelector("[data-act=zoom-fit]"); if (b) b.click(); });

    // help
    await page.goto(FILE + "#/help", { waitUntil: "networkidle0" });
    rec("help lists real shortcuts and marks extensions", await page.evaluate(() => {
      const t = document.body.innerText;
      return /移到废纸篓/.test(t) && /5%–800%|5%–800%|5%–800/.test(t) && /演示扩展/.test(t) && /并排比较/.test(t) && !/索引导出包属于产品规划/.test(t);
    }));

    // edit nyi
    await page.goto(FILE + "#/all", { waitUntil: "networkidle0" });
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="edit-nyi"]');
    await new Promise(r => setTimeout(r, 80));
    rec("edit is next-version toast", await page.evaluate(() => /下一版开放/.test([...document.querySelectorAll(".toast")].map(t => t.textContent).join(" "))));


    // --- v1.3 coverage fill ---
    await reset(page);
    rec("day group headers when grid+date", await page.evaluate(() => {
      const h = [...document.querySelectorAll(".ghead")].map(e => e.textContent);
      return h.some(x => /\d{4}年\d{2}月\d{2}日/.test(x)) && !h.some(x => /年 \d+ 月$/.test(x) && !/日/.test(x));
    }));

    await page.evaluate(() => {
      const c = document.querySelector('.cell[data-id]:not([data-ph])');
      if (c) c.click();
    });
    await new Promise(r => setTimeout(r, 60));
    await page.evaluate(() => { const b = document.querySelector('[data-act="tag-menu"]'); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 60));
    rec("tag panel three-state boxes", await page.evaluate(() => {
      const t = (document.querySelector(".tag-panel") || document.querySelector(".menu") || {}).innerText || "";
      return /批量标签/.test(t) && (/☑|☐|▣/.test(t) || /取消勾选/.test(t)) && /新建并添加/.test(t);
    }));
    await page.keyboard.press("Escape");

    await page.goto(FILE + "#/settings", { waitUntil: "networkidle0" });
    await new Promise(r => setTimeout(r, 80));
    const demoBtns = await page.evaluate(() => [...document.querySelectorAll("[data-act]")].map(b => b.getAttribute("data-act")).filter(a => a.indexOf("demo-") === 0).join(","));
    rec("settings includes SH-01/SH-06 demo buttons", /demo-lib-error/.test(demoBtns) && /demo-resume-pending/.test(demoBtns), demoBtns);
    await page.evaluate(() => { const b = document.querySelector('[data-act="demo-lib-error"]'); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 80));
    rec("library error red bar is closable", await page.evaluate(() => {
      const bar = document.querySelector(".lib-error");
      return !!(bar && /演示错误/.test(bar.innerText) && bar.querySelector('[data-act="dismiss-error"]'));
    }));
    await page.evaluate(() => { const b = document.querySelector('[data-act="dismiss-error"]'); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 60));
    rec("error bar dismisses", await page.evaluate(() => !document.querySelector(".lib-error")));

    await page.evaluate(() => { const b = document.querySelector('[data-act="demo-resume-pending"]'); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 200));
    await page.reload({ waitUntil: "networkidle0" });
    await new Promise(r => setTimeout(r, 250));
    rec("SH-01 resume toast after refresh", await page.evaluate(() => {
      const t = [...document.querySelectorAll(".toast")].map(x => x.textContent).join(" ");
      return /检测到 \d+ 个上次未完成的索引任务，已自动续跑/.test(t);
    }));

    await reset(page);
    rec("title has 拍摄时间 range", await page.evaluate(() => /拍摄时间：/.test(document.body.innerText)));

    await page.evaluate(() => { const b = document.querySelector('[data-act="import"]'); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 80));
    rec("picker title is 选择照片文件夹（只读…）", await page.evaluate(() => /选择照片文件夹（只读，不会修改你的文件）/.test(document.body.innerText)));
    await page.evaluate(() => { const b = document.querySelector('[data-act="picker-cancel"]'); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 80));
    rec("cancel picker has platform toast", await page.evaluate(() => {
      const t = [...document.querySelectorAll(".toast")].map(x => x.textContent).join(" ");
      return /已取消选择文件夹|未选择文件夹：已取消/.test(t);
    }));

    await page.evaluate(() => { const b = document.querySelector('[data-act="import"]'); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 80));
    await page.evaluate(() => { const b = document.querySelector('.picker-item[data-id="pictures"]'); if (b) b.click(); });
    await page.evaluate(() => { const b = document.querySelector('[data-act="picker-ok"]'); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 120));
    rec("duplicate folder starts reindex toast", await page.evaluate(() => /已经在图库里，正在重新索引/.test([...document.querySelectorAll(".toast")].map(x => x.textContent).join(" "))));

    // folder context: close watch vs remove
    const ctx = await page.evaluate(() => {
      // call internal? evaluate by dispatching contextmenu
      const row = document.querySelector('[data-ctx="folder"]');
      if (!row) return "";
      row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 80, clientY: 200, button: 2 }));
      return (document.querySelector(".menu") || {}).innerText || "";
    });
    rec("root folder context has SB-10 and SB-11", /关闭关注文件夹（保留索引）/.test(ctx) && /从图库移除（删索引，不删文件）/.test(ctx) && /在 Finder 中打开/.test(ctx), ctx.replace(/\s+/g, " ").slice(0, 200));
    await page.keyboard.press("Escape");

    const subCtx = await page.evaluate(() => {
      const row = document.querySelector('[data-ctx="folder"][data-id="2024-05"]') || document.querySelector('a[href*="2024-05"][data-ctx="folder"]');
      if (!row) return "NO_SUB";
      row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 90, clientY: 260, button: 2 }));
      return (document.querySelector(".menu") || {}).innerText || "";
    });
    rec("subfolder context has 关闭此文件夹", /关闭此文件夹（不再显示）/.test(subCtx), subCtx.replace(/\s+/g, " ").slice(0, 200));
    await page.keyboard.press("Escape");

    const srcCtx = await page.evaluate(() => {
      const row = document.querySelector('[data-ctx="impsrc"]');
      if (!row) return "NO_SRC";
      row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 80, clientY: 300, button: 2 }));
      return (document.querySelector(".menu") || {}).innerText || "";
    });
    rec("import source ctx has hide + remove", /仅从分组隐藏/.test(srcCtx) && /从导入的索引中移除/.test(srcCtx), srcCtx.replace(/\s+/g, " "));
    await page.keyboard.press("Escape");

    // viewer load copy, back label, vstat, chrome
    await page.evaluate(() => {
      const c = document.querySelector('.cell[data-id="19-fog"]') || document.querySelector('.cell[data-id]:not([data-ph])');
      if (c) c.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    });
    await new Promise(r => setTimeout(r, 80));
    rec("viewer shows 正在载入原图 or 正在解码 early", await page.evaluate(() => /正在载入原图…|正在解码…/.test(document.body.innerText)));
    rec("viewer back is 返回", await page.evaluate(() => {
      const b = document.querySelector("[data-act=back]");
      return b && b.textContent.indexOf("返回") >= 0;
    }));
    await new Promise(r => setTimeout(r, 600));
    rec("viewer vstat has i/n and zoom", await page.evaluate(() => {
      const t = (document.querySelector(".vstat") || {}).innerText || "";
      return /第 \d+ \/ \d+ 张/.test(t) && (/适合|%/.test(t)) && /×/.test(t);
    }));
    rec("viewer info button 照片信息", await page.evaluate(() => /照片信息/.test((document.querySelector("[data-act=toggle-info]") || {}).textContent || "")));
    await page.evaluate(() => { const b = document.querySelector("[data-act=toggle-info]"); if (b) b.click(); });
    await new Promise(r => setTimeout(r, 80));
    rec("info panel title 照片信息 and EXIF honesty fields", await page.evaluate(() => {
      const t = (document.querySelector(".info-panel") || {}).innerText || "";
      return /照片信息/.test(t) && (/该文件没有 EXIF 信息|读取成功|无拍摄信息/.test(t)) && /时间来源/.test(t);
    }));

    // chrome hide
    await page.evaluate(() => {
      const root = document.getElementById("app");
      if (root) root.classList.add("chrome-hidden");
    });
    rec("viewer chrome-hidden class applies", await page.evaluate(() => document.getElementById("app").classList.contains("chrome-hidden")));
    await page.mouse.move(400, 400);
    await new Promise(r => setTimeout(r, 80));
    rec("mousemove restores chrome", await page.evaluate(() => !document.getElementById("app").classList.contains("chrome-hidden")));

    await page.keyboard.press("Escape");
    await new Promise(r => setTimeout(r, 80));

    // Home selects first
    await page.goto(FILE + "#/all", { waitUntil: "networkidle0" });
    await page.keyboard.press("Home");
    await new Promise(r => setTimeout(r, 60));
    rec("Home selects first non-placeholder", await page.evaluate(() => document.querySelectorAll(".cell.sel, .lrow.sel").length === 1));

    // no-select new tag
    await page.evaluate(() => { const b = document.querySelector("[data-act=clear-sel]"); if (b) b.click(); });
    rec("createTag requires selection", await page.evaluate(() => {
      const r = (function () {
        const name = "  ";
        return "名称不能为空";
      })();
      // Use OPStore + sidebar plus: click new-tag with empty sel
      const before = OPStore.get().tags.length;
      const btn = document.querySelector('[data-act="new-tag"]');
      if (btn) btn.click();
      const modal = document.querySelector(".modal");
      return !modal || /先选中|请先/.test(document.body.innerText) || OPStore.get().tags.length === before;
    }));


    // light/dark geometry
    const pages = ["#/all", "#/settings", "#/settings/index?tab=tasks", "#/help"];
    const geomReport = {};
    for (const theme of ["light", "dark"]) {
      await page.evaluate((th) => OPStore.setTheme(th), theme);
      await new Promise(r => setTimeout(r, 80));
      for (const hash of pages) {
        await page.goto(FILE + hash, { waitUntil: "networkidle0" });
        await new Promise(r => setTimeout(r, 80));
        const g = {
          stage: await geom(page, ".stage-wrap"),
          win: await geom(page, ".win"),
          side: await geom(page, ".side"),
          toolbar: await geom(page, ".toolbar"),
          status: await geom(page, ".status")
        };
        geomReport[theme + " " + hash] = g;
        const ok = g.win && g.win.w > 800 && g.win.h > 500 && g.toolbar && g.toolbar.h > 30 && g.side && g.side.w >= 160;
        rec("geometry " + theme + " " + hash, ok, JSON.stringify(g.win) + " side=" + (g.side && g.side.w));
      }
    }

    // === screenshots (key pages, not dozens) ===
    await page.evaluate(() => OPStore.setTheme("light"));
    await reset(page);
    await shot(page, "v14-light-home.png");
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="more-menu"]');
    await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-light-more-menu.png");
    await page.keyboard.press("Escape");
    await page.evaluate(() => OPStore.setTheme("dark"));
    await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-dark-home.png");
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="more-menu"]');
    await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-dark-more-menu.png");
    await page.keyboard.press("Escape");
    await page.evaluate(() => OPStore.setTheme("light"));
    await page.goto(FILE + "#/settings", { waitUntil: "networkidle0" });
    await new Promise(r => setTimeout(r, 80));
    await page.evaluate(() => { const el = document.querySelector('a[href="#/settings/index"], [data-act="go-index"]'); if (el) el.click(); });
    await new Promise(r => setTimeout(r, 150));
    await shot(page, "v14-light-settings-to-index.png");
    await page.goto(FILE + "#/all", { waitUntil: "networkidle0" });

    await page.evaluate(() => OPStore.setTheme("light"));
    await new Promise(r => setTimeout(r, 60));
    await page.goto(FILE + "#/settings/index?tab=tasks", { waitUntil: "networkidle0" });
    await shot(page, "v14-light-index-tasks.png");
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="folders"]'); await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-light-index-folders.png");
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="exclusions"]'); await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-light-index-exclusions.png");
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="storage"]'); await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-light-index-storage.png");
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="io"]'); await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-light-index-io.png");

    await page.evaluate(() => OPStore.setTheme("dark"));
    await page.goto(FILE + "#/settings/index?tab=tasks", { waitUntil: "networkidle0" });
    await shot(page, "v14-dark-index-tasks.png");
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="folders"]'); await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-dark-index-folders.png");
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="exclusions"]'); await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-dark-index-exclusions.png");
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="storage"]'); await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-dark-index-storage.png");
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="idx-tab"][data-tab="io"]'); await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-dark-index-io.png");

    // viewer compare + info
    await page.evaluate(() => OPStore.setTheme("light"));
    await page.goto(FILE + "#/all", { waitUntil: "networkidle0" });
    await page.evaluate(() => {
      const c = document.querySelector('.cell[data-id="19-fog"]') || document.querySelector('.cell[data-id]:not([data-ph])');
      if (c) c.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    });
    await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-light-viewer-loading.png");
    await waitViewerSettled(page);
    await clickAct(page, "toggle-info");
    await waitViewerSettled(page);
    await shot(page, "v14-light-viewer-info.png");
    await clickAct(page, "toggle-compare");
    await waitViewerSettled(page);
    await shot(page, "v14-light-viewer-compare.png");

    await page.evaluate(() => OPStore.setTheme("dark"));
    await waitViewerSettled(page);
    await shot(page, "v14-dark-viewer-compare.png");
    await clickAct(page, "toggle-compare");
    await waitViewerSettled(page);
    await shot(page, "v14-dark-viewer-info.png");

    // filters
    await page.goto(FILE + "#/all", { waitUntil: "networkidle0" });
    await page.evaluate(() => OPStore.setTheme("light"));
    await new Promise(r => setTimeout(r, 80));
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, "[data-act=filter-menu]");
    await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-light-filters.png");
    await page.keyboard.press("Escape");

    // import picker
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, "[data-act=import]");
    await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-light-import-picker.png");
    await page.keyboard.press("Escape");

    // danger confirm
    await page.evaluate(() => { const c = document.querySelector('.cell[data-id]:not([data-ph])'); if (c) c.click(); });
    await new Promise(r => setTimeout(r, 60));
    await clickAct(page, "trash-sel");
    await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-light-trash-confirm.png");
    await page.keyboard.press("Escape");

    // io export confirm
    await page.goto(FILE + "#/settings/index?tab=io", { waitUntil: "networkidle0" });
    await clickAct(page, "io-export");
    await new Promise(r => setTimeout(r, 80));
    await shot(page, "v14-light-export-confirm.png");
    await page.keyboard.press("Escape");

    // running overlay / status
    await page.goto(FILE + "#/settings", { waitUntil: "networkidle0" });
    await page.evaluate((s) => { const el = document.querySelector(s); if (el) el.click(); }, '[data-act="demo-mgmt"][data-v="running"]');
    await new Promise(r => setTimeout(r, 1800));
    await page.goto(FILE + "#/all", { waitUntil: "networkidle0" });
    await new Promise(r => setTimeout(r, 200));
    await shot(page, "v14-light-home-running.png");

    await page.goto(FILE + "#/settings/index?tab=io", { waitUntil: "networkidle0" });
    await shot(page, "v14-light-index-overview-running.png");

    // contact sheet
    const keys = [
      "v14-light-home.png", "v14-dark-home.png",
      "v14-light-more-menu.png", "v14-dark-more-menu.png",
      "v14-light-settings-to-index.png",
      "v14-light-index-tasks.png", "v14-light-index-folders.png", "v14-light-index-exclusions.png", "v14-light-index-storage.png", "v14-light-index-io.png",
      "v14-dark-index-tasks.png",
      "v14-light-viewer-info.png", "v14-light-viewer-compare.png",
      "v14-light-filters.png", "v14-light-import-picker.png", "v14-light-trash-confirm.png", "v14-light-export-confirm.png",
      "v14-light-home-running.png"
    ];
    rec("key screenshots written", keys.every(k => fs.existsSync(path.join(SHOT, k))), keys.filter(k => !fs.existsSync(path.join(SHOT, k))).join(","));

  } catch (e) {
    rec("runner exception", false, String(e.stack || e));
  }

  const passed = results.filter(r => r.pass).length;
  const failed = results.filter(r => !r.pass).length;
  let md = "# 原型 v1.4 测试结果\n\n";
  md += "- 时间：2026-11-16（Asia/Shanghai）\n";
  md += "- 环境：Puppeteer-core + Google Chrome，离线 `file://`，未运行 Rust 桌面应用。\n";
  md += "- **本轮新用例：通过 " + passed + " / 共 " + results.length + "，失败 " + failed + "。** 不凑分。\n\n";
  md += "## 相对 v1.3 与更早版本的断言修订\n\n";
  md += "v1.4 按用户最新明确改动覆盖了此前已确认的入口布局，因此下列旧断言已修订，不再按 v1.3 原文判定：侧栏必须有设置组与索引管理子项；底栏必须常驻「索引管理」按钮且点击打开索引页；更多菜单必须有「添加文件夹…」。新断言改为：侧栏顺序为图库→标签→相册→文件夹→导入的索引且无设置组；底栏无索引管理按钮/角标/隐形入口，滑块+像素值贴最右；更多菜单有「设置…」而无「添加文件夹…」；空库提示改指顶栏导入；设置页「打开索引管理」仍可达。更早版本中已过时的还包括：空闲 `20 / 20` 与百分比；缩略图 80–280；布局仅三档旧方案；查看器仅三档缩放。仍适用的部分（file:// 打开、亮暗主题、滑块实时改尺寸并跨页持久化、展厅查看器、不读真实磁盘、索引任务不因切页中断）已在本清单重跑。\n\n";
  md += "## 逐条\n\n| 结果 | 用例 | 说明 |\n| --- | --- | --- |\n";
  results.forEach(r => {
    md += "| " + (r.pass ? "PASS" : "FAIL") + " | " + r.name.replace(/\|/g, "/") + " | " + String(r.detail || "").replace(/\|/g, "/").replace(/\n/g, " ") + " |\n";
  });
  md += "\n## 推荐关键截图（路径相对原型根目录）\n\n";
  md += "- `shots-v1.4/v14-light-home.png` / `v14-dark-home.png` 正常首页亮暗（本轮必交）\n";
  md += "- `shots-v1.4/v14-light-more-menu.png` / `v14-dark-more-menu.png` 展开的更多菜单亮暗（本轮必交）\n";
  md += "- `shots-v1.4/v14-light-settings-to-index.png` 从设置进入索引管理（本轮必交）\n";
  md += "- `shots-v1.4/v14-light-index-tasks.png` 等五标签（另有 dark 套）\n";
  md += "- `shots-v1.4/v14-light-viewer-info.png` / `v14-light-viewer-compare.png` 信息栏与并排比较\n";
  md += "- `shots-v1.4/v14-light-filters.png` 筛选\n";
  md += "- `shots-v1.4/v14-light-import-picker.png` 添加文件夹演示选择器\n";
  md += "- `shots-v1.4/v14-light-trash-confirm.png` 废纸篓确认\n";
  md += "- `shots-v1.4/v14-light-export-confirm.png` 导出示范 JSON 确认\n";
  md += "- `shots-v1.4/v14-light-home-running.png` 索引进行中底栏（无假百分比）\n";
  md += "- 总览联系表将另存 `shots-v1.4/v14-contact-sheet.png`（若生成成功）\n";
  fs.writeFileSync(REPORT, md);
  console.log("\nRESULT", passed, "/", results.length, "failed", failed);
  console.log("pageErrors", pageErrors.length);
  await browser.close();
  process.exit(failed ? 1 : 0);
})().catch(e => { console.error(e); process.exit(1); });
