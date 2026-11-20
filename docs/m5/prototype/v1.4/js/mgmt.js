/* Index management, progress overlay, and import/export for the offline prototype.
   Simulates application behavior; never scans a real disk or writes original files. */
(function (g) {
  function S() { return OPStore.get(); }
  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, function (c) {
      return ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c];
    });
  }
  function toast(m) { if (g.OPApp && OPApp.toast) OPApp.toast(m); }
  function render() { if (g.OPApp && OPApp.render) OPApp.render(); else OPStore.persist(); }
  function go(h) { if (g.OPApp && OPApp.go) OPApp.go(h); else location.hash = h; }

  function bytes(n) {
    n = Math.max(0, n || 0);
    if (n < 1024) return n + " B";
    if (n < 1024 * 1024) return Math.round(n / 1024) + " KB";
    if (n < 1024 * 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + " MB";
    return (n / (1024 * 1024 * 1024)).toFixed(2) + " GB";
  }
  function mmss(ms) {
    ms = Math.max(0, ms || 0);
    const s = Math.floor(ms / 1000);
    const m = Math.floor(s / 60);
    return String(m).padStart(2, "0") + ":" + String(s % 60).padStart(2, "0");
  }
  function relCoarse(ts) {
    if (!ts) return "从未";
    const d = Date.now() - ts;
    if (d < 0) return "刚刚";
    if (d < 60000) return "刚刚";
    if (d < 3600000) return Math.floor(d / 60000) + " 分钟前";
    if (d < 86400000) return Math.floor(d / 3600000) + " 小时前";
    if (d < 30 * 86400000) return Math.floor(d / 86400000) + " 天前";
    return "30 天前以上";
  }
  function relFine(ts) {
    if (!ts) return "刚刚";
    const d = Date.now() - ts;
    if (d < 60000) return (d < 1000 ? "刚刚" : Math.max(1, Math.floor(d / 1000)) + " 秒前");
    if (d < 3600000) return Math.floor(d / 60000) + " 分钟前";
    return Math.floor(d / 3600000) + " 小时前";
  }

  function failKinds(m) {
    return OPData.FAIL_KINDS.map(function (k) {
      const row = (m.failures && m.failures[k.id]) || { count: 0, ignored: 0, ids: [] };
      return Object.assign({}, k, row);
    });
  }
  function unignoredFailTasks(m) {
    const hist = ((m.tasks && m.tasks.history) || []).filter(function (t) {
      return (t.failed || 0) > 0 && t.outcome !== "已取消";
    });
    const run = ((m.tasks && m.tasks.running) || []).filter(function (t) { return (t.failed || 0) > 0; });
    const allIgnored = failKinds(m).every(function (k) { return k.count === 0; });
    if (allIgnored) return 0;
    return hist.length + run.length;
  }
  function invalidExclusions(m) {
    return (m.exclusions || []).filter(function (e) { return e.invalid; }).length;
  }
  function offlineOpenRoots(m) {
    return (m.roots || []).filter(function (r) { return r.status === "offline"; }).length;
  }
  function degradedWatches(m) {
    return (m.roots || []).filter(function (r) {
      return r.status === "normal" && r.watch && r.watch !== "live" && r.watch !== "none";
    }).length;
  }
  function badge(m) {
    m = m || (S().mgmt || {});
    return unignoredFailTasks(m) + invalidExclusions(m) + offlineOpenRoots(m) + degradedWatches(m);
  }
  function badgeSentences(m) {
    const lines = [];
    const a = unignoredFailTasks(m), b = invalidExclusions(m), c = offlineOpenRoots(m);
    if (a) lines.push(a + " 个任务有失败项");
    if (b) lines.push(b + " 条排除项已失效");
    if (c) lines.push(c + " 个根目录离线");
    return lines;
  }
  function badgeTooltip(m) {
    m = m || (S().mgmt || {});
    const lines = badgeSentences(m);
    if (m.verify) lines.push("启动校验：" + m.verify.kind + "：已检查 " + m.verify.dirs + " 目录 / " + m.verify.files + " 文件");
    return lines.join("\n");
  }
  function defaultTab(m) {
    m = m || (S().mgmt || {});
    if (unignoredFailTasks(m) > 0) return "tasks";
    if (offlineOpenRoots(m) > 0) return "folders";
    if (invalidExclusions(m) > 0) return "exclusions";
    return "tasks";
  }
  function runningTask(m) {
    m = m || (S().mgmt || {});
    const list = (m.tasks && m.tasks.running) || [];
    return list[0] || null;
  }
  function statusKind(m) {
    m = m || (S().mgmt || {});
    const b = badge(m);
    if (b > 0) return "attention";
    if (m.pausedBySystem) return "paused";
    if (runningTask(m) && !m.globalPaused) return "running";
    if (runningTask(m) && m.globalPaused) return "paused";
    return "idle";
  }
  function allPhotos(m) {
    return (m.roots || []).reduce(function (n, r) {
      return n + (r.status === "closed" || r.status === "removed" ? 0 : (r.photos || 0));
    }, 0);
  }
  function lastUpdate(m) {
    let t = 0;
    (m.roots || []).forEach(function (r) { if (r.lastIndexed > t) t = r.lastIndexed; });
    ((m.tasks && m.tasks.running) || []).forEach(function (x) { if (x.startedAt > t) t = x.startedAt; });
    ((m.tasks && m.tasks.history) || []).forEach(function (x) { if ((x.endedAt || x.startedAt) > t) t = x.endedAt || x.startedAt; });
    return t;
  }
  function indexStateLabel(m) {
    const t = runningTask(m);
    if (m.pausedBySystem) return "暂停";
    if (m.globalPaused && t) return "暂停";
    if (t) return t.state || "扫描中";
    return "空闲";
  }
  function rootSummaryLine(r) {
    const stt = OPData.ROOT_STATUS[r.status] || r.status;
    let s = stt + " · " + (r.photos || 0) + " 张";
    if (r.thumbsBytes) s += " · 缩略图 " + bytes(r.thumbsBytes);
    if (r.failed > 0) s += " · 失败 " + r.failed;
    return s;
  }
  function syncIndexJob() {
    OPStore.patch(function (st) {
      const m = st.mgmt;
      const t = runningTask(m);
      const kind = statusKind(m);
      st.indexJob = st.indexJob || OPData.defaultIndexJob();
      st.indexJob.status = kind === "attention" && t ? "running" : (kind === "attention" ? "idle" : kind === "running" ? "running" : kind === "paused" ? "paused" : "idle");
      if (t) {
        st.indexJob.label = t.scope;
        st.indexJob.done = t.discovered || 0;
        st.indexJob.elapsed = Date.now() - (t.startedAt || Date.now());
        st.indexJob.note = "演示索引进行中，不会扫描磁盘，也不会改写原文件。";
        delete st.indexJob.total;
        delete st.indexJob.pct;
      } else if (kind !== "running") {
        st.indexJob.label = "";
        st.indexJob.done = 0;
        st.indexJob.elapsed = 0;
        st.indexJob.note = "演示索引，未扫描真实磁盘，原文件未被写入。";
        delete st.indexJob.total;
        st.indexJob.pct = 0;
      }
    }, true);
  }

  var tickTimer = null;
  function scheduleTick() {
    if (tickTimer) return;
    tickTimer = setTimeout(function () {
      tickTimer = null;
      tick();
    }, 280);
  }
  function tick() {
    const st = S();
    const m = st.mgmt;
    if (!m) return;
    if (m.pausedBySystem || m.globalPaused) { maybeOverlay(); return; }
    const running = (m.tasks.running || []).filter(function (t) { return t.userPaused !== true && t.taskId; });
    if (!running.length) {
      maybeOverlay();
      if (m.io && m.io.running) tickIo();
      return;
    }
    OPStore.patch(function (s) {
      (s.mgmt.tasks.running || []).forEach(function (t) {
        if (t.userPaused || !t.taskId) return;
        if (s.mgmt.pausedBySystem || s.mgmt.globalPaused) return;
        if (t.state === "等待") t.state = "扫描中";
        else if (t.state === "扫描中" && (t.discovered || 0) > 8) t.state = "生成缩略图中";
        t.discovered = (t.discovered || 0) + 1 + Math.floor(Math.random() * 2);
        t.elapsed = Date.now() - t.startedAt;
        const target = t.targetCount || 24;
        if (t.discovered >= target) finishTask(s, t, t.willFail ? "部分失败" : "成功");
      });
    });
    syncIndexJob();
    maybeOverlay();
    render();
    if (runningTask(S().mgmt)) scheduleTick();
    if (S().mgmt.io && S().mgmt.io.running) tickIo();
  }
  function finishTask(st, t, outcome) {
    t.state = outcome === "部分失败" ? "部分失败" : (outcome === "失败" ? "不可访问" : "已完成");
    t.outcome = outcome;
    t.completed = Math.max(0, (t.discovered || 0) - (t.willFail ? 2 : 0));
    t.skipped = 0;
    t.failed = t.willFail ? 2 : 0;
    t.endedAt = Date.now();
    st.mgmt.tasks.running = st.mgmt.tasks.running.filter(function (x) { return x.id !== t.id; });
    st.mgmt.tasks.history = [t].concat(st.mgmt.tasks.history).slice(0, 50);
    const root = (st.mgmt.roots || []).find(function (r) { return r.path === t.scope; });
    if (root && root.status !== "removed") root.lastIndexed = Date.now();
    hideOverlayIfClean(st);
    toast(outcome === "成功" ? "演示索引完成。未扫描真实磁盘，原文件未改动。" : ("索引任务结束：" + outcome + "（演示，未改原文件）"));
  }
  function startTask(opts) {
    const st = S();
    if (st.mgmt.pausedBySystem) { toast("系统暂停中（休眠 / 低电量），恢复由系统事件自动发生"); return; }
    const job = {
      id: (st.mgmt.tasks.nextId || 10),
      taskId: opts.pending ? 0 : (st.mgmt.tasks.nextId || 10),
      scope: opts.scope,
      kind: opts.kind || "刷新",
      state: "等待",
      discovered: 0, completed: 0, skipped: 0, failed: 0,
      startedAt: Date.now(),
      userPaused: false,
      willFail: !!opts.willFail,
      targetCount: opts.target || 22,
      folderName: (opts.scope || "").split("/").pop()
    };
    OPStore.patch(function (s) {
      s.mgmt.tasks.nextId = job.id + 1;
      if (opts.pending) job.taskId = 0;
      s.mgmt.tasks.running.unshift(job);
      s.mgmt.overlay.batchId = "b-" + job.id;
      s.mgmt.overlay.dismissed = false;
      s.mgmt.overlay.shown = false;
      s.mgmt.overlay.startedAt = Date.now();
      s.mgmt.folderJobs = [{
        id: job.id, name: job.folderName || job.scope, path: job.scope,
        state: "等待", discovered: 0, completed: 0, failed: 0, startedAt: job.startedAt, running: true
      }].concat((s.mgmt.folderJobs || []).filter(function (f) { return !f.running; }).slice(0, 8));
    });
    if (opts.pending) {
      setTimeout(function () {
        OPStore.patch(function (s) {
          const t = (s.mgmt.tasks.running || []).find(function (x) { return x.id === job.id; });
          if (t) t.taskId = t.id;
        });
      }, 400);
    }
    syncIndexJob();
    scheduleTick();
    render();
    return job;
  }
  function maybeOverlay() {
    const st = S();
    const m = st.mgmt;
    const t = runningTask(m);
    const age = Date.now() - (m.overlay.startedAt || 0);
    if (t && !m.overlay.dismissed && age >= 1500 && !m.overlay.shown) {
      OPStore.patch(function (s) { s.mgmt.overlay.shown = true; }, false);
    }
  }
  function hideOverlayIfClean(st) {
    const m = st.mgmt;
    const hasFail = failKinds(m).some(function (k) { return k.count > 0; });
    const t = runningTask(m);
    if (t || m.pausedBySystem) return;
    if (hasFail && m.overlay.shown) return;
    if (m.overlay.shown) {
      setTimeout(function () {
        OPStore.patch(function (s) {
          if (!runningTask(s.mgmt) && !s.mgmt.pausedBySystem) s.mgmt.overlay.shown = false;
        });
      }, 1000);
    }
  }


  var ioTimer = null;
  function tickIo() {
    const io = S().mgmt.io;
    if (!io || !io.running) return;
    OPStore.patch(function (s) {
      const x = s.mgmt.io;
      if (!x.running) return;
      if (x.known) {
        x.done = Math.min(x.total, (x.done || 0) + Math.max(1, Math.round((x.total || 10) / 12)));
        if (x.done >= x.total) completeIo(s, x);
      } else {
        x.done = (x.done || 0) + 3;
        if (x.phase === "正在校验索引文件" && x.done > 18) x.phase = "正在校验索引文件并准备导入";
        if (x.phase.indexOf("校验") >= 0 && x.done > 28) finishValidate(s, x);
      }
    });
    render();
    if (S().mgmt.io.running) ioTimer = setTimeout(tickIo, 220);
  }
  function finishValidate(st, x) {
    const pack = OPData.DEMO_PACKS.find(function (p) { return p.id === x.packId; }) || OPData.DEMO_PACKS[0];
    x.running = false;
    x.state = "idle";
    x.preview = {
      app: pack.app, format: pack.version, db: pack.dbVersion,
      photos: pack.photos, roots: pack.roots, folders: pack.folders, tags: pack.tags, albums: pack.albums,
      dups: pack.dups, unmatched: pack.unmatched, treeSrc: pack.treeSrc, thumbs: true
    };
    x.step = 1; x.phase = ""; x.message = "校验通过";
    toast("索引文件校验通过（演示包，不是真实数据库格式）。");
  }
  function completeIo(st, x) {
    x.running = false;
    if (x.kind === "export") {
      x.state = "success";
      x.message = "已导出演示索引摘要（JSON，不是真实 .opicasaidx 数据库）";
      x.lastBackup = { path: "~/Desktop/photo-index-demo.json", sha: "3f9c1a8e2b70", time: Date.now(), thumbs: !!x.includeThumbs, missing: false };
      x.phase = "";
      downloadDemo("photo-index-demo.json", demoExportPayload(x.includeThumbs), "application/json");
      toast("已导出演示文件 photo-index-demo.json（标清示例，非真实索引包格式）。");
    } else if (x.kind === "import") {
      x.state = "success";
      const pack = OPData.DEMO_PACKS.find(function (p) { return p.id === x.packId; }) || OPData.DEMO_PACKS[0];
      const imported = pack.photos;
      const ids = [];
      const pool = st.photos.filter(function (p) { return p.file; });
      for (let i = 0; i < Math.min(4, imported); i++) {
        const src = pool[i % pool.length];
        const id = "batch-" + Date.now() + "-" + i;
        st.photos.unshift({
          id: id, file: src.file, title: "本次导入 " + (i + 1), w: src.w, h: src.h, bytes: src.bytes,
          date: "2024-05-23", time: "12:" + String(10 + i).padStart(2, "0"), folder: "2024-05",
          starred: false, rating: 0, tags: [], albums: [], recent: true, offline: false,
          source: "pictures", thumbState: "ready", format: "JPEG", hasExif: false,
          changedAt: "2024-05-23T12:10:00", demoImport: true
        });
        ids.push(id);
      }
      st.ui.libraryEmpty = false;
      x.result = { imported: imported, skipped: 1, failed: 0, conflict: pack.dups || 0, ids: ids, errors: [] };
      x.message = "导入完成（演示数据，未写原文件）";
      x.step = 4;
      st.ui.importBatchIds = ids;
    }
  }
  function demoExportPayload(thumbs) {
    const st = S();
    return JSON.stringify({
      warning: "这是 OpenPicasa 交互原型导出的演示 JSON，不是真实的 .opicasaidx / 数据库格式，不能被正式应用导入。不含原始照片。",
      prototype: "v1.3",
      includeThumbsMention: !!thumbs,
      photos: st.photos.filter(function (p) { return p.thumbState === "ready" && p.file; }).length,
      roots: (st.mgmt.roots || []).map(function (r) { return { path: r.path, status: r.status, photos: r.photos }; }),
      exportedAt: new Date().toISOString()
    }, null, 2);
  }
  function downloadDemo(name, text, mime) {
    try {
      const blob = new Blob([text], { type: mime || "text/plain;charset=utf-8" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = name;
      document.body.appendChild(a);
      a.click();
      setTimeout(function () { URL.revokeObjectURL(a.href); a.remove(); }, 500);
    } catch (e) {
      toast("浏览器限制了演示文件下载。内容未写入照片目录。");
    }
  }
  function confirmIndex(opts) {
    if (g.OPApp && OPApp.confirmIndex) return OPApp.confirmIndex(opts);
    if (g.OPApp && OPApp.confirmDlg) return OPApp.confirmDlg(opts);
  }
  function countOrPending(n) { return (n == null) ? "待计算" : String(n); }

  function statusHtml() {
    const st = S();
    const m = st.mgmt || OPData.seedMgmt();
    const kind = statusKind(m);
    const t = runningTask(m);
    const io = m.io || {};
    let pill = "";
    if (io.running && io.kind) {
      const known = !!io.known && io.total > 0;
      const label = known
        ? ((io.phase || "正在导出") + " · " + (io.done || 0) + " / " + io.total)
        : ((io.phase || "正在校验索引文件") + " · 已处理 " + (io.done || 0));
      const w = known ? Math.min(100, Math.round((io.done || 0) / io.total * 100)) : 0;
      pill = '<div class="io-pill' + (known ? "" : " unknown") + '" title="' + esc(label) + '">' +
        '<div class="io-t">' + esc(label) + '</div><div class="io-bar"><i style="' + (known ? ("width:" + w + "%") : "") + '"></i></div></div>';
    }
    let prog = "";
    if (t) {
      const elapsed = Date.now() - (t.startedAt || Date.now());
      const spin = (kind === "running") ? '<i class="idx-spin" aria-hidden="true"></i>' : "";
      prog = '<span class="idx-status is-' + kind + '" data-index-status="' + kind + '" data-done="' + (t.discovered || 0) + '">' +
        spin + '<span class="idx-prog">已处理 ' + (t.discovered || 0) + " 张 · 已用 " + mmss(elapsed) + "</span></span>";
    }
    if (!pill && !prog) return "";
    return '<div class="status-left">' + pill + prog + "</div>";
  }
  function settingsSummaryHtml() {
    const m = S().mgmt || {};
    const t = runningTask(m);
    const kind = statusKind(m);
    const label = t
      ? ("● " + (t.state || "扫描中") + " · 已处理 " + (t.discovered || 0) + " 张 · 已用 " + mmss(Date.now() - t.startedAt))
      : ("● " + indexStateLabel(m));
    const chip = kind === "running" ? "run" : kind === "paused" ? "offline" : kind === "attention" ? "offline" : "";
    const word = kind === "attention" ? "需要处理" : kind === "running" ? "进行中" : kind === "paused" ? (m.pausedBySystem ? "系统暂停" : "已暂停") : "空闲";
    return '<div class="index-detail" data-index-status="' + kind + '" data-done="' + (t ? (t.discovered || 0) : 0) + '">' +
      '<div class="set-row"><div class="desc"><b>索引状态</b><span>' + esc(label) +
      "</span></div><span class=\"st-chip " + chip + '">' + esc(word) + "</span></div>" +
      (t ? '<p class="note">已处理 ' + (t.discovered || 0) + " 张 · 已用 " + mmss(Date.now() - t.startedAt) + "（总数未知，不显示百分比或 ETA）</p>" : '<p class="note">演示索引，未扫描真实磁盘，原文件未被写入。关闭本页不会取消后台任务。</p>') +
      "</div>";
  }

  const TABS = [
    { id: "tasks", label: "任务" },
    { id: "folders", label: "关注文件夹" },
    { id: "exclusions", label: "排除项" },
    { id: "storage", label: "存储与诊断" },
    { id: "io", label: "导入 / 导出" }
  ];
  function tabId() { return S().mgmt.tab || "tasks"; }
  function overviewHtml(m) {
    const roots = m.roots || [];
    let rootVal;
    if (!roots.length) rootVal = '<span class="warn-c">尚未添加图库根目录</span>';
    else rootVal = roots.map(function (r) {
      const cls = r.status === "offline" ? "danger-c" : (r.status === "removed" || r.status === "closed" ? "warn-c" : "");
      return '<div class="ov-root ' + cls + '">' + esc(r.path) + " · " + esc(rootSummaryLine(r)) + "</div>";
    }).join("");
    const t = runningTask(m);
    const stLine = t ? ("● " + (t.state || "扫描中")) : "● 空闲";
    const b = badge(m);
    const alert = (b > 0) ? '<div class="alert-line">⚠ ' + badgeSentences(m).join(" · ") + "</div>" : "";
    const lu = lastUpdate(m);
    return '<div class="ov">' +
      '<div class="ov-row"><div class="k">图库根目录</div><div class="v">' + rootVal + "</div></div>" +
      '<div class="ov-row"><div class="k">索引状态</div><div class="v">' + esc(stLine) + "</div></div>" +
      '<div class="ov-row"><div class="k">照片数量</div><div class="v">' + allPhotos(m) + " 张</div></div>" +
      '<div class="ov-row"><div class="k">最近更新</div><div class="v">' + (lu ? relCoarse(lu) : "尚未索引") + "</div></div>" +
      alert + "</div>";
  }
  function tasksTab(m) {
    let h = "<h3>失败分类</h3>";
    const kinds = failKinds(m);
    const any = kinds.some(function (k) { return (k.count + k.ignored) > 0; });
    if (!any) h += '<p class="note">没有失败项</p>';
    else kinds.forEach(function (k) {
      if (k.count + k.ignored === 0) return;
      h += '<div class="fail-row" data-kind="' + k.id + '"><b>' + esc(k.label) + "</b><span>" + k.count + " 条</span>";
      if (k.ignored > 0) h += "<span>已忽略 " + k.ignored + "（仍可见）</span>";
      if (k.retry && k.count > 0) h += '<button class="btn" data-act="fail-retry" data-kind="' + k.id + '">重试全部</button>';
      if (!k.retry) h += '<span class="note" style="padding:0">不支持重试</span>';
      if (k.count > 0) h += '<button class="btn" data-act="fail-ignore" data-kind="' + k.id + '">忽略全部</button>';
      if (k.ignored > 0) h += '<button class="btn" data-act="fail-unignore" data-kind="' + k.id + '">取消忽略</button>';
      h += "</div>";
    });
    const hist = m.tasks.history || [];
    const run = m.tasks.running || [];
    h += "<h3>任务</h3>";
    h += '<div class="set-row"><button class="btn" data-act="clear-task-history"' + (hist.length ? "" : " disabled") + ' title="删除任务页里已结束的记录与失败明细；正在跑的任务不受影响">清空任务历史…</button>';
    if (!hist.length) h += '<span class="note" style="padding:0 8px">没有已结束的任务记录</span>';
    h += "</div>";
    if (!run.length && !hist.length) return h + '<p class="note">暂无索引任务</p>';
    run.forEach(function (t) { h += taskRow(t, true); });
    hist.forEach(function (t) { h += taskRow(t, false); });
    if (m.pausedBySystem && (run.length || hist.length)) {
      h += '<p class="warn-c">系统暂停中（休眠 / 低电量），恢复由系统事件自动发生</p>';
    }
    return h;
  }
  function taskRow(t, live) {
    let h = '<div class="task-row" data-task="' + t.id + '"><span class="scope">' + esc(t.scope) + "</span>";
    h += "<span>" + esc(t.kind) + "</span> <span>" + esc(t.state) + "</span> ";
    if (live) {
      h += '<span class="task-prog">已处理 ' + (t.discovered || 0) + " 张 · 已用 " + mmss(Date.now() - t.startedAt) + "</span>";
      if (t.taskId) {
        if (t.userPaused) {
          h += '<span class="warn-c">已暂停</span>';
          h += '<button class="btn" data-act="task-resume" data-id="' + t.id + '">继续</button>';
          h += '<button class="btn" data-act="task-cancel" data-id="' + t.id + '">取消</button>';
        } else {
          h += '<button class="btn" data-act="task-pause" data-id="' + t.id + '">暂停</button>';
          h += '<button class="btn" data-act="task-cancel" data-id="' + t.id + '">取消</button>';
        }
      }
    } else {
      h += "<span>" + esc(t.outcome || "") + "</span>";
      h += "<span>完成 " + (t.completed || 0) + " · 跳过 " + (t.skipped || 0) + " · 失败 " + (t.failed || 0);
      if (t.cancelReason) h += " · 已取消（" + esc(t.cancelReason) + "）";
      h += "</span>";
    }
    return h + "</div>";
  }
  function foldersTab(m) {
    const roots = m.roots || [];
    let h = "";
    const deg = degradedWatches(m);
    if (deg) h += '<p class="warn-c">' + deg + " 个文件夹的实时监视不可用，已改为定时检查（详情见各行的「为什么？」）</p>";
    if (!roots.length) return h + '<p class="note">还没有关注任何文件夹</p>';
    roots.forEach(function (r) {
      const cls = "st-" + r.status;
      h += '<div class="root-card" data-root="' + esc(r.id) + '"><h4>' + esc(r.path) + ' <span class="' + cls + '">' + esc(OPData.ROOT_STATUS[r.status]) + "</span></h4>";
      h += '<div class="note" style="padding:2px 0 6px">' + esc(rootSummaryLine(r)) + "</div>";
      h += "<div>最近索引：" + relCoarse(r.lastIndexed) + "</div>";
      if (r.status === "removed") {
        h += '<p class="note">索引已移除：只有收藏 / 标签 / 说明 / 相册这些用户标记还在，重新索引会按身份自动挂回；原图与磁盘文件从未被动过。</p>';
        h += '<div class="acts"><button class="btn primary" data-act="root-restore" data-id="' + r.id + '">重新索引（恢复）</button>';
        h += '<button class="btn" data-act="show-finder" data-path="' + esc(r.path) + '">在 Finder 中显示</button>';
        h += '<button class="btn ghost" data-act="root-purge" data-id="' + r.id + '" title="彻底删除该根的索引与缩略图，并从索引管理里移除这一行（原图不动、不能撤销）">删除索引</button></div></div>';
        return;
      }
      h += '<div class="watch-line">监视　' + esc(watchLabel(r));
      if (r.watch && r.watch !== "live" && r.watch !== "none") {
        h += ' <span class="why" title="' + esc(whyText(r)) + '">为什么？</span>';
      }
      h += "</div><div class=\"acts\">";
      if (r.status === "offline") {
        h += '<button class="btn" data-act="root-reconnect" data-id="' + r.id + '">连接后刷新</button>';
        h += '<button class="btn ghost" data-act="root-purge" data-id="' + r.id + '" title="彻底删除该根的索引与缩略图，并从索引管理里移除这一行（原图不动、不能撤销）">删除索引</button>';
      } else {
        h += '<button class="btn" data-act="root-refresh" data-id="' + r.id + '">刷新</button>';
        h += '<button class="btn" data-act="root-reindex" data-id="' + r.id + '">重新索引</button>';
        h += '<button class="btn" data-act="root-force" data-id="' + r.id + '">强制重扫</button>';
        h += '<button class="btn" data-act="root-rebuild" data-id="' + r.id + '">全量重建</button>';
        if (r.status === "closed") h += '<button class="btn" data-act="root-reopen" data-id="' + r.id + '">重新打开</button>';
        else h += '<button class="btn" data-act="root-close" data-id="' + r.id + '">关闭</button>';
        h += '<button class="btn" data-act="show-finder" data-path="' + esc(r.path) + '">在 Finder 中显示</button>';
        h += '<button class="btn ghost" data-act="root-purge" data-id="' + r.id + '" title="彻底删除该根的索引与缩略图，并从索引管理里移除这一行（原图不动、不能撤销）">删除索引</button>';
      }
      h += "</div></div>";
    });
    return h;
  }
  function watchLabel(r) {
    if (r.watch === "live") return "实时";
    if (r.watch === "flood") return "事件过多，等待全量刷新";
    if (r.watch === "none" || r.status === "offline" || r.status === "closed") return "未监视";
    const reason = r.watchReason || "网络卷";
    return "每 " + (r.watchMinutes || 15) + " 分钟检查 · " + reason;
  }
  function whyText(r) {
    const k = r.watchReason || r.watch;
    if (k === "网络卷" || r.watch === "interval") return "网络卷（SMB / AFP / NFS）通常不支持文件系统事件，软件会按固定间隔检查。想要实时生效，可以把照片拷到本地磁盘再关注。";
    if (k === "监视句柄不足" || r.watch === "inotify") return "该目录树需要 " + (r.watchNeed || 8192) + " 个监视句柄，当前上限 " + (r.watchHave || 1024) + "。可以在系统层面调高（软件不会自动修改系统设置）：\nsudo sysctl fs.inotify.max_user_watches=" + (r.watchNeed || 16384);
    if (k === "权限不足") return "没有读取该目录的权限。请在「系统设置 → 隐私与安全性 → 完全磁盘访问」里授权。";
    if (k === "监视器错误") return "监视器启动失败：" + (r.watchErr || "backend error") + "。已退回定时检查。";
    if (k === "事件过多" || r.watch === "flood") return "短时间内有太多文件变化，已改为一次性重新检查这个文件夹。完成后会自动恢复实时监视。";
    return "已改为定时检查。";
  }
  function exclusionsTab(m) {
    const list = m.exclusions || [];
    if (!list.length) return '<p class="note">没有已关闭的子目录</p>';
    const inv = list.filter(function (e) { return e.invalid; }).length;
    let h = '<div class="set-row"><button class="btn" data-act="ex-restore-all">全部恢复</button>';
    h += '<button class="btn" data-act="ex-clean-invalid"' + (inv ? "" : " disabled") + ">清理失效项</button></div>";
    list.forEach(function (e) {
      h += '<div class="ex-row"><span>' + esc(e.path) + "</span>";
      if (e.invalid) h += '<span class="warn-c">已失效</span>';
      h += '<button class="btn" data-act="ex-restore" data-id="' + e.id + '">恢复</button></div>';
    });
    return h;
  }
  function storageTab(m) {
    const s = m.storage || {};
    let h = "<h3>占用</h3>";
    h += rowKV("索引库文件", bytes(s.db));
    h += rowKV("WAL", bytes(s.wal));
    h += rowKV("缩略图", s.computed ? ((s.thumbsN || 0) + " 条 · " + bytes(s.thumbs)) : "计算中…");
    h += rowKV("照片", (s.photos || 0) + " 张");
    h += "<h3>按根目录分布</h3>";
    if (!(s.byRoot || []).length) h += '<p class="note">还没有关注文件夹</p>';
    else (s.byRoot || []).forEach(function (r) {
      h += rowKV(r.path, s.computed ? bytes(r.bytes) : "计算中…");
    });
    h += "<h3>操作</h3><div class=\"set-row\">";
    h += '<button class="btn" data-act="clean-thumbs">清理缩略图</button>';
    h += '<button class="btn" data-act="vacuum">VACUUM</button>';
    h += '<button class="btn" data-act="clear-logs" title="~/Library/Logs/app.openpicasa/openpicasa.log">清空运行日志</button>';
    h += '<button class="btn" data-act="export-diag">导出诊断日志</button></div>';
    h += '<div class="set-row"><label><input type="checkbox" data-act="toggle-frame-stats"' + (m.frameStats ? " checked" : "") + "> 显示帧时间直方图（p50/p95/p99/max）</label></div>";
    h += '<p class="notice">此复选框在当前产品里点了不会画出直方图（可见未接通）。查看器信息里的 RGB 直方图是另一回事。</p>';
    if (m.frameStats) h += '<p class="note">已勾选，但没有直方图可显示（未接通）。</p>';
    return h;
  }
  function rowKV(k, v) {
    return '<div class="st-row"><span class="k">' + esc(k) + '</span><span class="v tnum">' + esc(v) + "</span></div>";
  }

  function ioTab(m) {
    const io = m.io || {};
    let h = "";
    (m.importSources || []).filter(function (s) { return s.hidden || (m.hiddenImportSources || []).indexOf(s.id) >= 0; }).forEach(function (s) {
      h += '<div class="set-row">已从分组隐藏：<button class="btn" data-act="src-unhide" data-id="' + s.id + '" title="只影响左栏分组显示；索引与用户标记一直都在">恢复「' + esc(s.name) + '」</button></div>';
    });
    if (m.lastExportInterrupted) {
      h += '<div class="banner warn"><div>上次导出未完成（进程中断）· 临时产物已清理，可一键重来</div><div class="acts">' +
        '<button class="btn" data-act="io-retry-export">重来</button><button class="btn" data-act="io-ignore-export">忽略</button></div></div>';
      const meta = m.lastExportInterruptedMeta || {};
      h += "<p class=\"note\">目标：" + esc(meta.path || "") + "（含缩略图：" + (meta.thumbs ? "是" : "否") + "）</p>";
    }
    if (m.lastImportRolledBack) {
      h += '<div class="banner warn"><div>上次导入在提交前中断 · 已回滚，现有索引未改动，可重新开始</div><div class="acts"><button class="btn" data-act="io-ack-rollback">知道了</button></div></div>';
    }
    if (m.lastImportCommitted && !io.result) {
      const c = m.lastImportCommitted;
      h += "<p>上次已提交的导入（" + esc(c.timeLabel) + "）· 成功 " + c.ok + " · 跳过 " + c.skip + " · 失败 " + c.fail + " · 冲突 " + c.conflict + "</p>";
    }
    h += "<h3>导出索引文件</h3><p class=\"note\">把索引记录与整理关系存成文件 · 不含原始照片。本原型只生成标清演示 JSON，<b>不是</b>真实的 .opicasaidx 数据库格式。</p>";
    h += '<div class="set-row"><label><input type="checkbox" data-act="io-thumbs"' + (io.includeThumbs ? " checked" : "") + "> 包含缩略图（生成索引包）</label></div>";
    h += '<div class="set-row"><button class="btn primary" data-act="io-export">导出索引文件…</button></div>';
    if (io.lastBackup) {
      const b = io.lastBackup;
      h += "<h3>最近成功备份</h3>";
      if (b.missing) h += '<p class="warn-c">备份文件已不在原位置（可能被移动或删除）</p>';
      else {
        h += "<div>" + relCoarse(b.time) + "</div><div>SHA-256：" + esc(b.sha) + "…</div><div>" + esc(b.path) + "</div>";
        h += '<button class="btn" data-act="show-finder" data-path="' + esc(b.path) + '">在 Finder 中显示</button>';
      }
    }
    h += "<h3>导入索引文件</h3><p class=\"note\">从文件恢复索引与整理关系 · 导入前可预览、可选合并 / 替换。请选择一个演示索引包（不是真实数据库）。</p>";
    h += '<div class="set-row"><button class="btn" data-act="io-pick">选择索引文件…</button></div>';
    if (io.running && !io.preview && io.kind === "import") {
      h += '<p>正在校验索引文件…（后台执行，界面可继续操作）</p><div class="pbar ind"><i></i></div>';
      h += '<p class="note">大文件需要读两遍做完整性复核，期间不会修改任何数据 · 无 ETA</p>';
      h += '<button class="btn" data-act="io-cancel-validate">取消校验</button>';
    }
    if (io.preview) h += wizardHtml(io);
    h += ioStateHtml(io);
    h += '<p class="io-foot">ⓘ 相关操作不修改或移动原始照片 · 导入采用事务保护，失败可回滚</p>';
    return h;
  }
  function wizardHtml(io) {
    const steps = ["① 校验与预览", "② 策略与范围", "③ 执行", "④ 结果与匹配"];
    let h = '<div class="steps idx-steps">';
    steps.forEach(function (s, i) {
      const n = i + 1;
      h += '<span class="' + (io.step === n ? "on" : io.step > n ? "done" : "") + '" data-act="io-step" data-n="' + n + '">' + s + "</span>";
    });
    h += "</div>";
    const p = io.preview;
    if (io.step === 1) {
      h += "<p>✓ 完整性校验通过 · 版本兼容</p><h3>索引信息</h3>";
      h += rowKV("生成应用", p.app);
      h += rowKV("格式版本", "v" + p.format);
      h += rowKV("数据库版本", "v" + p.db);
      h += rowKV("数量", "照片 " + p.photos + " · 根 " + p.roots + " · 文件夹 " + p.folders + " · 标签 " + p.tags + " · 相册 " + p.albums);
      h += "<h3>内容预览（选前）</h3><div class=\"preview-thumbs\">";
      const ready = S().photos.filter(function (x) { return x.file; }).slice(0, 12);
      for (let i = 0; i < Math.min(12, p.photos); i++) {
        const src = ready[i];
        if (src && src.file) h += '<img src="' + OPData.photoSrc(src.file) + '" alt="">';
        else h += '<div class="nope">░<br>无预览</div>';
      }
      h += '</div><p class="notice">仅来自索引包内缩略图，不读取原图 · 最多 12 张 · 本处用演示图代替包内缩略图</p>';
      h += "<h3>冲突与路径匹配</h3>";
      h += rowKV("重复记录", p.dups + " 条（按身份键处理）");
      h += rowKV("路径无法匹配", p.unmatched + " 个根");
      h += rowKV("文件夹树", p.roots + " 个根 · " + p.folders + " 个文件夹");
      const tree = p.treeSrc === "manifest" ? "清单自带（校验和覆盖）" : p.treeSrc === "folders" ? "由包内文件夹表重建" : "结构由照片路径推导";
      h += rowKV("树来源", tree);
      if (p.treeSrc !== "manifest") h += '<p class="warn-c">⚠ ' + tree + "：包内没有可校验的文件夹树，层级与计数按包内容重新推导，可能与源机不完全一致</p>";
      h += '<p class="note">ℹ 未匹配的照片导入后可重新匹配根目录，将标记为离线，不复制或移动照片</p>';
      h += '<div class="modal-acts"><button class="btn primary" data-act="io-step" data-n="2">下一步</button></div>';
    } else if (io.step === 2) {
      h += '<div class="policy">';
      h += '<div class="opt' + (io.policy !== "replace" ? " on" : "") + '" data-act="io-policy" data-v="merge">' + (io.policy !== "replace" ? "◉" : "○") + " 合并（推荐，默认）</div>";
      h += '<div class="opt' + (io.policy === "replace" ? " on" : "") + '" data-act="io-policy" data-v="replace">' + (io.policy === "replace" ? "◉" : "○") + " 替换</div></div>";
      if (io.policy !== "replace") {
        h += "<p>合并：只新增、不删除现有记录；身份相同且内容不同保留现有，计入冲突清单。</p>";
        h += "<p>标签 / 相册按名称合并去重，星标与说明保留现有；现有图库根目录不变。</p>";
      } else {
        h += "<p class=\"warn-c\">⚠ 影响范围（替换）</p>";
        h += "<p>现有独有的整理数据（约 " + (p.albums + p.tags + 4) + " 项）将被移除；执行前先生成备份。</p>";
        h += "<p>现有照片记录以导入文件为准覆盖。✓ 照片文件不受影响（不复制、不移动、不重命名）</p>";
      }
      h += '<div class="set-row"><label><input type="checkbox" data-act="io-ack"' + (io.ack ? " checked" : "") + "> 我已了解上述影响范围</label></div>";
      h += '<div class="modal-acts"><button class="btn" data-act="io-step" data-n="1">上一步</button>' +
        '<button class="btn primary" data-act="io-start-import"' + (io.ack && !io.running ? "" : " disabled") + ">开始导入</button></div>";
    } else if (io.step === 3) {
      h += "<h3>执行导入</h3>";
      if (io.running && io.kind === "import") {
        const known = !!io.known && io.total > 0;
        h += "<p>" + esc(io.phase || "正在写入整理关系（单事务）") + "（后台执行，界面可继续操作）</p>";
        if (known) {
          const pct = Math.min(100, Math.round(io.done / io.total * 100));
          h += '<div class="pbar"><i style="width:' + pct + '%"></i></div><p>已完成 ' + pct + "%（单调不回退）</p>";
        } else h += '<div class="pbar ind"><i></i></div>';
        if (io.current) h += "<p>当前：" + esc(io.current) + "</p>";
        if ((io.errors || []).length) h += "<p>已收集 " + io.errors.length + " 项错误</p>";
        h += '<p class="note">导入在后台线程执行，界面可继续操作 · 无 ETA</p>';
        h += '<button class="btn" data-act="io-cancel-import">取消导入</button>';
      } else if (io.state === "cancelled" || io.cancelled) {
        h += "<p>⏹ 已取消 · 整库回滚、无残留 staging 文件/连接 · 现有索引未改动</p>";
      } else if (io.state === "fail") {
        h += '<p class="danger-c">' + esc(io.message || "导入失败") + "</p>";
      } else if (io.result) {
        h += "<p>导入已完成，请查看结果。</p><button class=\"btn primary\" data-act=\"io-step\" data-n=\"4\">查看结果</button>";
      } else {
        h += "<p>准备就绪，点「开始导入」执行。</p><button class=\"btn\" data-act=\"io-step\" data-n=\"2\">返回策略</button>";
      }
      h += '<p class="notice">ⓘ 关闭本窗口不会取消导入；重开可继续查看进度</p>';
    } else {
      const r = io.result || { imported: 0, skipped: 0, failed: 0, conflict: 0, ids: [], errors: [] };
      h += "<h3>导入结果</h3>";
      h += rowKV("成功导入", r.imported + " 条");
      h += rowKV("跳过", String(r.skipped));
      h += rowKV("失败", String(r.failed));
      h += rowKV("冲突", String(r.conflict));
      if (r.ids && r.ids.length) h += '<div class="set-row"><button class="btn primary" data-act="browse-import">浏览导入的照片</button></div><p class="notice">只读索引与缩略图；原图按原路径读取，不写照片目录</p>';
      if (r.errors && r.errors.length) {
        h += "<h3>失败明细（查看详情）</h3>";
        r.errors.slice(0, 50).forEach(function (e, i) { h += "<div>" + (i + 1) + ". " + esc(e) + "</div>"; });
        if (r.errors.length > 50) h += "<div>… 另有 " + (r.errors.length - 50) + " 项</div>";
      }
      h += "<h3>重新匹配图库根目录（可选后续动作）</h3>";
      h += "<div>✓ ~/Pictures</div>";
      h += '<div class="set-row">⚠ /Users/src/京都 · 路径不存在（离线） <button class="btn" data-act="remap-root" data-path="/Users/src/京都">选择本机目录…</button></div>';
      h += '<p class="note">ℹ 重新匹配不会复制、移动或重命名照片；映射后做增量核对</p>';
    }
    return h;
  }
  function ioStateHtml(io) {
    let state = io.state || "idle";
    if (io.running) state = io.known ? "running" : "preparing";
    const names = { idle: "空闲", preparing: "准备中", running: "进行中", success: "✓ 成功", fail: "⛔ 失败", cancelled: "⏹ 已取消" };
    let body = io.message || "";
    if (state === "idle") body = io.message || "";
    if (state === "preparing") body = io.phase || "正在准备… 校验目标 / 计算预计内容 / 读清单";
    if (state === "running") {
      body = io.phase || "";
      if (io.known && io.total) body += " 已完成 " + Math.round(io.done / io.total * 100) + "% · 已处理 " + io.done + " 项";
    }
    if (state === "success" && io.result && io.result.failed > 0) body += " 部分失败：" + io.result.failed + " 项错误，请在「导入结果」查看详情";
    if (state === "fail") body = (io.message || "") + " 现有索引保持可用；可重新选择文件或稍后重试";
    if (state === "cancelled") body = "已取消 · 现有索引未改动。现有索引保持可用；可重新选择文件或稍后重试";
    return "<h3>操作状态</h3><div class=\"st-row\"><b>" + esc(names[state] || state) + "</b> <span>" + esc(body) + "</span></div>";
  }
  function pageHtml() {
    const m = S().mgmt;
    const tab = tabId();
    const b = badge(m);
    let h = '<div class="page index-page">';
    h += '<div class="crumb"><a href="#/settings">' + (g.OPIcons ? OPIcons.icon("back", 14, 2) : "") + "返回设置</a><span class=\"crumb-sep\">/</span><span>索引管理</span></div>";
    h += "<h2>索引管理</h2>";
    h += '<p class="note lead">这是「设置」的下级页面。从设置页「打开索引管理」或更多菜单进入。底层行为按现有应用源码保留。不扫描真实磁盘，也不写入原文件。</p>';
    if (b > 0) h += '<div class="badge-note">' + badgeSentences(m).join(" · ") + "</div>";
    h += overviewHtml(m);
    h += '<div class="idx-tabs">';
    TABS.forEach(function (t) {
      h += '<a href="#/settings/index?tab=' + t.id + '" class="' + (tab === t.id ? "on" : "") + '" data-act="idx-tab" data-tab="' + t.id + '">' + t.label + "</a>";
    });
    h += "</div><div class=\"idx-pane\" data-tab=\"" + tab + "\">";
    if (tab === "folders") h += foldersTab(m);
    else if (tab === "exclusions") h += exclusionsTab(m);
    else if (tab === "storage") h += storageTab(m);
    else if (tab === "io") h += ioTab(m);
    else h += tasksTab(m);
    h += "</div></div>";
    return h;
  }
  function overlayHtml() {
    const m = S().mgmt;
    if (!m || !m.overlay.shown || m.overlay.dismissed) return "";
    const t = runningTask(m);
    const failN = failKinds(m).reduce(function (n, k) { return n + k.count; }, 0);
    const stage = t ? (m.globalPaused || m.pausedBySystem ? "暂停" : t.state) : (failN ? "部分失败" : "已完成");
    let h = '<div class="idx-float" data-overlay="index">';
    h += "<h4>正在建立缩略图索引<span class=\"sp\"></span>";
    h += '<button class="mini" data-act="idx-float-collapse">' + (m.overlay.collapsed ? "展开" : "收起") + "</button>";
    h += '<button class="mini" data-act="idx-float-close" title="关闭（索引会继续）">×</button></h4>';
    if (m.overlay.collapsed) {
      h += "<div>" + esc(stage) + " · 已发现 " + (t ? (t.discovered || 0) : 0) + " 张</div>";
    } else {
      h += "<div>" + esc((t && (t.folderName || t.scope)) || "") + " · " + esc(stage) + "</div>";
      h += '<div class="pbar' + (t ? " ind" : "") + '"><i></i></div>';
      h += "<div>已发现 " + (t ? (t.discovered || 0) : 0) + " 张" + (failN ? " · 失败 " + failN + " 项" : "") + "</div>";
      h += '<div class="mini-acts"><button class="btn" data-act="open-progress">详情</button></div>';
      if (t && t.state === "不可访问") h += '<p class="warn-c">有目录不可访问，请检查外接盘或权限</p>';
      else if (failN && !t) h += '<p class="warn-c">部分失败 ' + failN + " 项，索引未完全成功</p>";
    }
    return h + "</div>";
  }
  function progressHtml() {
    const m = S().mgmt;
    if (!m || !m.progressOpen) return "";
    const run = (m.tasks.running || []);
    const jobs = m.folderJobs || [];
    let h = '<div class="idx-prog-win" role="dialog" aria-label="索引进度"><h3>索引进度</h3>';
    if (!run.length && !jobs.length) {
      h += "<p>暂无索引任务</p><p>添加关注文件夹后，程序会在后台建立缩略图；<br>索引在后台进行，不阻塞浏览。</p>";
      h += '<div class="set-row"><button class="btn primary" data-act="import">添加关注文件夹</button>';
      h += '<button class="btn ghost" data-act="purge-all" title="删除已建立的照片索引与缩略图，关注文件夹保留">删除索引</button></div>';
    } else {
      const t = run[0];
      const found = t ? (t.discovered || 0) : jobs.reduce(function (n, j) { return n + (j.discovered || 0); }, 0);
      const done = t ? (t.completed || Math.max(0, found - 3)) : found;
      h += "<div>" + Math.max(1, (m.roots || []).filter(function (r) { return r.status === "normal"; }).length) + " 个文件夹 · 已发现 " + found + " 张 · " + esc((t && t.state) || "扫描中") + "</div>";
      h += '<div class="pbar ind"><i></i></div>';
      const completed = jobs.filter(function (j) { return !j.running; });
      if (completed.length) {
        h += '<div class="mi" data-act="toggle-completed">' + (m.completedOpen ? ("已完成 " + completed.length + " 项（点击收起）") : ("已完成 " + completed.length + " 项（点击展开）")) + "</div>";
        if (m.completedOpen) completed.forEach(function (j) { h += folderJobRow(j, false); });
      }
      jobs.filter(function (j) { return j.running; }).forEach(function (j) { h += folderJobRow(j, true); });
      if (t && !jobs.some(function (j) { return j.running; })) {
        h += folderJobRow({ name: t.folderName || t.scope, path: t.scope, state: t.state, discovered: t.discovered, completed: t.completed, failed: t.failed, startedAt: t.startedAt, running: true }, true);
      }
      const fails = m.failedItems || [];
      if (fails.length) {
        h += '<div class="set-row"><button class="btn" data-act="toggle-failed">' + (m.failedOpen ? "收起失败项" : "查看失败项") + "</button>";
        h += '<button class="btn" data-act="retry-failed">重试失败项</button></div>';
        if (m.failedOpen) {
          fails.slice(0, 50).forEach(function (f) { h += "<div>· " + esc(f.name) + " — " + esc(f.reason) + "</div>"; });
          if (fails.length > 50) h += "<div>… 另有 " + (fails.length - 50) + " 项</div>";
        }
      }
      h += '<div class="set-row" style="margin-top:12px">';
      if (m.globalPaused) h += '<button class="btn primary" data-act="global-resume">继续索引</button>';
      else if (run.length || t) h += '<button class="btn" data-act="global-pause">暂停索引</button>';
      if (fails.length) h += '<button class="btn" data-act="retry-all-failed">全部重试</button>';
      h += '<button class="btn" data-act="open-index-smart">打开索引管理</button>';
      h += '<button class="btn ghost" data-act="purge-all" title="删除照片索引与缩略图，关注文件夹保留">删除索引</button>';
      h += '<button class="btn" data-act="close-progress">关闭</button></div>';
    }
    return h + "</div>";
  }
  function folderJobRow(j, live) {
    return '<div class="folder-job"><b>' + esc((j.name || "").slice(0, 22)) + "</b> " + esc(j.state || "") +
      "<div>" + esc((j.path || "").split("/").slice(-2).join("/")) + " · 已完成 " + (j.completed || 0) + " / 已发现 " + (j.discovered || 0) +
      (j.failed ? " 失败 " + j.failed + " 项" : "") + " · <span title=\"" + esc(relFine(j.startedAt)) + "\">" + relFine(j.startedAt) + "</span></div>" +
      (live ? '<div class="pbar ind"><i></i></div>' : "") + "</div>";
  }

  function setTab(id) {
    OPStore.patch(function (s) { s.mgmt.tab = id; });
    const hash = "#/settings/index?tab=" + id;
    if ((location.hash || "").indexOf("#/settings/index") === 0) {
      if (location.hash !== hash) location.hash = hash;
      else render();
    } else go(hash);
  }
  function onAct(act, el) {
    const id = el && (el.getAttribute("data-id") || el.getAttribute("data-kind") || el.getAttribute("data-tab"));
    if (act === "idx-tab") { setTab(el.getAttribute("data-tab")); return true; }
    if (act === "open-index") { setTab(el.getAttribute("data-tab") || defaultTab(S().mgmt)); return true; }
    if (act === "open-index-smart") { OPStore.patch(function (s) { s.mgmt.progressOpen = false; }); setTab(defaultTab(S().mgmt)); return true; }
    if (act === "fail-retry") {
      const k = OPData.FAIL_KINDS.find(function (x) { return x.id === el.getAttribute("data-kind"); });
      const row = S().mgmt.failures[k.id];
      if (!k.retry) return true;
      if (!row || row.count <= 0) { toast("没有可重试的「" + k.label + "」项"); return true; }
      const n = row.count;
      startTask({ scope: "~/Pictures", kind: "刷新", target: 10 });
      toast("已重新入队 " + n + " 张（" + k.label + "）");
      return true;
    }
    if (act === "fail-ignore") {
      const k = el.getAttribute("data-kind");
      const meta = OPData.FAIL_KINDS.find(function (x) { return x.id === k; });
      let n = 0;
      OPStore.patch(function (s) {
        const row = s.mgmt.failures[k];
        n = row.count;
        row.ignored += n; row.count = 0;
        (s.mgmt.failedItems || []).forEach(function (f) { if (f.kind === k) f.ignored = true; });
      });
      toast("已忽略 " + n + " 条「" + meta.label + "」失败项（列表仍然可见）");
      return true;
    }
    if (act === "fail-unignore") {
      const k = el.getAttribute("data-kind");
      const meta = OPData.FAIL_KINDS.find(function (x) { return x.id === k; });
      OPStore.patch(function (s) {
        const row = s.mgmt.failures[k];
        row.count += row.ignored; row.ignored = 0;
        (s.mgmt.failedItems || []).forEach(function (f) { if (f.kind === k) f.ignored = false; });
      });
      toast("已取消忽略「" + meta.label + "」失败项");
      return true;
    }
    if (act === "clear-task-history") {
      const n = (S().mgmt.tasks.history || []).length;
      if (!n) { toast("没有已结束的任务记录"); return true; }
      confirmIndex({
        title: "确认清空任务历史", ok: "确认",
        body: "只删除任务页里已结束的记录与失败明细。运行 / 排队 / 暂停中的任务不受影响。角标会归零。不影响照片、缩略图、标记、运行日志。",
        onOk: function () {
          const c = S().mgmt.tasks.history.length;
          OPStore.patch(function (s) { s.mgmt.tasks.history = []; });
          toast(c ? ("已清空 " + c + " 条任务历史") : "没有已结束的任务记录");
        }
      });
      return true;
    }
    if (act === "task-pause") {
      OPStore.patch(function (s) {
        const t = s.mgmt.tasks.running.find(function (x) { return String(x.id) === String(el.getAttribute("data-id")); });
        if (t) { t.userPaused = true; t.state = "暂停"; }
      });
      toast("该索引任务已暂停（其它任务不受影响）");
      return true;
    }
    if (act === "task-resume") {
      if (S().mgmt.pausedBySystem) { toast("系统暂停中（休眠 / 低电量），恢复由系统事件自动发生"); return true; }
      OPStore.patch(function (s) {
        const t = s.mgmt.tasks.running.find(function (x) { return String(x.id) === String(el.getAttribute("data-id")); });
        if (t) { t.userPaused = false; t.state = "扫描中"; }
      });
      scheduleTick();
      toast("该索引任务已继续（从检查点续跑）");
      return true;
    }
    if (act === "task-cancel") {
      OPStore.patch(function (s) {
        const t = s.mgmt.tasks.running.find(function (x) { return String(x.id) === String(el.getAttribute("data-id")); });
        if (!t) return;
        t.outcome = "已取消"; t.state = "已完成"; t.cancelReason = "用户取消"; t.endedAt = Date.now();
        s.mgmt.tasks.running = s.mgmt.tasks.running.filter(function (x) { return x.id !== t.id; });
        s.mgmt.tasks.history.unshift(t);
      });
      toast("该索引任务已取消（已建索引与用户标记保留）");
      return true;
    }
    if (act === "root-refresh" || act === "root-reindex") {
      const r = S().mgmt.roots.find(function (x) { return x.id === id; });
      if (!r) return true;
      startTask({ scope: r.path, kind: act === "root-reindex" ? "重建" : "刷新" });
      return true;
    }
    if (act === "root-restore") {
      const r = S().mgmt.roots.find(function (x) { return x.id === id; });
      if (!r) return true;
      OPStore.patch(function (s) {
        const x = s.mgmt.roots.find(function (z) { return z.id === id; });
        if (x) x.status = "normal";
        if (s.ui.parkedPhotos && s.ui.parkedPhotos[id]) {
          s.photos = s.photos.concat(s.ui.parkedPhotos[id]);
          delete s.ui.parkedPhotos[id];
        }
      });
      startTask({ scope: r.path, kind: "恢复" });
      toast("正在重新索引「" + r.path + "」（用户标记会按身份自动挂回）");
      return true;
    }
    if (act === "root-force") {
      const r = S().mgmt.roots.find(function (x) { return x.id === id; });
      if (!r) return true;
      if ((S().mgmt.tasks.running || []).some(function (t) { return t.kind === "校验"; })) {
        toast("已有校验任务在运行，请稍候"); return true;
      }
      startTask({ scope: r.path, kind: "校验", target: 16 });
      toast("已对 " + r.path + " 开始强制重扫（跳过快筛，文件级核对）");
      return true;
    }
    if (act === "root-rebuild") {
      const r = S().mgmt.roots.find(function (x) { return x.id === id; });
      if (!r) return true;
      confirmIndex({
        title: "确认全量重建", ok: "确认",
        body: "范围：此根及其全部子文件夹（根 / 子树）。\n路径：" + r.path + "\n照片数：" + countOrPending(r.photos) + " 张 · 标记数：" + countOrPending(r.marks) + " 张（带收藏/标签/说明/相册的照片）\n将重新生成全部缩略图；原图不受影响，用户标记全部保留。重建期间旧缩略图保持可见，直到被新图替换。",
        onOk: function () {
          startTask({ scope: r.path, kind: "重建", target: 26 });
          toast("已对 " + r.path + " 开始全量重建（旧缩略图保留直到替换）");
        }
      });
      return true;
    }
    if (act === "root-close") {
      OPStore.patch(function (s) {
        const x = s.mgmt.roots.find(function (z) { return z.id === id; });
        if (x) { x.status = "closed"; x.watch = "none"; x.watchReason = "已关闭"; }
      });
      return true;
    }
    if (act === "root-reopen") {
      OPStore.patch(function (s) {
        const x = s.mgmt.roots.find(function (z) { return z.id === id; });
        if (x) { x.status = "normal"; x.watch = "live"; x.watchReason = ""; }
      });
      return true;
    }
    if (act === "root-reconnect") {
      const r = S().mgmt.roots.find(function (x) { return x.id === id; });
      if (r) startTask({ scope: r.path, kind: "刷新" });
      return true;
    }
    if (act === "root-purge") {
      const r = S().mgmt.roots.find(function (x) { return x.id === id; });
      if (!r) { toast("该索引已经不存在"); return true; }
      if (S().mgmt.deleting) { toast("已有删除任务在跑，请稍候"); return true; }
      confirmIndex({
        title: "确认删除索引", ok: "确认", danger: true,
        body: "范围：此根及其全部子文件夹（根 / 子树）。\n路径：" + r.path + "\n照片数：" + countOrPending(r.photos) + " 张 · 标记数：" + countOrPending(r.marks) + " 张（带收藏/标签/说明/相册的照片）\n将彻底删除该根的索引、缩略图、索引管理里这一行、关注关系与本根排除记录，左栏和网格里这些文件夹和预览也不见；不能撤销，不能再「重新索引（恢复）」，要重新添加并扫描。原图不动。标记按身份骨架保留，以后重加同一目录会挂回。本原型只模拟资料库变化，不会删除磁盘上的照片。",
        onOk: function () {
          OPStore.patch(function (s) {
            s.mgmt.roots = s.mgmt.roots.filter(function (z) { return z.id !== r.id; });
            if (r.sourceId) s.sources = s.sources.filter(function (z) { return z.id !== r.sourceId; });
            if (r.id === "pictures") {
              s.folders = s.folders.filter(function (f) { return f.source !== "pictures"; });
              s.photos = s.photos.filter(function (p) { return (p.source || "pictures") !== "pictures" && p.source !== undefined && p.source !== "pictures"; });
              s.photos = s.photos.filter(function (p) { return p.source && p.source !== "pictures"; });
            }
            if (r.id === "sd") s.photos = s.photos.filter(function (p) { return p.source !== "sd"; });
          });
          toast("已彻底删除「" + r.path + "」的索引（索引数据与缩略图已删除，原图未动）");
        }
      });
      return true;
    }
    if (act === "show-finder") {
      const path = el.getAttribute("data-path") || "";
      const sdOff = S().mgmt.roots.find(function (r) { return r.id === "sd" && r.status === "offline"; });
      if (path.indexOf("/Volumes/TravelSD") === 0 && sdOff) toast("文件不存在：" + path);
      else toast("已请求在 Finder 中显示：" + path + "（演示，本原型不会打开系统访达，也不改原文件）");
      return true;
    }
    if (act === "ex-restore-all") {
      (S().mgmt.exclusions || []).forEach(function (e) { toast("已恢复 " + e.path); });
      OPStore.patch(function (s) { s.mgmt.exclusions = []; });
      return true;
    }
    if (act === "ex-restore") {
      const e = S().mgmt.exclusions.find(function (x) { return x.id === el.getAttribute("data-id"); });
      if (!e) { toast("恢复失败：找不到该排除项"); return true; }
      OPStore.patch(function (s) { s.mgmt.exclusions = s.mgmt.exclusions.filter(function (x) { return x.id !== e.id; }); });
      toast("已恢复 " + e.path);
      return true;
    }
    if (act === "ex-clean-invalid") {
      const n = S().mgmt.exclusions.filter(function (e) { return e.invalid; }).length;
      OPStore.patch(function (s) { s.mgmt.exclusions = s.mgmt.exclusions.filter(function (e) { return !e.invalid; }); });
      toast("已清理 " + n + " 条失效排除项");
      return true;
    }
    if (act === "clean-thumbs") {
      confirmIndex({
        title: "确认清理缩略图", ok: "确认",
        body: "范围：全库。将清除全部缩略图，下次浏览时重新生成。原图与标记保留。本原型只模拟资料库变化。",
        onOk: function () {
          const n = S().mgmt.storage.thumbsN || 0;
          OPStore.patch(function (s) { s.mgmt.storage.thumbs = 0; s.mgmt.storage.thumbsN = 0; s.mgmt.storage.computed = true; });
          toast("已清理 " + n + " 条缩略图");
        }
      });
      return true;
    }
    if (act === "vacuum") { toast("已完成 VACUUM"); return true; }
    if (act === "clear-logs") {
      confirmIndex({
        title: "确认清空运行日志", ok: "确认",
        body: "范围：~/Library/Logs/app.openpicasa/openpicasa.log。不影响索引库、任务历史、照片、已经导出的诊断副本。本原型只模拟清空。",
        onOk: function () { toast("已清空运行日志（12.0 KB）"); }
      });
      return true;
    }
    if (act === "export-diag") {
      confirmIndex({
        title: "导出诊断日志（演示）", ok: "导出示例文件",
        body: "产品会打开系统选目录。本原型改为下载一份脱敏示例 JSON（路径已哈希），文件名 openpicasa-diag-demo.json。不会写入照片目录。",
        onOk: function () {
          const name = "openpicasa-diag-demo.json";
          downloadDemo(name, JSON.stringify({ warning: "演示诊断日志，路径已脱敏，不是完整产品导出。", exportedAt: new Date().toISOString(), roots: (S().mgmt.roots || []).length, tasks: (S().mgmt.tasks.history || []).length }, null, 2), "application/json");
          toast("✓ 已导出诊断日志：" + name);
        }
      });
      return true;
    }
    if (act === "toggle-frame-stats") { OPStore.patch(function (s) { s.mgmt.frameStats = !s.mgmt.frameStats; }); return true; }
    if (act === "io-thumbs") { OPStore.patch(function (s) { s.mgmt.io.includeThumbs = !s.mgmt.io.includeThumbs; }); return true; }
    if (act === "io-export") {
      if (S().mgmt.io.running) { toast("已有导入 / 导出任务在运行，请稍候"); return true; }
      confirmIndex({
        title: "导出索引文件（不含原始照片）", ok: "导出演示 JSON",
        body: "产品会打开系统保存框，默认文件名为 photo-index.opicasaidx 或 .opicasaidx.zip。本原型只生成标清演示 JSON（photo-index-demo.json），明确不是真实索引包 / 数据库格式，也不含原片。",
        onOk: function () {
          OPStore.patch(function (s) {
            s.mgmt.io.running = true; s.mgmt.io.kind = "export"; s.mgmt.io.state = "running";
            s.mgmt.io.phase = "正在导出索引文件"; s.mgmt.io.known = true; s.mgmt.io.done = 0; s.mgmt.io.total = 40;
            s.mgmt.tab = "io";
          });
          go("#/settings/index?tab=io");
          tickIo();
        }
      });
      return true;
    }
    if (act === "io-pick") { if (g.OPApp && OPApp.openDemoPackPicker) OPApp.openDemoPackPicker(); return true; }
    if (act === "io-step") {
      if (S().mgmt.io.running) return true;
      const n = parseInt(el.getAttribute("data-n"), 10);
      OPStore.patch(function (s) { if (s.mgmt.io.preview) s.mgmt.io.step = n; });
      return true;
    }
    if (act === "io-policy") { OPStore.patch(function (s) { s.mgmt.io.policy = el.getAttribute("data-v"); }); return true; }
    if (act === "io-ack") { OPStore.patch(function (s) { s.mgmt.io.ack = !s.mgmt.io.ack; }); return true; }
    if (act === "io-start-import") {
      const io = S().mgmt.io;
      if (!io.ack || io.running) return true;
      OPStore.patch(function (s) {
        s.mgmt.io.running = true; s.mgmt.io.kind = "import"; s.mgmt.io.state = "running";
        s.mgmt.io.phase = "正在写入整理关系（单事务）"; s.mgmt.io.step = 3;
        s.mgmt.io.known = true; s.mgmt.io.done = 0; s.mgmt.io.total = 32; s.mgmt.io.cancelled = false;
        s.mgmt.io.current = "相册 / 标签（演示）";
      });
      tickIo();
      return true;
    }
    if (act === "io-cancel-validate") {
      OPStore.patch(function (s) {
        s.mgmt.io.running = false; s.mgmt.io.state = "cancelled"; s.mgmt.io.phase = "";
        s.mgmt.io.preview = null; s.mgmt.io.message = "已取消 · 现有索引未改动";
      });
      toast("已取消校验，现有索引未改动");
      return true;
    }
    if (act === "io-cancel-import") {
      OPStore.patch(function (s) {
        s.mgmt.io.running = false; s.mgmt.io.state = "cancelled"; s.mgmt.io.cancelled = true;
        s.mgmt.io.message = "已取消 · 现有索引未改动"; s.mgmt.io.phase = "已取消 · 现有索引未改动";
      });
      toast("已取消导入 · 现有索引未改动");
      return true;
    }
    if (act === "io-retry-export") {
      if (S().mgmt.io.running) { toast("已有导入 / 导出任务在运行，请稍候"); return true; }
      OPStore.patch(function (s) { s.mgmt.lastExportInterrupted = false; });
      onAct("io-export", el);
      return true;
    }
    if (act === "io-ignore-export") { OPStore.patch(function (s) { s.mgmt.lastExportInterrupted = false; }); return true; }
    if (act === "io-ack-rollback") { OPStore.patch(function (s) { s.mgmt.lastImportRolledBack = false; }); return true; }
    if (act === "src-unhide") {
      OPStore.patch(function (s) {
        const src = (s.mgmt.importSources || []).find(function (x) { return x.id === el.getAttribute("data-id"); });
        if (src) src.hidden = false;
        s.mgmt.hiddenImportSources = (s.mgmt.hiddenImportSources || []).filter(function (x) { return x !== el.getAttribute("data-id"); });
      });
      toast("已恢复该来源的分组显示");
      return true;
    }
    if (act === "browse-import") {
      const ids = (S().mgmt.io.result && S().mgmt.io.result.ids) || S().ui.importBatchIds || [];
      if (!ids.length) { toast("这次导入没有带进任何照片"); return true; }
      OPStore.patch(function (s) { s.ui.importBatchIds = ids; });
      go("#/import-batch");
      toast("本次导入 · " + ids.length + " 张");
      return true;
    }
    if (act === "remap-root") {
      if (g.OPApp && OPApp.openRemapPicker) OPApp.openRemapPicker(el.getAttribute("data-path"));
      else toast("请选择本机目录（演示，不复制、不移动、不重命名照片）");
      return true;
    }
    if (act === "idx-float-close") { OPStore.patch(function (s) { s.mgmt.overlay.dismissed = true; s.mgmt.overlay.shown = false; }); return true; }
    if (act === "idx-float-collapse") { OPStore.patch(function (s) { s.mgmt.overlay.collapsed = !s.mgmt.overlay.collapsed; }); return true; }
    if (act === "open-progress") { OPStore.patch(function (s) { s.mgmt.progressOpen = true; }); return true; }
    if (act === "close-progress") { OPStore.patch(function (s) { s.mgmt.progressOpen = false; }); return true; }
    if (act === "toggle-failed") { OPStore.patch(function (s) { s.mgmt.failedOpen = !s.mgmt.failedOpen; }); return true; }
    if (act === "toggle-completed") { OPStore.patch(function (s) { s.mgmt.completedOpen = !s.mgmt.completedOpen; }); return true; }
    if (act === "retry-failed" || act === "retry-all-failed") {
      const items = (S().mgmt.failedItems || []).filter(function (f) { return !f.ignored && f.kind !== "unsupported"; });
      if (!items.length) { toast("没有可重试的失败项"); return true; }
      startTask({ scope: "~/Pictures", kind: "刷新", target: 8 });
      toast("已重新入队失败项（演示）");
      return true;
    }
    if (act === "global-pause") { OPStore.patch(function (s) { s.mgmt.globalPaused = true; }); toast("索引已暂停（已建索引保留，可继续）"); return true; }
    if (act === "global-resume") {
      if (S().mgmt.pausedBySystem) { toast("系统暂停中（休眠 / 低电量），恢复由系统事件自动发生"); return true; }
      OPStore.patch(function (s) { s.mgmt.globalPaused = false; });
      scheduleTick();
      toast("索引已继续");
      return true;
    }
    if (act === "purge-all") {
      const n = allPhotos(S().mgmt);
      confirmIndex({
        title: "删除索引", ok: "删除索引", danger: true,
        body: "范围：全库（所有关注文件夹）。将删除已建立的照片索引与缩略图。\n照片数：" + n + " 张（全库）\n原图与磁盘文件不删；关注文件夹和「关闭此文件夹」规则保留；收藏/标签/说明/相册保留，重新扫描后挂回。本原型只模拟资料库变化，不会删除磁盘文件。",
        onOk: function () {
          if (S().mgmt.deleting) { toast("已在删除中，请稍候"); return; }
          toast("开始删除索引…");
          OPStore.patch(function (s) {
            s.photos = [];
            s.ui.libraryEmpty = true;
            (s.mgmt.roots || []).forEach(function (r) {
              if (r.status !== "removed") { r.photos = 0; r.thumbsBytes = 0; }
            });
            s.mgmt.progressOpen = false;
          });
          toast("已删除全库索引与缩略图（关注文件夹保留，原图未动，演示）");
        }
      });
      return true;
    }
    return false;
  }
  function startValidate(packId) {
    OPStore.patch(function (s) {
      s.mgmt.tab = "io";
      s.mgmt.io = Object.assign(s.mgmt.io, {
        running: true, kind: "import", state: "preparing", phase: "正在校验索引文件",
        known: false, done: 0, total: 0, preview: null, step: 1, packId: packId,
        ack: false, cancelled: false, result: null, message: ""
      });
    });
    go("#/settings/index?tab=io");
    tickIo();
  }
  function demo(name) {
    if (name === "running") { startTask({ scope: "~/Pictures", kind: "刷新", target: 36 }); return; }
    if (name === "system-pause") {
      if (!runningTask(S().mgmt)) startTask({ scope: "~/Pictures", kind: "刷新", target: 40 });
      OPStore.patch(function (s) { s.mgmt.pausedBySystem = true; s.mgmt.overlay.shown = true; s.mgmt.overlay.dismissed = false; });
      toast("已进入系统暂停演示。任务页没有「继续」按钮，请用「系统恢复」结束演示。");
      return;
    }
    if (name === "system-resume") {
      OPStore.patch(function (s) { s.mgmt.pausedBySystem = false; });
      scheduleTick();
      toast("系统事件已恢复，索引继续（演示）");
      return;
    }
    if (name === "failures") {
      OPStore.patch(function (s) {
        if (window.OPData && OPData.DEMO_FAILURES) {
          s.mgmt.failures = JSON.parse(JSON.stringify(OPData.DEMO_FAILURES));
          s.mgmt.failedItems = JSON.parse(JSON.stringify(OPData.DEMO_FAILED_ITEMS || []));
        }
        const have = {};
        s.photos.forEach(function (p) { have[p.id] = true; });
        ((window.OPData && OPData.PLACEHOLDERS) || []).forEach(function (ph) {
          if (!have[ph.id]) s.photos.push(JSON.parse(JSON.stringify(ph)));
        });
        const ex = (s.mgmt.exclusions || []).find(function (e) { return e.id === "ex-1"; });
        if (ex) ex.invalid = true;
        const sd = (s.mgmt.roots || []).find(function (r) { return r.id === "sd"; });
        if (sd) sd.status = "offline";
        const hist = (s.mgmt.tasks && s.mgmt.tasks.history) || [];
        const fail = window.OPData && OPData.DEMO_FAIL_TASK;
        if (fail && !hist.some(function (h) { return h.failed > 0 && h.outcome !== "已取消"; })) {
          const row = JSON.parse(JSON.stringify(fail));
          row.startedAt = Date.now() - 4 * 86400000;
          row.endedAt = row.startedAt + 88000;
          hist.unshift(row);
          s.mgmt.tasks.history = hist;
        }
        const pic = (s.mgmt.roots || []).find(function (r) { return r.id === "pictures"; });
        if (pic) pic.failed = 6;
      });
      setTab("tasks");
      toast("已进入失败与占位格演示：图库会出现待生成/失败/云端格。失败计数可在设置 → 索引管理中查看。可用「退出演示状态」还原。");
      return;
    }
    if (name === "offline") {
      OPStore.patch(function (s) {
        const sd = (s.mgmt.roots || []).find(function (r) { return r.id === "sd"; });
        if (sd) sd.status = "offline";
        const closed = (s.mgmt.roots || []).find(function (r) { return r.id === "scans"; });
        if (closed) closed.status = "closed";
        const removed = (s.mgmt.roots || []).find(function (r) { return r.id === "archive"; });
        if (removed) removed.status = "removed";
      });
      setTab("folders");
      return;
    }
    if (name === "import-cancel") {
      OPStore.patch(function (s) { s.mgmt.lastImportRolledBack = true; s.mgmt.tab = "io"; });
      go("#/settings/index?tab=io");
      return;
    }
    if (name === "export-interrupt") {
      OPStore.patch(function (s) { s.mgmt.lastExportInterrupted = true; s.mgmt.tab = "io"; });
      go("#/settings/index?tab=io");
      return;
    }
    if (name === "watch-degrade") {
      OPStore.patch(function (s) {
        const r = s.mgmt.roots.find(function (x) { return x.id === "pictures"; });
        if (r) { r.watch = "interval"; r.watchReason = "监视句柄不足"; r.watchNeed = 8192; r.watchHave = 1024; r.watchMinutes = 15; }
        s.mgmt.tab = "folders";
      });
      go("#/settings/index?tab=folders");
      return;
    }
    if (name === "verify") {
      OPStore.patch(function (s) { s.mgmt.verify = { kind: "快筛", dirs: 12, files: 240 }; });
      toast("启动校验进行中（演示）。可在设置 → 索引管理中查看校验行；校验不计入失败数。");
    }
  }

  g.OPMgmt = {
    badge: function () { return badge(S().mgmt); },
    badgeTooltip: function () { return badgeTooltip(S().mgmt); },
    defaultTab: function () { return defaultTab(S().mgmt); },
    statusKind: function () { return statusKind(S().mgmt); },
    statusHtml: statusHtml,
    settingsSummaryHtml: settingsSummaryHtml,
    pageHtml: pageHtml,
    overlayHtml: overlayHtml,
    progressHtml: progressHtml,
    onAct: onAct,
    setTab: setTab,
    startTask: startTask,
    startValidate: startValidate,
    scheduleTick: scheduleTick,
    demo: demo,
    relCoarse: relCoarse,
    allPhotos: function () { return allPhotos(S().mgmt); },
    mmss: mmss,
    syncIndexJob: syncIndexJob,
    downloadDemo: downloadDemo,
    runningTask: function () { return runningTask(S().mgmt); }
  };
})(window);
