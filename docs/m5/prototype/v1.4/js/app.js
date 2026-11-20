/* OpenPicasa prototype-v1 — interactive offline shell. Visuals follow fusion-v1. */
(function (g) {
  const icon = function (n, s, w, f) { return OPIcons.icon(n, s, w, f); };
  const App = {
    sel: [], lastId: null, q: "",
    filters: { starred: false, tag: "", month: "", year: "", type: "", sizeClass: "", camera: "", lens: "", noDate: false, noMeta: false, sizeMin: "", sizeMax: "" },
    infoOpen: false, zoom: "fit",
    modal: null, menu: null, toasts: [],
    slideshow: false, slideIdx: 0,
    importW: null, creating: null, createErr: "",
    zoomPct: 0, compare: false, showFilenames: true,
    libError: "", picker: null, marquee: null
  };
  function S() { return OPStore.get(); }
  function theme() { return S().theme === "dark" ? "dark" : "light"; }
  function viewerBg() {
    const v = (S().settings && S().settings.viewerBg) || "auto";
    if (v && v !== "auto") return v;
    return theme() === "dark" ? "dark" : "white";
  }
  function $all(sel, root) { return Array.prototype.slice.call((root || document).querySelectorAll(sel)); }
  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, function (c) {
      return ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c];
    });
  }
  function escAttr(s) { return esc(s); }

  function parseRoute() {
    const raw = (location.hash || "#/all").replace(/^#/, "");
    const parts = raw.split("?");
    const segs = (parts[0] || "/all").replace(/^\//, "").split("/");
    const kind = segs[0] || "all";
    const id = decodeURIComponent(segs.slice(1).join("/") || "");
    const qs = {};
    (parts[1] || "").split("&").forEach(function (p) {
      if (!p) return;
      const kv = p.split("=");
      qs[decodeURIComponent(kv[0])] = decodeURIComponent(kv[1] || "1");
    });
    if (qs.info === "1") App.infoOpen = true;
    if (qs.info === "0") App.infoOpen = false;
    if (kind === "settings" && (id === "index" || id === "indexing") && qs.tab && window.OPMgmt) {
      const allowed = { tasks:1, folders:1, exclusions:1, storage:1, io:1 };
      if (allowed[qs.tab] && OPStore.get().mgmt.tab !== qs.tab) {
        OPStore.patch(function (st) { st.mgmt.tab = qs.tab; }, true);
      }
    }
    return { kind: kind, id: id, qs: qs };
  }
  function go(hash) {
    const cur = parseRoute();
    if (cur.kind !== "viewer") App.lastLibHash = location.hash || "#/all";
    const h = hash.charAt(0) === "#" ? hash : "#/" + hash.replace(/^\//, "");
    if ((location.hash || "#") === h) render();
    else location.hash = h.replace(/^#/, "#");
  }

  function visiblePhotos() {
    const st = S();
    const r = parseRoute();
    let list;
    if (st.ui.libraryEmpty) list = [];
    else if (r.kind === "import-batch") {
      const ids = st.ui.importBatchIds || [];
      list = st.photos.filter(function (p) { return ids.indexOf(p.id) >= 0; });
    }
    else if (r.kind === "recent") list = st.photos.filter(function (p) { return p.recent; });
    else if (r.kind === "changed") {
      list = st.photos.filter(function (p) { return (p.changedAt || "") >= "2024-05-21"; }).slice();
    }
    else if (r.kind === "stars") list = st.photos.filter(function (p) { return p.starred; });
    else if (r.kind === "folder") list = OPStore.folderPhotos(r.id, !!st.settings.recurseSubfolders);
    else if (r.kind === "album") list = OPStore.photosInAlbum(r.id);
    else if (r.kind === "tag") list = OPStore.photosWithTag(r.id);
    else if (r.kind === "source") list = st.photos.filter(function (p) { return (p.source || "pictures") === r.id; });
    else list = st.photos.slice();
    const q = App.q.trim().toLowerCase();
    const scopeAll = (st.ui.searchScope || "all") === "all";
    if (q && scopeAll && r.kind === "folder") list = st.photos.slice();
    if (q) {
      list = list.filter(function (p) {
        const path = folderPath(p.folder) + " " + (p.importPath || "");
        const blob = [p.title, p.file, p.folder, path, (p.tags || []).join(" "), monthLabel(p.folder)].join(" ").toLowerCase();
        return blob.indexOf(q) >= 0;
      });
    }
    const f = App.filters;
    if (f.starred) list = list.filter(function (p) { return p.starred; });
    if (f.tag) list = list.filter(function (p) { return (p.tags || []).indexOf(f.tag) >= 0; });
    if (f.year) list = list.filter(function (p) { return (p.date || "").slice(0, 4) === f.year; });
    if (f.type) {
      list = list.filter(function (p) {
        const fmt = (p.format || "JPEG").toUpperCase();
        if (f.type === "RAW") return /CR2|CR3|NEF|ARW|DNG/.test(fmt);
        return fmt === f.type;
      });
    }
    if (f.sizeClass) {
      list = list.filter(function (p) {
        const long = Math.max(p.w || 0, p.h || 0);
        if (!long) return false;
        if (f.sizeClass === "small") return long <= 640;
        if (f.sizeClass === "medium") return long > 640 && long <= 1920;
        if (f.sizeClass === "large") return long > 1920;
        return true;
      });
    }
    if (f.camera) {
      const c = f.camera.toLowerCase();
      list = list.filter(function (p) { return ((p.cam || "") + " " + (p.lens || "")).toLowerCase().indexOf(c) >= 0; });
    }
    if (f.lens) {
      const c = f.lens.toLowerCase();
      list = list.filter(function (p) { return (p.lens || "").toLowerCase().indexOf(c) >= 0; });
    }
    if (f.noDate) list = list.filter(function (p) { return !p.date; });
    if (f.noMeta) list = list.filter(function (p) { return !p.hasExif; });
    if (f.sizeMin || f.sizeMax) {
      const lo = (parseFloat(f.sizeMin) || 0) * 1024;
      const hi = f.sizeMax ? (parseFloat(f.sizeMax) * 1024) : 1e15;
      list = list.filter(function (p) { return (p.bytes || 0) >= lo && (p.bytes || 0) <= hi; });
    }
    const ex = (st.mgmt && st.mgmt.exclusions) || [];
    if (ex.length) {
      list = list.filter(function (p) {
        const folder = st.folders.find(function (x) { return x.id === p.folder; });
        const path = (folder && folder.path) || "";
        return !ex.some(function (e) { return path === e.path || (e.path && path.indexOf(e.path + "/") === 0); });
      });
    }
    const key = st.settings.sortKey || "date";
    const dir = st.settings.sortDir === "asc" ? 1 : -1;
    list = list.slice().sort(function (a, b) {
      if (st.settings.thumbsFirst) {
        const ra = a.thumbState === "ready" ? 0 : 1, rb = b.thumbState === "ready" ? 0 : 1;
        if (ra !== rb) return ra - rb;
      }
      let cmp = 0;
      if (key === "name") cmp = (a.file || a.title || "").localeCompare(b.file || b.title || "", "zh");
      else if (key === "size") cmp = (a.bytes || 0) - (b.bytes || 0);
      else if (key === "dim") cmp = (Math.max(a.w || 0, a.h || 0)) - (Math.max(b.w || 0, b.h || 0));
      else if (key === "mtime" || key === "changed") cmp = (a.changedAt || a.date || "").localeCompare(b.changedAt || b.date || "");
      else if (key === "added") cmp = (a.addedAt || a.changedAt || a.date || "").localeCompare(b.addedAt || b.changedAt || b.date || "");
      else if (key === "rating") cmp = (a.rating || 0) - (b.rating || 0);
      else cmp = (a.date + (a.time || "")).localeCompare(b.date + (b.time || ""));
      if (cmp === 0) cmp = (a.file || a.id).localeCompare(b.file || b.id, "zh");
      return cmp * dir;
    });
    return list;
  }
  function dayLabel(iso) {
    const m = String(iso || "").match(/^(\d{4})-(\d{2})-(\d{2})$/);
    if (m) return m[1] + "年" + m[2] + "月" + m[3] + "日";
    if (iso === "nodate") return "无日期";
    return monthLabel(iso);
  }
  function monthLabel(fid) {
    if (OPData.MONTH_META[fid] && OPData.MONTH_META[fid].label) return OPData.MONTH_META[fid].label;
    const m = String(fid || "").match(/^(\d{4})-(\d{2})$/);
    if (m) return m[1] + " 年 " + Number(m[2]) + " 月";
    if (fid === "nodate") return "无日期";
    return fid;
  }
  function groups(list) {
    const map = {}, order = [];
    list.forEach(function (p) {
      const k = (p.date || "").slice(0, 10) || "nodate";
      if (!map[k]) { map[k] = []; order.push(k); }
      map[k].push(p);
    });
    order.sort(function (a, b) {
      if (a === "nodate") return 1;
      if (b === "nodate") return -1;
      return b.localeCompare(a);
    });
    return order.map(function (k) { return { id: k, label: dayLabel(k), items: map[k] }; });
  }
  function filterActive() {
    const f = App.filters;
    return !!(App.q.trim() || f.starred || f.tag || f.month || f.year || f.type || f.sizeClass || f.camera || f.lens || f.noDate || f.noMeta || f.sizeMin || f.sizeMax);
  }
  function shotSpan(list, n) {
    n = (n != null) ? n : (list ? list.length : 0);
    const dates = (list || []).map(function (p) { return p.date; }).filter(Boolean).sort();
    if (!dates.length) return n + " 张";
    const a = dates[0], b = dates[dates.length - 1];
    return n + " 张 · 拍摄时间：" + a + (a === b ? "" : ("—" + b));
  }
  function routeTitle(r, n, list) {
    const sub = shotSpan(list || [], n);
    if (r.kind === "import-batch") return { t: "本次导入 · " + n + " 张", s: "清除筛选即可离开此范围" };
    if (r.kind === "recent") return { t: "最近导入", s: sub + " · 演示扩展视图" };
    if (r.kind === "changed") return { t: "最近更改", s: sub };
    if (r.kind === "stars") return { t: "星标", s: sub };
    if (r.kind === "folder") {
      const f = S().folders.find(function (x) { return x.id === r.id; });
      return { t: (f && f.name) || "未命名文件夹", s: sub };
    }
    if (r.kind === "album") {
      const a = OPStore.album(r.id);
      return { t: a ? a.name : "相册", s: sub + " · 应用内相册，不改原文件" };
    }
    if (r.kind === "tag") {
      const tg = S().tags.find(function (x) { return x.id === r.id || x.name === r.id; });
      return { t: "标签：" + ((tg && tg.name) || r.id) + " · " + n + " 张", s: sub };
    }
    if (r.kind === "source") {
      const s = S().sources.find(function (x) { return x.id === r.id; });
      return { t: (s && s.name) || "来源", s: sub };
    }
    if (r.kind === "settings" && (r.id === "index" || r.id === "indexing")) return { t: "索引管理", s: "设置 · 演示索引，不写入原文件" };
    if (App.filters.year && r.kind === "all") return { t: App.filters.year + " 年", s: sub };
    if (r.kind === "settings") return { t: "设置", s: "外观、图库来源与缩略图（原型）" };
    if (r.kind === "help") return { t: "帮助与快捷键", s: "本原型演示说明" };
    if (r.kind === "import") return { t: "添加受监控文件夹", s: "演示向导 · 不读取真实文件" };
    return { t: "所有照片", s: sub };
  }

  function toast(msg) {
    App.toasts.push({ id: Date.now() + Math.random(), msg: msg });
    renderOverlays();
    setTimeout(function () { App.toasts.shift(); renderOverlays(); }, 2800);
  }
  function closeMenu() { App.menu = null; renderOverlays(); }
  function closeModal() { App.modal = null; renderOverlays(); }
  function confirmDlg(opts) {
    App.modal = { kind: "confirm", title: opts.title, body: opts.body, danger: !!opts.danger, ok: opts.ok || "确定", cancel: opts.cancel || "取消", onOk: opts.onOk, focusCancel: opts.focusCancel !== false };
    renderOverlays();
    setTimeout(function () {
      const btn = document.querySelector(App.modal && App.modal.focusCancel ? '[data-act="modal-cancel"]' : '[data-act="modal-ok"]');
      if (btn) btn.focus();
    }, 30);
  }
  function confirmIndex(opts) { confirmDlg(Object.assign({ focusCancel: true, cancel: "取消" }, opts)); }
  function promptDlg(opts) {
    App.modal = { kind: "prompt", title: opts.title, body: opts.body, value: opts.value || "", placeholder: opts.placeholder || "", ok: opts.ok || "确定", onOk: opts.onOk };
    renderOverlays();
    setTimeout(function () { const inp = document.querySelector(".modal input"); if (inp) { inp.focus(); inp.select(); } }, 30);
  }

  function selectOne(id, additive, range) {
    const ids = visiblePhotos().map(function (p) { return p.id; });
    if (range && App.lastId) {
      const a = ids.indexOf(App.lastId), b = ids.indexOf(id);
      if (a >= 0 && b >= 0) { App.sel = ids.slice(Math.min(a, b), Math.max(a, b) + 1); render(); return; }
    }
    if (additive) {
      const i = App.sel.indexOf(id);
      if (i >= 0) App.sel.splice(i, 1); else App.sel.push(id);
    } else App.sel = [id];
    App.lastId = id;
    render();
  }
  function clearSel() { App.sel = []; render(); }
  function toggleStar(ids, value) {
    OPStore.patch(function (st) {
      st.photos.forEach(function (p) {
        if (ids.indexOf(p.id) >= 0) {
          p.starred = (value == null) ? !p.starred : !!value;
          if (p.starred && !p.rating) p.rating = 4;
          if (!p.starred) p.rating = 0;
        }
      });
    });
  }
  function setRating(ids, n) {
    OPStore.patch(function (st) {
      st.photos.forEach(function (p) {
        if (ids.indexOf(p.id) >= 0) { p.rating = n; p.starred = n > 0; }
      });
    });
  }
  function addToAlbum(albumId, ids) {
    ids = ids || App.sel.slice();
    if (!ids.length) { toast("请先选中照片，再加入相册。"); return; }
    const alb = OPStore.album(albumId);
    if (!alb) return;
    OPStore.patch(function (st) {
      st.photos.forEach(function (p) {
        if (ids.indexOf(p.id) >= 0) {
          p.albums = p.albums || [];
          if (p.albums.indexOf(albumId) < 0) p.albums.push(albumId);
        }
      });
    });
    toast("已将 " + ids.length + " 张加入「" + alb.name + "」。仅更新资料库，原文件未改动。");
  }
  function removeFromAlbum(albumId, ids) {
    ids = ids || App.sel.slice();
    OPStore.patch(function (st) {
      st.photos.forEach(function (p) {
        if (ids.indexOf(p.id) >= 0) p.albums = (p.albums || []).filter(function (x) { return x !== albumId; });
      });
    });
    toast("已从相册移出 " + ids.length + " 张。原文件仍在资料库中。");
  }
  function createAlbum(name) {
    name = (name || "").trim();
    if (!name) return "请输入相册名称";
    if (S().albums.some(function (a) { return a.name === name; })) return "已有同名相册，未覆盖。";
    const id = "alb-" + Date.now();
    OPStore.patch(function (st) { st.albums.push({ id: id, name: name, cover: (App.sel[0] || null) }); });
    if (App.sel.length) addToAlbum(id, App.sel.slice());
    else toast("已创建相册「" + name + "」。相册只存在于资料库。");
    return null;
  }
  function renameAlbum(id, name) {
    name = (name || "").trim();
    if (!name) return "名称不能为空";
    if (S().albums.some(function (a) { return a.name === name && a.id !== id; })) return "已有同名相册";
    OPStore.patch(function (st) { const a = st.albums.find(function (x) { return x.id === id; }); if (a) a.name = name; });
    return null;
  }
  function deleteAlbum(id) {
    const a = OPStore.album(id);
    confirmDlg({
      title: "删除相册「" + (a && a.name) + "」？",
      body: "只解除相册与照片的关联，不会从资料库移除照片，更不会改动或删除原文件。",
      danger: true, ok: "删除相册",
      onOk: function () {
        OPStore.patch(function (st) {
          st.albums = st.albums.filter(function (x) { return x.id !== id; });
          st.photos.forEach(function (p) { p.albums = (p.albums || []).filter(function (x) { return x !== id; }); });
        });
        const r = parseRoute();
        if (r.kind === "album" && r.id === id) go("#/all");
        toast("相册已删除。照片仍在资料库，原文件未改动。");
      }
    });
  }
  function createTag(name, applySel) {
    name = (name || "").trim();
    if (!name) return "名称不能为空";
    if (!App.sel.length) return "先选中照片再新建标签";
    if (S().tags.some(function (tg) { return tg.id === name || tg.name === name; })) return "已有同名标签，未覆盖。";
    OPStore.patch(function (st) { st.tags.push({ id: name, name: name, color: "#0485f7" }); });
    toggleTag(name, App.sel, true);
    toast("已为 " + App.sel.length + " 张打上「" + name + "」");
    return null;
  }
  function renameTag(id, name) {
    name = (name || "").trim();
    if (!name) return "名称不能为空";
    if (S().tags.some(function (t) { return (t.id === name || t.name === name) && t.id !== id; })) return "已有同名标签";
    OPStore.patch(function (st) {
      const t = st.tags.find(function (x) { return x.id === id; });
      if (t) { t.name = name; t.id = name; }
      st.photos.forEach(function (p) { p.tags = (p.tags || []).map(function (x) { return x === id ? name : x; }); });
    });
    const r = parseRoute();
    if (r.kind === "tag" && r.id === id) go("#/tag/" + encodeURIComponent(name));
    return null;
  }
  function deleteTag(id) {
    confirmDlg({
      title: "删除标签「" + id + "」？",
      body: "将从所有照片上移除该标签，不会从资料库移除照片，也不会改动原文件。",
      danger: true, ok: "删除标签",
      onOk: function () {
        OPStore.patch(function (st) {
          st.tags = st.tags.filter(function (t) { return t.id !== id; });
          st.photos.forEach(function (p) { p.tags = (p.tags || []).filter(function (x) { return x !== id; }); });
        });
        const r = parseRoute();
        if (r.kind === "tag" && r.id === id) go("#/all");
        toast("标签已删除。照片仍在资料库。");
      }
    });
  }
  function toggleTag(tid, ids, forceOn) {
    ids = ids || App.sel.slice();
    if (!ids.length) { toast("请先选中照片，再添加标签。"); return; }
    OPStore.patch(function (st) {
      st.photos.forEach(function (p) {
        if (ids.indexOf(p.id) < 0) return;
        p.tags = p.tags || [];
        const i = p.tags.indexOf(tid);
        if (forceOn === true && i < 0) p.tags.push(tid);
        else if (forceOn === false && i >= 0) p.tags.splice(i, 1);
        else if (forceOn == null) { if (i >= 0) p.tags.splice(i, 1); else p.tags.push(tid); }
      });
    });
  }
  function removeFromLibrary(ids) {
    ids = ids || App.sel.slice();
    if (!ids.length) return;
    confirmDlg({
      title: "从资料库移除 " + ids.length + " 张？",
      body: "只从 OpenPicasa 的资料库索引中移除这些条目，原照片文件会留在原文件夹，本原型也不会去改写或删除它们。",
      danger: true, ok: "从资料库移除",
      onOk: function () {
        OPStore.patch(function (st) {
          st.photos = st.photos.filter(function (p) { return ids.indexOf(p.id) < 0; });
          st.tray = st.tray.filter(function (id) { return ids.indexOf(id) < 0; });
        });
        App.sel = [];
        toast("已从资料库移除。原文件未改动。");
        const r = parseRoute();
        if (r.kind === "viewer" && ids.indexOf(r.id) >= 0) go("#/all");
      }
    });
  }
  function addToTray(ids) {
    ids = ids || App.sel.slice();
    if (!ids.length) { toast("请先选中照片。"); return; }
    OPStore.patch(function (st) {
      st.ui.trayOpen = true;
      ids.forEach(function (id) { if (st.tray.indexOf(id) < 0) st.tray.push(id); });
    });
    toast("已放入照片托盘 " + ids.length + " 张。");
  }

  function emptyFilters() {
    return { starred: false, tag: "", month: "", year: "", type: "", sizeClass: "", camera: "", lens: "", noDate: false, noMeta: false, sizeMin: "", sizeMax: "" };
  }
  function viewerList() {
    if (App.viewList && App.viewList.length) {
      const list = App.viewList.map(function (id) { return OPStore.photo(id); }).filter(Boolean);
      if (list.length) return list;
    }
    const vis = (typeof visiblePhotos === "function") ? visiblePhotos() : [];
    if (vis && vis.length) return vis;
    return S().photos.slice();
  }
  function clampZoom(n) {
    n = Math.round(Number(n) || 100);
    return Math.max(5, Math.min(800, n));
  }
  function isFitZoom() { return !App.zoomPct || App.zoom === "fit"; }
  function setZoomFit() { App.zoom = "fit"; App.zoomPct = 0; }
  function setZoomPct(n) { App.zoomPct = clampZoom(n); App.zoom = String(App.zoomPct); }
  function bumpZoom(dir) {
    const cur = isFitZoom() ? 100 : App.zoomPct;
    const step = cur < 100 ? 5 : (cur < 200 ? 10 : 25);
    setZoomPct(cur + dir * step);
  }
  function showLocation(ids) {
    ids = ids || (parseRoute().kind === "viewer" ? [parseRoute().id] : App.sel.slice());
    if (!ids.length) { toast("请先选中照片。"); return; }
    const lines = ids.slice(0, 8).map(function (id) {
      const p = OPStore.photo(id);
      if (!p) return "（找不到该条目）";
      const path = (folderPath(p.folder) || "示例路径") + "/" + (p.file || p.title);
      return path;
    });
    confirmDlg({
      title: "显示位置（演示）",
      body: "产品会在系统访达中显示原文件。本原型不会打开访达，也不会读写真实照片。<br><br>" + lines.map(function (s) { return "<code>" + esc(s) + "</code>"; }).join("<br>") +
        (ids.length > 8 ? "<br>…共 " + ids.length + " 项" : ""),
      ok: "知道了", cancel: "关闭",
      onOk: function () { toast("已请求在访达中显示（演示，未打开系统访达，未改原文件）"); }
    });
  }
  function moveToTrash(ids) {
    ids = ids || (parseRoute().kind === "viewer" ? [parseRoute().id] : App.sel.slice());
    if (!ids.length) { toast("请先选中照片。"); return; }
    const n = ids.length;
    const paths = ids.slice(0, 6).map(function (id) {
      const p = OPStore.photo(id);
      return p ? ((folderPath(p.folder) || "") + "/" + (p.file || p.title)) : id;
    });
    confirmDlg({
      title: "移到废纸篓？",
      body: "产品会把这 " + n + " 张的<strong>原文件</strong>移动到系统废纸篓" +
        "（Unix 启动盘 ~/.Trash，外接卷 /Volumes/{卷}/.Trashes/{uid}；跨卷会失败；Windows 暂不支持）。" +
        "索引条目随后一并移除。<br><br>本原型<strong>只模拟</strong>该结果，不会读写、移动或删除你磁盘上的真实照片。<br><br>" +
        paths.map(function (s) { return "<code>" + esc(s) + "</code>"; }).join("<br>") +
        (n > 6 ? "<br>…共 " + n + " 张" : ""),
      danger: true, ok: "移到废纸篓", cancel: "取消",
      onOk: function () {
        OPStore.patch(function (st) {
          st.photos = st.photos.filter(function (p) { return ids.indexOf(p.id) < 0; });
          st.tray = st.tray.filter(function (id) { return ids.indexOf(id) < 0; });
        });
        App.sel = [];
        toast("已模拟移到废纸篓（" + n + " 张）。本原型未移动真实文件。");
        const r = parseRoute();
        if (r.kind === "viewer" && ids.indexOf(r.id) >= 0) go("#/all");
      }
    });
  }
  function openFolderPicker() {
    App.picker = { kind: "folder", sel: null };
    renderOverlays();
  }
  function openDemoPackPicker() {
    App.picker = { kind: "pack", sel: null };
    renderOverlays();
  }
  function openRemapPicker(fromPath) {
    App.picker = { kind: "remap", sel: null, fromPath: fromPath || "" };
    renderOverlays();
  }
  function pickerHtml() {
    const p = App.picker;
    if (!p) return "";
    let title = "选择照片文件夹（只读，不会修改你的文件）";
    let note = "产品会打开系统选目录。本原型使用演示目录列表，不会打开系统文件对话框，也不会读取你磁盘上的真实照片。";
    let items = OPData.ADD_FOLDER_PRESETS || [];
    if (p.kind === "pack") {
      title = "选择演示索引包";
      note = "产品会打开系统选文件框，读取真实 .opicasaidx。本原型只提供演示 JSON 包，不是真实数据库格式。";
      items = OPData.DEMO_PACKS || [];
    }
    if (p.kind === "remap") {
      title = "重定位根目录（演示）";
      note = "请选择用来对应「" + esc(p.fromPath || "") + "」的本机演示目录。不复制、不移动、不重命名照片。";
    }
    let list = "";
    items.forEach(function (it) {
      const on = p.sel === it.id ? " on" : "";
      const sub = (it.path || "") + (it.note ? " · " + it.note : "") + (it.photos != null ? " · " + it.photos + " 张" : (it.count ? " · " + it.count + " 张" : ""));
      list += '<div class="picker-item' + on + '" data-act="picker-sel" data-id="' + escAttr(it.id) + '"><b>' + esc(it.name) + "</b><span>" + esc(sub) + "</span></div>";
    });
    return '<div class="modal-bg" data-act="picker-cancel"><div class="modal wide" role="dialog" aria-modal="true" data-act="noop">' +
      "<h3>" + esc(title) + "</h3><p class=\"note\">" + note + "</p>" +
      '<div class="picker-list">' + list + "</div>" +
      '<div class="modal-acts"><button class="btn ghost" data-act="picker-cancel">取消</button>' +
      '<button class="btn primary" data-act="picker-ok"' + (p.sel ? "" : " disabled") + ">确定</button></div></div></div>";
  }
  function applyFolderPreset(preset) {
    if (!preset) return;
    if (preset.kind === "duplicate") {
      toast("这个文件夹已经在图库里，正在重新索引");
      if (window.OPMgmt) OPMgmt.startTask({ scope: preset.path, kind: "刷新", target: 16 });
      return;
    }
    if (preset.kind === "inside") {
      ingestNewFolder(preset, { skipRootDup: true });
      toast("已添加；它的照片本来也在关注范围内，不会重复计数");
      return;
    }
    if (preset.kind === "wraps") {
      ingestNewFolder(preset);
      toast("已添加；它包含 " + (preset.wraps || 1) + " 个已关注的文件夹，照片不会重复计数");
      return;
    }
    if (preset.kind === "denied") {
      toast("没有读取权限（系统设置 → 隐私与安全性 → 文件与文件夹）：" + (preset.err || preset.path));
      return;
    }
    if (preset.kind === "missing") {
      toast("目录不存在，或所在磁盘没有挂载：" + (preset.err || preset.path));
      return;
    }
    ingestNewFolder(preset);
  }
  function ingestNewFolder(preset, opts) {
    opts = opts || {};
    const id = "f-" + preset.id + "-" + Date.now().toString(36);
    OPStore.patch(function (st) {
      st.folders.push({ id: id, name: preset.name.split("/").pop(), path: preset.path, parent: null, source: preset.id });
      if (!st.sources.some(function (s) { return s.id === preset.id; })) {
        st.sources.push({ id: preset.id, name: preset.name, path: preset.path, status: preset.kind === "new-offline" ? "offline" : "online", note: preset.note || "演示来源" });
      }
      if (!st.mgmt.roots.some(function (r) { return r.path === preset.path; })) {
        st.mgmt.roots.push({
          id: preset.id, path: preset.path, status: preset.kind === "new-offline" ? "offline" : "normal",
          watch: preset.kind === "new-offline" ? "none" : "live", watchReason: preset.kind === "new-offline" ? "卷未挂载（演示）" : "",
          photos: preset.count || 0, marks: 0, thumbsBytes: 0, sourceId: preset.id
        });
      }
    });
    if (window.OPMgmt) OPMgmt.startTask({ scope: preset.path, kind: "刷新", target: Math.max(6, preset.count || 8) });
    toast("已加入关注并开始演示索引：" + preset.path + "（不读取真实磁盘）");
    go("#/folder/" + encodeURIComponent(id));
  }
  function applyPackPick(pack) {
    if (!pack) return;
    App.picker = null;
    if (window.OPMgmt && OPMgmt.startValidate) OPMgmt.startValidate(pack.id);
    else toast("已选择演示包 " + pack.name);
  }
  function applyRemap(preset) {
    const from = (App.picker && App.picker.fromPath) || "";
    App.picker = null;
    if (!preset) return;
    if (preset.kind === "missing" || preset.kind === "denied") {
      toast("无法重定位：该演示目录不可用（" + preset.path + "）");
      return;
    }
    toast("已将「" + from + "」对应到「" + preset.path + "」（演示，未复制、未移动、未重命名照片）");
  }
  function folderHidden(f, st) {
    st = st || S();
    if (!f) return true;
    const path = f.path || "";
    const roots = (st.mgmt && st.mgmt.roots) || [];
    if (roots.some(function (r) {
      if (r.status !== "closed" && r.status !== "removed") return false;
      return path === r.path || (r.path && path.indexOf(r.path + "/") === 0) || f.source === r.sourceId || f.id === r.id;
    })) return true;
    const ex = (st.mgmt && st.mgmt.exclusions) || [];
    if (ex.some(function (e) { return path === e.path || (e.path && path.indexOf(e.path + "/") === 0); })) return true;
    return false;
  }
  function isFolderRoot(f) {
    if (!f) return false;
    if (!f.parent) return true;
    return (S().mgmt.roots || []).some(function (r) { return r.path === f.path || r.id === f.id || r.sourceId === f.source && !f.parent; });
  }
  function folderCtxHtml(id) {
    const f = S().folders.find(function (x) { return x.id === id; });
    if (!f) return "";
    const root = isFolderRoot(f);
    let h = '<div class="mh">' + esc(f.name) + "</div>";
    h += '<div class="mi" data-act="show-finder" data-path="' + escAttr(f.path || "") + '">在 Finder 中打开</div>';
    h += '<div class="mi" data-act="folder-refresh" data-id="' + escAttr(id) + '">刷新此文件夹</div>';
    if (root) {
      h += '<div class="sep"></div>';
      h += '<div class="mi" data-act="folder-close-watch" data-id="' + escAttr(id) + '">关闭关注文件夹（保留索引）</div>';
      h += '<div class="mi" data-act="folder-remove" data-id="' + escAttr(id) + '">从图库移除（删索引，不删文件）</div>';
    } else {
      h += '<div class="sep"></div>';
      h += '<div class="mi" data-act="folder-exclude" data-id="' + escAttr(id) + '">关闭此文件夹（不再显示）</div>';
    }
    return h;
  }
  function importSrcCtxHtml(id) {
    const s = ((S().mgmt && S().mgmt.importSources) || []).find(function (x) { return x.id === id; });
    if (!s) return "";
    return '<div class="mh">' + esc(s.name) + "</div>" +
      '<div class="mi" data-act="impsrc-hide" data-id="' + escAttr(id) + '">仅从分组隐藏</div>' +
      '<div class="mi" data-act="impsrc-remove" data-id="' + escAttr(id) + '">从导入的索引中移除…</div>';
  }
  function trayCtxHtml(id) {
    return '<div class="mh">照片托盘</div><div class="mi" data-act="tray-remove-one" data-id="' + escAttr(id) + '">从托盘移出</div>';
  }
  function toggleFullscreen() {
    const el = document.documentElement;
    if (!document.fullscreenElement) {
      if (el.requestFullscreen) el.requestFullscreen().catch(function () { toast("浏览器拒绝了全屏（演示）"); });
    } else if (document.exitFullscreen) document.exitFullscreen();
  }

  function armViewerIdle() {
    clearTimeout(App._idle);
    App.chromeHidden = false;
    const root = document.getElementById("app");
    if (root) root.classList.remove("chrome-hidden");
    App._idle = setTimeout(function () {
      if (parseRoute().kind !== "viewer") return;
      if (App.modal || App.menu || App.picker) return;
      App.chromeHidden = true;
      const root = document.getElementById("app");
      if (root) root.classList.add("chrome-hidden");
    }, 2500);
  }
  function startViewerLoad(id) {
    if (App.viewerPhaseId === id) return;
    App.viewerPhaseId = id;
    App.viewerPhase = "loading";
    App.comparePhase = App.compare ? "loading" : "ready";
    clearTimeout(App._vl1);
    clearTimeout(App._vl2);
    App._vl1 = setTimeout(function () {
      if (App.viewerPhaseId !== id) return;
      App.viewerPhase = "decoding";
      if (parseRoute().kind === "viewer") render();
    }, 220);
    App._vl2 = setTimeout(function () {
      if (App.viewerPhaseId !== id) return;
      App.viewerPhase = "ready";
      App.comparePhase = "ready";
      if (parseRoute().kind === "viewer") render();
    }, 500);
  }
  function startImport() { openFolderPicker(); }
  function rememberIndexTick(jobId, kind, presetId) {
    App.indexTick = { id: jobId, kind: kind || "rebuild", presetId: presetId || null };
  }
  function scheduleIndexTick() {
    setTimeout(function () {
      const meta = App.indexTick;
      const job = S().indexJob;
      if (!meta || !job || job.id !== meta.id || job.status !== "running") return;
      if (meta.kind === "import" && App.importW && App.importW.jobId === meta.id && App.importW.running === false) return;
      const pct = Math.min(100, (job.pct || 0) + 10 + Math.round(Math.random() * 8));
      const done = Math.min(job.total, Math.round(pct / 100 * job.total));
      if (pct >= 100) {
        if (meta.kind === "import") {
          const preset = OPData.IMPORT_PRESETS.find(function (x) { return x.id === meta.presetId; });
          if (App.importW && App.importW.jobId === meta.id) App.importW.running = false;
          finishImport(preset, meta.id);
        } else {
          completeRebuild(meta.id);
        }
        return;
      }
      OPStore.patch(function (st) {
        if (!st.indexJob || st.indexJob.id !== meta.id) return;
        st.indexJob.pct = pct;
        st.indexJob.done = done;
        st.indexJob.status = "running";
      });
      if (App.importW && App.importW.jobId === meta.id) App.importW.pct = pct;
      scheduleIndexTick();
    }, 180);
  }
  function runImport() {
    const w = App.importW;
    if (!w || !w.src) return;
    const preset = OPData.IMPORT_PRESETS.find(function (x) { return x.id === w.src; });
    const total = (preset && preset.count) || 6;
    const jobId = "job-" + Date.now();
    w.running = true; w.step = 3; w.pct = 0; w.result = null; w.jobId = jobId;
    OPStore.patch(function (st) {
      st.indexJob = {
        id: jobId, kind: "import", presetId: w.src,
        label: "导入 " + ((preset && preset.name) || "演示来源"),
        status: "running", pct: 0, done: 0, total: total, added: 0,
        note: "演示索引进行中，不会读取真实文件，也不会写入原照片文件夹。"
      };
    });
    rememberIndexTick(jobId, "import", w.src);
    scheduleIndexTick();
  }
  function finishImport(preset, jobId) {
    const n = preset ? preset.count : 6;
    const pool = S().photos.slice();
    const added = [];
    if (App.importW) {
      App.importW.running = false;
      App.importW.step = 4;
      App.importW.pct = 100;
    }
    OPStore.patch(function (st) {
      for (let i = 0; i < Math.min(n, pool.length); i++) {
        const src = pool[i];
        const id = "imp-" + Date.now() + "-" + i;
        st.photos.unshift({
          id: id, file: src.file, title: "导入演示 " + (i + 1),
          w: src.w, h: src.h, bytes: src.bytes,
          date: "2024-05-22", time: "10:" + String(10 + i).padStart(2, "0"),
          folder: "2024-05", starred: false, rating: 0, tags: [], albums: [],
          recent: true, offline: !!(preset && preset.risk === "offline"),
          source: (preset && preset.risk === "offline") ? "sd" : "pictures",
          demoImport: true
        });
        added.push(id);
      }
      st.ui.libraryEmpty = false;
      st.indexJob = {
        id: jobId || ((st.indexJob && st.indexJob.id) || ("job-" + Date.now())),
        kind: "import", presetId: preset && preset.id,
        label: "导入 " + ((preset && preset.name) || "演示来源"),
        pct: 100, status: "done",
        added: added.length, done: added.length, total: added.length,
        note: "演示索引已完成，并未扫描真实磁盘，原文件未被写入。"
      };
    });
    if (App.importW) {
      App.importW.result = { ok: added.length, skip: 0, fail: 0, note: "以上为演示数据，并未扫描真实磁盘。" };
    }
    render();
    toast("索引完成：成功 " + added.length + " 张。原文件夹未被写入。");
  }
  function completeRebuild(jobId) {
    const total = Math.max(0, S().photos.length);
    OPStore.patch(function (st) {
      st.indexJob = {
        id: jobId || ((st.indexJob && st.indexJob.id) || ("job-" + Date.now())),
        kind: "rebuild", label: "重建索引", status: "done",
        pct: 100, done: total, total: total, added: 0,
        note: "演示索引已完成，未扫描真实磁盘，原文件未改动。"
      };
    });
    toast("演示索引完成。未扫描真实磁盘，原文件未改动。");
  }
  function traffic() { return '<div class="traffic"><i class="r"></i><i class="y"></i><i class="g"></i></div>'; }
  function titlebar(extraRight) {
    const isV = parseRoute().kind === "viewer";
    return '<div class="titlebar">' + traffic() + (isV ? viewerBack() : '<div class="appname">OpenPicasa</div>') + (extraRight || "") + "</div>";
  }
  function viewerBack() { return '<button class="back" type="button" data-act="back" title="返回">' + icon("back", 14, 2) + "返回</button>"; }
  function folderCrumb(r) {
    if (r.kind !== "folder") return '<div class="crumb-bar"><span class="cur">图库</span></div>';
    const st = S();
    const f = st.folders.find(function (x) { return x.id === r.id; });
    if (!f) return '<div class="crumb-bar"><span class="cur">图库</span></div>';
    const chain = [];
    let cur = f;
    while (cur) { chain.unshift(cur); cur = st.folders.find(function (x) { return x.id === cur.parent; }); }
    let h = '<div class="crumb-bar"><a href="#/all" title="转到「图库」">图库</a>';
    chain.forEach(function (c, i) {
      h += '<span class="sep">/</span>';
      if (i === chain.length - 1) h += '<span class="cur">' + esc(c.name) + "</span>";
      else h += '<a href="#/folder/' + encodeURIComponent(c.id) + '" title="转到「' + escAttr(c.name) + '」">' + esc(c.name) + "</a>";
    });
    return h + "</div>";
  }
  function toolbar(title, sub) {
    const st = S();
    const layout = st.settings.layout || "grid";
    const r = parseRoute();
    const allStarred = App.sel.length && App.sel.every(function (id) { const p = OPStore.photo(id); return p && p.starred; });
    const inTray = App.sel.length && App.sel.every(function (id) { return st.tray.indexOf(id) >= 0; });
    const crumb = (r.kind !== "settings" && r.kind !== "help" && r.kind !== "viewer") ? folderCrumb(r) : "";
    const titleHtml = '<div class="title-block">' + (crumb || ('<h1>' + esc(title) + "</h1>")) + (sub ? ('<span class="sub">' + esc(sub) + "</span>") : "") + "</div>";
    const n = clampThumb((st.settings && st.settings.thumbSize) || 144);
    const scope = st.ui.searchScope === "folder" ? "文" : "全";
    const scopeTip = scope === "文" ? "搜索范围：当前文件夹。点击改为搜索全部图库" : "搜索范围：全部图库。点击改为搜索当前文件夹";
    return '<div class="toolbar">' +
      '<button class="ib" data-act="toggle-side" title="显示/隐藏边栏">' + icon("sidebar") + "</button>" +
      '<button class="ib" data-act="import" title="导入：添加文件夹…">' + icon("import") + "</button>" +
      '<button class="ib" data-act="slideshow" title="幻灯片（演示扩展；产品该按钮为下一版开放）">' + icon("play") + "</button>" +
      '<button class="ib" data-act="edit-nyi" title="编辑（下一版开放）">' + icon("edit") + "</button>" +
      '<button class="ib' + (allStarred ? " on" : "") + '" data-act="fav-toggle" title="' + (allStarred ? "取消收藏" : "收藏") + '">' + icon("heart", 15, allStarred ? 0 : 1.75, allStarred ? "currentColor" : "none") + "</button>" +
      '<button class="ib" data-act="open-caption" title="说明">' + icon("caption") + "</button>" +
      '<button class="ib" data-act="tag-menu" title="标签">' + icon("tag") + "</button>" +
      '<button class="ib' + (inTray ? " on" : "") + '" data-act="tray-toggle" title="' + (inTray ? "移出照片托盘" : "照片托盘 · ⌘H") + '">' + icon("plus") + "</button>" +
      '<div class="sep"></div>' + titleHtml +
      '<div class="grow"></div>' +
      '<div class="seg" title="布局">' +
        '<b data-layout="grid" class="' + (layout === "grid" ? "on" : "") + '" title="方格">' + icon("grid", 15) + "</b>" +
        '<b data-layout="waterfall-v" class="' + (layout === "waterfall-v" ? "on" : "") + '" title="纵向瀑布流">' + icon("waterfallV", 15) + "</b>" +
        '<b data-layout="waterfall-h" class="' + (layout === "waterfall-h" || layout === "masonry" ? "on" : "") + '" title="横向瀑布流">' + icon("waterfallH", 15) + "</b>" +
      "</div>" +
      '<div class="search' + (App.q ? " has-val" : "") + '" title="搜索 · ⌘F">' + icon("search", 14) +
        '<button type="button" class="search-scope" data-act="toggle-scope" title="' + escAttr(scopeTip) + '">' + scope + "</button>" +
        '<input id="search-input" type="search" placeholder="搜索" value="' + escAttr(App.q) + '">' +
        '<button class="clear" type="button" data-act="clear-search" title="清除搜索">' + icon("x", 12, 2) + "</button></div>" +
      '<button class="ib" data-act="sort-menu" title="排序">' + icon("sort") + "</button>" +
      '<button class="ib' + (filterActive() && !App.q ? " on" : "") + '" data-act="filter-menu" title="筛选（日期 / 类型 / 尺寸 / 标签）">' + icon("filter") + "</button>" +
      '<button class="ib" data-act="more-menu" title="更多">' + icon("more", 16, 0, "currentColor") + "</button></div>";
  }
  function rowHtml(on, ico, name, cnt, href, indent, extraCls, tip) {
    return '<a class="row' + (on ? " on" : "") + (indent ? " " + indent : "") + (extraCls || "") + '" href="' + href + '"' + (tip ? (' title="' + escAttr(tip) + '"') : "") + ">" +
      ico + '<span class="name">' + esc(name) + "</span>" +
      (cnt != null ? '<span class="cnt tnum">' + esc(String(cnt)) + "</span>" : "") + "</a>";
  }
  function isCollapsed(key) {
    return !!(S().settings.collapsed || {})[key];
  }
  function secHead(key, label, extra) {
    const col = isCollapsed(key);
    return '<div class="sec collapsible" data-act="toggle-sec" data-sec="' + key + '"><span class="chev">' + (col ? "▸" : "▾") + "</span>" + esc(label) + (extra || "") + "</div>";
  }
  function folderDot(fid) {
    const jobs = (S().mgmt && S().mgmt.tasks && S().mgmt.tasks.running) || [];
    const folders = S().folders;
    const f = folders.find(function (x) { return x.id === fid; });
    const path = (f && f.path) || "";
    const job = jobs.find(function (t) { return path && (t.scope === path || path.indexOf(t.scope) === 0 || t.scope.indexOf(path) === 0); });
    if (!job) return "";
    const stt = job.userPaused ? "pause" : (job.state === "已完成" ? "done" : (job.state === "部分失败" || job.state === "不可访问" ? "fail" : (job.state === "等待" ? "wait" : "scan")));
    const tip = (job.state || "") + "：" + (f && f.name || "") + " · 已完成 " + (job.completed || 0) + " / 已发现 " + (job.discovered || 0);
    return '<span class="idot ' + stt + '" title="' + escAttr(tip) + '"></span>';
  }
  function sidebar() {
    const st = S(), r = parseRoute();
    if (!st.ui.sidebarOpen && r.kind !== "viewer") return "";
    const w = st.settings.sidebarWidth || 228;
    const allN = st.photos.length;
    const recN = st.photos.filter(function (p) { return p.recent; }).length;
    const chN = st.photos.filter(function (p) { return (p.changedAt || "") >= "2024-05-21"; }).length;
    const starN = st.photos.filter(function (p) { return p.starred; }).length;
    let html = '<aside class="side" style="width:' + w + 'px;--side-w:' + w + 'px"><div class="side-scroll">';
    html += secHead("lib", "图库");
    if (!isCollapsed("lib")) {
      html += rowHtml(r.kind === "all" || r.kind === "viewer", icon("photo", 15), "所有照片", allN || null, "#/all");
      html += rowHtml(r.kind === "changed", icon("clock", 15), "最近更改", chN || null, "#/changed");
      html += rowHtml(r.kind === "recent", icon("clock", 15), "最近导入", recN || null, "#/recent");
      html += rowHtml(r.kind === "stars" || !!App.filters.starred, icon("star", 15), "星标", starN || null, "#/stars", "", "", "只看收藏（星标）的照片");
      const years = {};
      st.photos.forEach(function (p) { const y = (p.date || "").slice(0, 4); if (y) years[y] = (years[y] || 0) + 1; });
      Object.keys(years).sort().reverse().forEach(function (y) {
        html += '<a class="row year-row' + (App.filters.year === y ? " on" : "") + '" href="#/all" data-act="filter-year" data-year="' + y + '"><span class="name">' + y + '</span><span class="cnt tnum">' + years[y] + "</span></a>";
      });
    }
    const tagExtra = '<span class="sp"></span><button class="mini" data-act="tag-sort-menu" title="标签排序 / 显示空标签">' + icon("more", 12, 0, "currentColor") + '</button><button class="mini" data-act="new-tag" title="新建标签">' + icon("plus", 12) + "</button>";
    html += secHead("tags", "标签", tagExtra);
    if (!isCollapsed("tags")) {
      if (App.creating === "tag") {
        html += '<div class="inline-new' + (App.createErr ? " error" : "") + '"><input id="create-input" placeholder="新的标签"><div class="hint">' + (App.createErr || "⏎ 保存 · Esc 取消") + "</div></div>";
      }
      let tags = st.tags.slice();
      const counts = {};
      tags.forEach(function (tg) { counts[tg.id] = OPStore.photosWithTag(tg.id).length; });
      if (st.settings.tagsSort === "count") tags.sort(function (a, b) { return (counts[b.id] || 0) - (counts[a.id] || 0); });
      else tags.sort(function (a, b) { return a.name.localeCompare(b.name, "zh"); });
      tags.forEach(function (tg, i) {
        const n = counts[tg.id] || 0;
        if (n === 0 && !st.settings.showZeroTags) return;
        const viewN = (r.kind !== "all") ? visiblePhotos().filter(function (p) { return (p.tags || []).indexOf(tg.id) >= 0; }).length : null;
        const dot = '<span class="dot' + (i === 1 ? " t2" : i === 2 ? " t3" : "") + '"></span>';
        html += '<a class="row' + (r.kind === "tag" && r.id === tg.id ? " on" : "") + '" href="#/tag/' + encodeURIComponent(tg.id) + '" data-ctx="tag" data-id="' + escAttr(tg.id) + '" title="' + escAttr(tg.name) + '">' +
          dot + '<span class="name">' + esc(tg.name) + "</span>" +
          (viewN != null && r.kind !== "all" && r.kind !== "tag" ? '<span class="cnt tnum">' + n + " / " + viewN + "</span>" : '<span class="cnt tnum">' + n + "</span>") + "</a>";
      });
    }
    html += secHead("albums", "相册", '<span class="sp"></span><button class="mini" data-act="new-album" title="新建相册">' + icon("plus", 12) + "</button>");
    if (!isCollapsed("albums")) {
      if (App.creating === "album") {
        html += '<div class="inline-new' + (App.createErr ? " error" : "") + '"><input id="create-input" placeholder="新相册"><div class="hint">' + (App.createErr || "⏎ 保存 · Esc 取消") + "</div></div>";
      }
      if (theme() === "dark") {
        html += '<div class="alb-covers">';
        st.albums.forEach(function (a) {
          const cover = a.cover && OPStore.photo(a.cover);
          const src = cover && cover.file ? OPData.photoSrc(cover.file) : "";
          html += '<a class="cover' + (r.kind === "album" && r.id === a.id ? " on" : "") + '" href="#/album/' + encodeURIComponent(a.id) + '" data-ctx="album" data-id="' + escAttr(a.id) + '">' +
            (src ? '<img src="' + src + '" alt="">' : '<div style="aspect-ratio:1;background:#222;border-radius:8px"></div>') +
            "<span>" + esc(a.name) + "</span></a>";
        });
        html += "</div>";
      } else {
        st.albums.forEach(function (a) {
          const n = OPStore.photosInAlbum(a.id).length;
          html += '<a class="row' + (r.kind === "album" && r.id === a.id ? " on" : "") + '" href="#/album/' + encodeURIComponent(a.id) + '" data-ctx="album" data-id="' + escAttr(a.id) + '">' +
            icon("album", 15) + '<span class="name">' + esc(a.name) + "</span>" + (n ? '<span class="cnt tnum">' + n + "</span>" : "") + "</a>";
        });
      }
    }
    html += secHead("folders", "文件夹");
    if (!isCollapsed("folders")) {
      const tree = st.folders.filter(function (f) {
        if ((f.source || "").indexOf("imp") === 0) return false;
        return !folderHidden(f, st);
      });
      if (!tree.length) html += '<p class="note" style="padding:6px 10px">还没有加入任何文件夹<br>点顶栏的导入按钮，选择一个文件夹</p>';
      tree.forEach(function (f) {
        const indent = !f.parent ? "" : (f.parent === "pictures" ? "indent" : "indent2");
        const n = OPStore.folderLayerCount(f.id);
        const cnt = n ? n : null;
        html += '<a class="row' + (r.kind === "folder" && r.id === f.id ? " on" : "") + (indent ? " " + indent : "") + '" href="#/folder/' + encodeURIComponent(f.id) + '" data-ctx="folder" data-id="' + escAttr(f.id) + '">' +
          icon("folder", 15) + folderDot(f.id) + '<span class="name">' + esc(f.name) + "</span>" +
          (cnt != null ? '<span class="cnt tnum">' + cnt + "</span>" : "") + "</a>";
      });
    }
    const sources = (st.mgmt && st.mgmt.importSources) || [];
    const visibleSrc = sources.filter(function (s) { return !s.hidden && (st.mgmt.hiddenImportSources || []).indexOf(s.id) < 0; });
    if (visibleSrc.length) {
      html += secHead("impsrc", "导入的索引");
      if (!isCollapsed("impsrc")) {
        visibleSrc.forEach(function (s) {
          const n = st.photos.filter(function (p) { return p.source === s.id; }).length;
          let tip = "导入时间：" + (window.OPMgmt ? OPMgmt.relCoarse(s.importedAt) : "") + "\n";
          if (!s.online) tip += "离线：导出时的原路径当前不可读\n";
          tip += "原路径：" + (s.paths || []).join("、") + "\n右键可从导入的索引中移除（不碰原文件）";
          html += '<a class="row' + (s.online ? "" : " offline-src") + (r.kind === "source" && r.id === s.id ? " on" : "") + '" href="#/source/' + encodeURIComponent(s.id) + '" data-ctx="impsrc" data-id="' + escAttr(s.id) + '" title="' + escAttr(tip) + '">' +
            icon("import", 15) + '<span class="name">' + esc(s.name) + "</span>" + (n ? '<span class="cnt tnum">' + n + "</span>" : "") + "</a>";
        });
      }
    }
    html += "</div>";
    html += '<div class="tray' + (st.ui.trayOpen ? " open" : "") + '">';
    html += '<div class="tray-h" data-act="toggle-tray"><span>照片托盘' + (st.tray.length ? " · " + st.tray.length + " 张" : "（空）") + "</span>";
    if (st.tray.length) html += '<span class="act" data-act="clear-tray">清空</span>';
    html += '<span class="act" data-act="toggle-tray">' + (st.ui.trayOpen ? "▾" : "▸") + "</span></div>";
    html += '<div class="tray-body">';
    if (!st.tray.length) html += '<div class="tray-empty">把照片放入托盘以便批量整理。不会改动原文件。</div>';
    st.tray.forEach(function (id) {
      const ph = OPStore.photo(id);
      if (ph && ph.file) html += '<img src="' + OPData.photoSrc(ph.file) + '" alt="" data-open="' + ph.id + '" data-ctx="tray" data-id="' + ph.id + '" title="' + escAttr(ph.title) + '">';
    });
    html += "</div></div></aside><div class=\"side-resizer\" data-act=\"side-resize\"></div>";
    return html;
  }

  function clampThumb(n) {
    n = parseInt(n, 10);
    if (isNaN(n)) n = 144;
    return Math.max(OPData.THUMB_MIN, Math.min(OPData.THUMB_MAX, n));
  }
  function applyThumbVar(n) {
    n = clampThumb(n != null ? n : ((S().settings && S().settings.thumbSize) || 144));
    document.documentElement.style.setProperty("--thumb-size", n + "px");
    document.documentElement.setAttribute("data-thumb", String(n));
    if (n < 64) document.documentElement.setAttribute("data-thumb-sm", "1");
    else document.documentElement.removeAttribute("data-thumb-sm");
    return n;
  }
  function applyThumbSize(n, silent) {
    n = OPStore.setThumbSize(n, silent !== false);
    applyThumbVar(n);
    const labels = document.querySelectorAll("[data-thumb-val]");
    for (let i = 0; i < labels.length; i++) labels[i].textContent = n + " px";
    const sliders = document.querySelectorAll("input.thumb-range");
    for (let i = 0; i < sliders.length; i++) {
      if (sliders[i].value !== String(n)) sliders[i].value = String(n);
    }
    if (S().settings.layout === "masonry") layoutJustified();
    return n;
  }
  function indexSnapshot() {
    const st = S();
    const job = st.indexJob || OPData.defaultIndexJob(st.photos.length);
    const status = job.status || "idle";
    const total = job.total || 0;
    const done = job.done || 0;
    const pct = job.pct == null ? (total ? Math.round(done / total * 100) : 0) : job.pct;
    return { job: job, status: status, total: total, done: done, pct: pct, label: job.label || "" };
  }
  function statusLabel(status) {
    if (status === "running") return "正在索引";
    if (status === "paused") return "已暂停";
    if (status === "done") return "索引完成";
    return "索引空闲";
  }
  function isIndexPage(r) {
    r = r || parseRoute();
    return r.kind === "settings" && (r.id === "index" || r.id === "indexing");
  }
  function thumbSliderHtml(wide) {
    const n = clampThumb((S().settings && S().settings.thumbSize) || 144);
    return '<label class="status-size"><span class="sz-lab">缩略图</span>' +
      '<input class="thumb-range' + (wide ? " wide" : "") + '" data-act="thumb-range" type="range" min="' + OPData.THUMB_MIN + '" max="' + OPData.THUMB_MAX + '" step="1" value="' + n + '" aria-label="缩略图边长">' +
      '<span class="tnum" data-thumb-val="1">' + n + " px</span></label>";
  }
  function statusIndexHtml() {
    if (window.OPMgmt) return OPMgmt.statusHtml();
    return "";
  }
  function settingsIndexHtml() {
    if (window.OPMgmt) return OPMgmt.settingsSummaryHtml();
    return '<p class="note">演示索引，未扫描真实磁盘。</p>';
  }

  function statusbar() {
    const sel = App.sel.length ? '<span class="status-sel">已选 ' + App.sel.length + " 张</span>" : "";
    return '<div class="status">' + sel + statusIndexHtml() + '<div class="sp"></div>' + thumbSliderHtml(false) + "</div>";
  }
  function batchBar() {
    if (!App.sel.length) return "";
    const r = parseRoute();
    return '<div class="batch"><span>已选 ' + App.sel.length + " 张</span><div class=\"sp\"></div>" +
      '<button class="btn" data-act="star-sel">' + icon("star", 14) + "加星标</button>" +
      '<button class="btn" data-act="album-menu">' + icon("album", 14) + "加入相册</button>" +
      '<button class="btn" data-act="tag-menu">' + icon("tag", 14) + "标签</button>" +
      '<button class="btn" data-act="tray-sel">' + icon("plus", 14) + "放入托盘</button>" +
      (r.kind === "album" ? '<button class="btn" data-act="remove-from-album" title="演示扩展：产品相册移出能力未作为已实装项声称">从相册移出（演示扩展）</button>' : "") +
      '<button class="btn danger" data-act="trash-sel">移到废纸篓…</button>' +
      '<button class="btn ghost" data-act="clear-sel">取消选择</button></div>';
  }
  function cellHtml(p, opts) {
    opts = opts || {};
    const on = App.sel.indexOf(p.id) >= 0;
    const off = p.offline || (S().ui.forceOffline && p.source === "sd");
    const fn = (App.showFilenames && !opts.noName && (S().settings.layout || "grid") === "grid") ? ('<span class="fn">' + esc(p.file || p.title || "") + "</span>") : "";
    if (p.thumbState === "pending") {
      return '<div class="cell ph-cell' + (on ? " sel" : "") + '" data-id="' + p.id + '" data-ph="1"><span class="ph-lab">待生成</span>' + fn + "</div>";
    }
    if (p.thumbState === "failed") {
      return '<div class="cell ph-cell ph-fail' + (on ? " sel" : "") + '" data-id="' + p.id + '" data-ph="1"><span class="ph-lab">失败</span><span class="ph-bang">!</span>' + fn + "</div>";
    }
    if (p.thumbState === "cloud") {
      return '<div class="cell ph-cell ph-cloud' + (on ? " sel" : "") + '" data-id="' + p.id + '" data-ph="1"></div>';
    }
    const src = p.file ? OPData.photoSrc(p.file) : "";
    return '<div class="cell' + (on ? " sel" : "") + (off ? " offline" : "") + '" data-id="' + p.id + '">' +
      (src ? '<img src="' + src + '" alt="' + escAttr(p.title) + '">' : "") +
      '<span class="badge-chk">' + icon("check", 11, 2.4) + "</span>" +
      (p.starred ? '<span class="badge-star">' + icon("star", 14, 0, "currentColor") + "</span>" : "") +
      (off ? '<span class="badge-off">离线</span>' : "") + fn + "</div>";
  }
  function justifiedHtml(list) {
    const target = clampThumb((S().settings && S().settings.thumbSize) || 144);
    let html = '<div class="justified waterfall-h">';
    list.forEach(function (p) {
      const ar = (p.w && p.h) ? (p.w / p.h) : 1.5;
      const piece = cellHtml(p, { noName: true });
      html += piece.replace('class="cell', 'data-aspect="' + ar + '" style="width:' + Math.round(target * ar) + "px;height:" + target + 'px" class="cell');
    });
    return html + "</div>";
  }
  function layoutJustified() {
    const box = document.querySelector(".justified");
    if (!box) return;
    const gap = 8, target = clampThumb((S().settings && S().settings.thumbSize) || 144), width = box.clientWidth;
    if (width < 40) return;
    const nodes = $all(".cell", box);
    let row = [], rowW = 0;
    function flush(fill) {
      if (!row.length) return;
      const scale = fill ? (width - gap * (row.length - 1)) / rowW : 1;
      const h = Math.max(22, Math.round(target * scale));
      row.forEach(function (el) {
        const ar = parseFloat(el.getAttribute("data-aspect") || "1.5");
        el.style.height = h + "px"; el.style.width = Math.round(h * ar) + "px";
      });
      row = []; rowW = 0;
    }
    nodes.forEach(function (el) {
      const ar = parseFloat(el.getAttribute("data-aspect") || "1.5");
      const w = target * ar;
      if (row.length && rowW + w + gap > width) flush(true);
      row.push(el); rowW += w;
    });
    flush(row.length >= 3);
  }
  function listHtml(list) {
    return '<div class="list">' + list.map(function (p) {
      return '<div class="lrow' + (App.sel.indexOf(p.id) >= 0 ? " sel" : "") + '" data-id="' + p.id + '">' +
        '<img src="' + OPData.photoSrc(p.file) + '" alt="">' +
        '<div class="meta"><b>' + esc(p.title) + "</b><span>" + esc(p.file) + " · " + esc(p.date) + " · " + p.w + " × " + p.h + "</span></div>" +
        (p.starred ? '<span class="badge-star" style="position:static;filter:none">' + icon("star", 14, 0, "currentColor") + "</span>" : "") + "</div>";
    }).join("") + "</div>";
  }
  function gridOf(list) {
    const layout = S().settings.layout || "grid";
    if (layout === "waterfall-h" || layout === "masonry") return justifiedHtml(list);
    if (layout === "waterfall-v") {
      return '<div class="waterfall-v">' + list.map(function (p) { return cellHtml(p, { noName: true }); }).join("") + "</div>";
    }
    if (layout === "rows") return listHtml(list);
    return '<div class="grid">' + list.map(cellHtml).join("") + "</div>";
  }
  function emptyState(kind) {
    if (kind === "search") {
      return '<div class="empty">' + icon("search", 28) + "<h2>没有符合条件的照片</h2><p>当前搜索或筛选没有结果。可以清除条件，或换一个关键词试试。</p>" +
        '<div style="margin-top:10px"><button class="btn primary" data-act="clear-filters">清除全部筛选</button></div></div>';
    }
    if (kind === "offline") {
      return '<div class="empty">' + icon("offline", 28) + "<h2>目录暂时无法访问</h2><p>资料库仍保留这些照片的索引、星标、标签和相册关系。原文件若还在盘上，重新连接后即可继续浏览。本原型不会改写该目录。</p></div>";
    }
    if (kind === "denied") {
      return '<div class="empty">' + icon("shield", 28) + "<h2>没有访问权限</h2><p>这是权限失败的演示状态。真实产品会请你在系统设置中授予文件夹权限。资料库索引仍保留，原文件不会被修改。</p>" +
        '<div style="margin-top:10px"><button class="btn" data-act="clear-demo-flags">退出演示状态</button></div></div>';
    }
    return '<div class="empty">' + icon("photo", 28) + "<h2>这里还没有照片</h2><p>点顶栏的导入按钮，选一个放照片的目录（只读，不改你的文件）。本原型使用演示目录，不会读取你磁盘上的真实文件。</p>" +
      '<div style="margin-top:10px"><button class="btn primary" data-act="import">导入…</button>' +
      '<button class="btn" data-act="reset-demo" style="margin-left:8px">恢复演示数据（演示扩展）</button></div></div>';
  }
  function sourceStatus(id) {
    const s = S().sources.find(function (x) { return x.id === id; });
    if (S().ui.forceDenied) return "denied";
    if (S().ui.forceOffline) return "offline";
    return (s && s.status) || "online";
  }
  function wrapMain(body, n) {
    const r = parseRoute();
    const skip = ["settings", "help", "import"].indexOf(r.kind) >= 0 || isIndexPage(r);
    const batch = skip ? "" : batchBar();
    const inner = (theme() === "dark")
      ? '<div class="card"><div class="inner">' + body + batch + "</div></div>" + statusbar()
      : '<div class="card">' + body + batch + statusbar() + "</div>";
    return inner;
  }
  function renderLibrary() {
    const st = S(), r = parseRoute(), list = visiblePhotos(), tt = routeTitle(r, list.length, list);
    let body = "";
    if (st.ui.forceLoading) {
      body = '<div class="content"><div class="ghead">正在载入…</div><div class="loading-grid">' + new Array(12).fill(0).map(function () { return '<div class="ph"></div>'; }).join("") + "</div></div>";
    } else if (st.ui.forceDenied && r.kind === "source") {
      body = '<div class="content">' + emptyState("denied") + "</div>";
    } else if ((r.kind === "source" && sourceStatus(r.id) !== "online") || (st.ui.forceOffline && r.kind === "source")) {
      const src = st.sources.find(function (s) { return s.id === r.id; }) || {};
      body = '<div class="content"><div class="banner warn">' + icon("warning", 16) +
        "<div>「" + esc(src.name || r.id) + "」当前不可访问（" + esc(src.path || "") + "）。已保留索引，原文件未被改动。</div></div>" +
        (list.length ? groups(list).map(function (g) { return '<div class="ghead">' + esc(g.label) + '<span class="cnt">' + g.items.length + " 张</span></div>" + gridOf(g.items); }).join("") : emptyState("offline")) + "</div>";
    } else if (st.ui.libraryEmpty || (st.photos.length === 0 && !filterActive())) {
      body = '<div class="content">' + emptyState("empty") + "</div>";
    } else if (list.length === 0) {
      body = '<div class="content">' + emptyState("search") + "</div>";
    } else {
      const layoutNow = S().settings.layout || "grid";
      const grouped = layoutNow === "grid" && (S().settings.sortKey || "date") === "date";
      body = '<div class="content">';
      if (filterActive()) {
        body += '<div class="banner">' + icon("filter", 14) + "<div>正在显示筛选结果 · " + list.length + " 张</div>" +
          '<div class="acts"><button class="btn" data-act="clear-filters">清除全部筛选</button></div></div>';
      }
      if (grouped) {
        groups(list).forEach(function (g) { body += '<div class="ghead">' + esc(g.label) + '<span class="cnt">' + g.items.length + " 张</span></div>" + gridOf(g.items); });
      } else {
        if (r.kind === "album") body += '<p class="note" style="padding:4px 4px 10px">这是应用内相册，只保存在资料库中，不会复制或移动原文件。</p>';
        body += gridOf(list);
      }
      body += "</div>";
    }
    return titlebar() + toolbar(tt.t, tt.s) + '<div class="body">' + sidebar() + '<div class="main">' + wrapMain(body) + "</div></div>";
  }
  function starsText(n) {
    n = n || 0;
    let s = "";
    for (let i = 1; i <= 5; i++) s += i <= n ? "★" : "<u>☆</u>";
    return s;
  }
  function folderPath(fid) {
    const f = S().folders.find(function (x) { return x.id === fid; });
    return (f && f.path) || "~/Pictures";
  }
  function tagChips(p) {
    const set = p.tags || [];
    let h = S().tags.map(function (t) {
      return '<button class="chp' + (set.indexOf(t.id) >= 0 ? " on" : "") + '" data-act="toggle-tag" data-tag="' + escAttr(t.id) + '" data-id="' + p.id + '">' + esc(t.name) + "</button>";
    }).join("");
    h += '<button class="chp add" data-act="new-tag">+ 添加</button>';
    return h;
  }
  function infoPanel(p, ex) {
    const kb = OPData.kb(p.bytes), path = folderPath(p.folder);
    const noExif = !p.hasExif || !ex;
    const exNote = noExif
      ? '<div class="note">此演示条目没有 EXIF。产品在读不到拍摄信息时也只显示「无拍摄信息」，不会编造相机或曝光值。</div>'
      : '<div class="note">尺寸与大小取自本地演示文件。日期、相机、镜头与曝光为<strong>界面示例元数据</strong>，不是从该文件读出的真实 EXIF。</div>';
    const histo = '<div class="lab">RGB 直方图</div><div class="histo" title="演示直方图，非根据本图计算的真实通道数据">' +
      '<i class="r" style="height:40%"></i><i class="g" style="height:70%"></i><i class="b" style="height:55%"></i>' +
      '<span class="note" style="display:block;padding:4px 0 0">演示示意图，不是按像素统计的真实直方图。</span></div>';
    const locBtn = '<div class="set-row" style="padding:8px 0"><button class="btn" data-act="show-loc" data-id="' + p.id + '">' + icon("locate", 13) + " 显示位置</button></div>";
    const rateExt = '<div class="lab">评分 <span class="ext-tag">演示扩展</span></div><div class="rate" data-act="rate-bar" data-id="' + p.id + '">' + starsText(p.rating) + "</div>";
    if (theme() === "dark") {
      return '<aside class="info-panel"><div class="pc">' +
        '<div class="pc-head"><h2>照片信息</h2><button class="pc-close" type="button" data-act="toggle-info" title="关闭">' + icon("x", 14, 2) + "</button></div>" +
        '<div class="ii">' +
          '<div class="ph2">' + (p.file ? '<img src="' + OPData.photoSrc(p.file) + '" alt="">' : "") + '<div><div class="t">' + esc(p.title) + "</div><p>" + esc(p.file || "") + "</p></div></div>" +
          (noExif ? "" : ('<div class="stats"><div><b>' + esc(ex.iso) + "</b><span>ISO</span></div><div><b>" + esc(ex.fl) + "</b><span>焦距</span></div>" +
            "<div><b>" + esc(ex.ap) + "</b><span>光圈</span></div><div><b>" + esc(ex.sh) + "</b><span>快门</span></div></div>")) +
          '<div class="kv"><span>文件</span><b>' + esc(p.file || p.title) + "</b></div>" +
          '<div class="kv"><span>尺寸</span><b class="tnum">' + p.w + " × " + p.h + "</b></div>" +
          '<div class="kv"><span>大小</span><b>' + kb + " KB（本地演示文件）</b></div>" +
          '<div class="kv"><span>格式</span><b>' + esc(p.format || "JPEG") + "</b></div>" +
          '<div class="kv"><span>日期</span><b>' + (p.date ? esc(p.date) : "无日期") + "</b></div>" +
          '<div class="kv"><span>相机</span><b>' + (noExif ? "无拍摄信息" : esc(ex.cam) + "（示例）") + "</b></div>" +
          '<div class="kv"><span>镜头</span><b>' + (noExif ? "无拍摄信息" : esc(ex.lens) + "（示例）") + "</b></div>" +
          '<div class="kv"><span>路径</span><b>' + esc(path) + "（示例路径）</b></div>" +
          '<div class="kv"><span>说明</span><b>' + esc(p.caption || "（无）") + "</b></div>" +
          exNote + '<div class="kv"><span>时间来源</span><b>' + (noExif ? "文件修改时间" : "EXIF 拍摄时间（示例）") + '</b></div><div class="kv"><span>色彩空间</span><b>' + (noExif ? "—" : "sRGB（示例）") + '</b></div><div class="kv"><span>元数据</span><b>' + (noExif ? "该文件没有 EXIF 信息" : "读取成功（示例字段，非真实读取）") + "</b></div>" + histo + locBtn + rateExt +
          '<div class="lab" style="padding:8px 14px 6px">标签</div>' +
          '<div class="chips" style="padding:0 14px 12px">' + tagChips(p) + "</div></div>" +
        '<div class="pfoot">' + icon("lock", 13) + "本地演示 · 不改原文件</div></div></aside>";
    }
    return '<aside class="info-panel"><div class="pc">' +
      '<div class="pc-head"><h2>照片信息</h2><button class="pc-close" type="button" data-act="toggle-info" title="关闭">' + icon("x", 14, 2) + "</button></div>" +
      '<div class="ph2">' + (p.file ? '<img src="' + OPData.photoSrc(p.file) + '" alt="">' : "") + '<div><div class="t">' + esc(p.title) + "</div><p>" + esc(p.file || "") + "</p></div></div>" +
      '<div class="lr"><span class="av">' + icon("cal", 15) + "</span><div><b>" + (p.date ? esc(p.date) : "无日期") + "</b><span>日期为界面示例，非真实 EXIF</span></div></div>" +
      '<div class="lr"><span class="av">' + icon("file", 15) + "</span><div><b>" + esc(p.file || p.title) + "</b><span>" + esc(p.format || "JPEG") + " · " + kb + " KB（本地演示文件）</span></div></div>" +
      '<div class="lr"><span class="av">' + icon("image", 15) + "</span><div><b class=\"tnum\">" + p.w + " × " + p.h + "</b><span>本地演示文件像素尺寸</span></div></div>" +
      '<div class="lr"><span class="av">' + icon("folder", 15) + "</span><div><b>" + esc(path) + "</b><span>路径为界面示例 · 只读</span></div></div>" +
      (p.caption ? ('<div class="lr"><span class="av">' + icon("caption", 15) + "</span><div><b>" + esc(p.caption) + "</b><span>说明只写资料库</span></div></div>") : "") +
      (noExif
        ? '<div class="lab">曝光</div><p class="note">无拍摄信息。本条演示条目没有 EXIF，不会编造相机或曝光值。</p>'
        : ('<div class="lab">曝光<em>拍摄信息为界面示例，非真实 EXIF</em></div>' +
           '<div class="ex"><div><b>' + esc(ex.iso) + "</b><span>ISO</span></div><div><b>" + esc(ex.fl) + "</b><span>焦距</span></div><div><b>" + esc(ex.ap) + "</b><span>光圈</span></div><div><b>" + esc(ex.sh) + "</b><span>快门</span></div></div>" +
           '<div class="lr" style="margin-top:8px"><span class="av">' + icon("camera", 15) + "</span><div><b>" + esc(ex.cam) + "</b><span>" + esc(ex.lens) + " · 示例</span></div></div>")) +
      exNote + '<div class="kv"><span>时间来源</span><b>' + (noExif ? "文件修改时间" : "EXIF 拍摄时间（示例）") + '</b></div><div class="kv"><span>色彩空间</span><b>' + (noExif ? "—" : "sRGB（示例）") + '</b></div><div class="kv"><span>元数据</span><b>' + (noExif ? "该文件没有 EXIF 信息" : "读取成功（示例字段，非真实读取）") + "</b></div>" + histo + locBtn + rateExt +
      '<div class="lab">标签</div><div class="chips">' + tagChips(p) + "</div>" +
      '<div class="pfoot">' + icon("lock", 13) + "本地演示 · 不改原文件</div></div></aside>";
  }
  function renderViewer() {
    const r = parseRoute();
    const list = viewerList();
    let idx = list.findIndex(function (p) { return p.id === r.id; });
    if (idx < 0) idx = 0;
    const p = list[idx] || list[0];
    if (!p) return titlebar() + '<div class="body"><div class="main"><div class="card"><div class="content">' + emptyState("empty") + "</div></div></div></div>";
    startViewerLoad(p.id);
    const ex = p.hasExif ? (OPData.SAMPLE_EXIF[p.file.replace(".jpg", "")] || OPData.SAMPLE_EXIF[p.id] || null) : null;
    const infoBtn = '<button class="info-toggle' + (App.infoOpen ? " on" : "") + '" type="button" data-act="toggle-info">' + icon("info", 14) + "照片信息</button>";
    const fitOn = isFitZoom();
    const pct = fitOn ? 0 : App.zoomPct;
    const zoom = '<div class="zoom-seg">' +
      '<b data-act="zoom-fit" class="' + (fitOn ? "on" : "") + '">适合</b>' +
      '<b data-act="zoom-100" class="' + (!fitOn && pct === 100 ? "on" : "") + '">100%</b>' +
      '<button class="ib" data-act="zoom-out" title="缩小">−</button>' +
      '<span class="zoom-readout tnum">' + (fitOn ? "适合" : (pct + "%")) + "</span>" +
      '<button class="ib" data-act="zoom-in" title="放大">+</button></div>';
    const bg = (viewerBg());
    const tools = '<div class="vbtns"><button class="ib" data-act="star-one" data-id="' + p.id + '" title="收藏">' +
      icon("heart", 15, p.starred ? 0 : 1.75, p.starred ? "currentColor" : "none") + "</button>" +
      '<button class="ib" data-act="show-loc" data-id="' + p.id + '" title="显示位置">' + icon("locate", 15) + "</button>" +
      '<button class="ib' + (App.compare ? " on" : "") + '" data-act="toggle-compare" title="并排比较">比较</button>' +
      '<button class="ib" data-act="cycle-bg" title="切换背景（空格）">背景</button>' +
      zoom + '<span class="vfile">' + esc(p.file || p.title || "") + "</span>" + infoBtn + "</div>";
    let strip = "";
    list.forEach(function (it) {
      const src = it.file ? OPData.photoSrc(it.file) : "";
      strip += src
        ? '<img class="' + (it.id === p.id ? "on" : "") + '" src="' + src + '" alt="" data-open="' + it.id + '">'
        : '<span class="ph-cell" data-open="' + it.id + '"></span>';
    });
    const visTags = (p.tags || []).slice();
    const extraN = visTags.length > 3 ? visTags.length - 3 : 0;
    const capTags = visTags.slice(0, 3).map(function (tg) { return "<b>" + esc(tg) + "</b>"; }).join("") +
      (extraN ? '<span class="tags-more">+' + extraN + "</span>" : "");
    const dateStr = (p.date || "").replace(/^(\d+)-(\d+)-(\d+)$/, "$1年$2月$3日");
    let stage = "";
    const loadCopy = App.viewerPhase === "loading" ? "正在载入原图…" : (App.viewerPhase === "decoding" ? "正在解码…" : "");
    const cmpLoad = App.compare && App.comparePhase === "loading";
    if (App.compare) {
      const q = list[idx + 1] || list[idx - 1];
      const right = q && q !== p
        ? (cmpLoad ? '<div class="vload">正在载入比较图…</div>' : ('<img src="' + (q.file ? OPData.photoSrc(q.file) : "") + '" alt=""><div class="lab">对比 · ' + esc(q.title) + "</div>"))
        : '<div class="lab">没有可比较的相邻照片</div>';
      stage = '<div class="compare-stage">' +
        '<div class="half">' + (loadCopy ? ('<div class="vload">' + loadCopy + "</div>") : ('<img src="' + (p.file ? OPData.photoSrc(p.file) : "") + '" alt="">')) + '<div class="lab">当前 · ' + esc(p.title) + "</div></div>" +
        '<div class="half">' + right + "</div></div>";
    } else {
      const imgStyle = fitOn ? "" : (' style="transform:scale(' + (pct / 100) + ');transform-origin:center center"');
      const stageCls = "stage" + (fitOn ? "" : " zoomed");
      stage = '<div class="' + stageCls + '">' + (loadCopy ? ('<div class="vload">' + loadCopy + "</div>") : (p.file ? ('<img src="' + OPData.photoSrc(p.file) + '" alt="' + escAttr(p.title) + '"' + imgStyle + ">") : '<div class="ph-cell">无预览</div>')) + "</div>";
    }
    const capMeta = (ex
      ? ('示例元数据 · ' + esc(ex.cam) + '<span class="tags-inline"><b>' + esc(ex.fl) + "</b><b>" + esc(ex.sh) + "</b><b>" + esc(ex.ap) + "</b><b>ISO " + esc(ex.iso) + "</b></span>")
      : "无拍摄信息（此演示条目没有 EXIF）");
    return titlebar(tools) + '<div class="vcols bg-' + escAttr(bg) + '"><div class="vmain">' +
      stage +
      (idx > 0 ? ('<button class="navhit l" data-act="prev" title="上一张">' + icon("left", 22) + "</button>") : "") +
      (idx < list.length - 1 ? ('<button class="navhit r" data-act="next" title="下一张">' + icon("right", 22) + "</button>") : "") +
      '<div class="caption"><div class="cap-title">' + esc(p.title) + (dateStr ? "<small>" + esc(dateStr) + "</small>" : "") + (p.caption ? "<small> · " + esc(p.caption) + "</small>" : "") + "</div>" +
        '<div class="cap-meta"><div>' + capMeta + "</div>" +
        '<div style="margin-top:2px"><span class="stars" title="收藏（只读）">' + (p.starred ? "收藏" : "") + '</span><span class="tags-inline">' + capTags + "</span></div></div></div>" +
      '<div class="strip">' + strip + "</div>" +
      '<div class="vstat"><span>' + esc(p.file || p.title || "") + "</span><span>" + (p.w && p.h ? (p.w + " × " + p.h) : "—") + "</span><span>第 " + (idx + 1) + " / " + list.length + " 张</span><span>" + (fitOn ? "适合" : (pct + "%")) + "</span>" + (App.infoOpen ? "<span>I 隐藏信息 · Space 切背景 · C 并排比较 · ←/→ 翻页 · Esc 返回</span>" : "") + "</div></div>" + (App.infoOpen ? infoPanel(p, ex) : "") + "</div>";
  }

  function renderSettings() {
    const st = S();
    const n = clampThumb((st.settings && st.settings.thumbSize) || 144);
    let srcRows = st.sources.map(function (s) {
      return '<div class="set-row"><div class="desc"><b>' + esc(s.name) + "</b><span>" + esc(s.path) + " · " + (s.status === "online" ? "在线" : "不可访问") + " · " + esc(s.note || "") +
        "</span></div>" + (s.id !== "pictures" ? '<button class="btn" data-act="remove-source" data-id="' + s.id + '">从资料库移除来源</button>' : "") + "</div>";
    }).join("");
    const body = '<div class="page"><h2>设置</h2>' +
      '<div class="set-sec"><h3>外观</h3><div class="set-row"><div class="desc"><b>主题</b><span>亮色沿用方案 A 清场 + HeroUI Pro，暗色沿用方案 B + beUI Pro。将在各页面间保持一致。</span></div>' +
        '<div class="theme-sw">' +
          '<i data-act="set-theme" data-theme="light" class="' + (theme() === "light" ? "on" : "") + '">' + icon("sun", 13) + "亮色</i>" +
          '<i data-act="set-theme" data-theme="dark" class="' + (theme() === "dark" ? "on" : "") + '">' + icon("moon", 13) + "暗色</i></div></div></div>" +
      '<div class="set-sec"><h3>图库来源</h3>' + srcRows +
        '<div class="set-row"><button class="btn primary" data-act="import">添加文件夹…</button></div>' +
        '<p class="note">添加或移除来源只会改资料库索引，不会在原照片文件夹里写入任何文件。</p></div>' +
      '<div class="set-sec"><h3>索引与缩略图</h3>' +
        '<div class="set-row"><div class="desc"><b>缩略图边长</b><span>连续滑块 22–512 px（step=1）放在底栏最右侧，与 ⌘− / ⌘+ / ⌘0（回到 96）共用同一状态。顶栏已去掉缩略图 −/+。极小尺寸会收紧圆角。不会改动原照片。</span></div>' +
        thumbSliderHtml(true) + "</div>" +
        settingsIndexHtml() +
        '<div class="set-row"><div class="desc"><b>索引管理</b><span>设置的下级页面，含任务、关注文件夹、排除项、存储与诊断、导入 / 导出。从本页或更多菜单「设置…」进入；侧栏与底栏不再单独放置索引管理入口。</span></div>' +
        '<a class="btn primary" href="#/settings/index" data-act="go-index">打开索引管理</a></div></div>' +
      '<div class="set-sec"><h3>演示与数据</h3>' +
        '<div class="set-row"><div class="desc"><b>演示数据保存在 localStorage</b><span>星标、标签、相册与主题仅存于演示资料库。本区入口供审核切换状态，不代表产品设置项。</span></div></div>' +
        '<div class="set-row"><button class="btn" data-act="demo-empty">演示：资料库为空</button>' +
        '<button class="btn" data-act="demo-offline">演示：目录失联</button>' +
        '<button class="btn" data-act="demo-denied">演示：权限失败</button>' +
        '<button class="btn" data-act="demo-loading">演示：加载中</button><button class="btn" data-act="demo-lib-error">演示：库错误红条</button><button class="btn" data-act="demo-resume-pending">演示：未完成任务（刷新后续跑）</button></div>' +
        '<p class="note">审核场景（不干扰主界面结构）：</p>' +
        '<div class="demo-grid">' +
        '<button class="btn" data-act="demo-mgmt" data-v="running">索引进行中</button>' +
        '<button class="btn" data-act="demo-mgmt" data-v="system-pause">系统暂停</button>' +
        '<button class="btn" data-act="demo-mgmt" data-v="system-resume">系统恢复</button>' +
        '<button class="btn" data-act="demo-mgmt" data-v="failures">失败分类</button>' +
        '<button class="btn" data-act="demo-mgmt" data-v="offline">离线根目录</button>' +
        '<button class="btn" data-act="demo-mgmt" data-v="watch-degrade">监视降级</button>' +
        '<button class="btn" data-act="demo-mgmt" data-v="import-cancel">导入中断回滚</button>' +
        '<button class="btn" data-act="demo-mgmt" data-v="export-interrupt">导出中断</button>' +
        '<button class="btn" data-act="demo-mgmt" data-v="verify">启动校验悬停</button>' +
        '</div>' +
        '<div class="set-row"><button class="btn" data-act="clear-demo-flags">退出演示状态</button>' +
        '<button class="btn primary" data-act="reset-demo">恢复演示数据</button></div></div></div>';
    return titlebar() + toolbar("设置", "外观、图库来源与缩略图") + '<div class="body">' + sidebar() + '<div class="main">' + wrapMain(body) + "</div></div>";
  }

  function sourceState(s) {
    const raw = sourceStatus(s.id);
    if (raw === "denied") return { cls: "denied", text: "权限失败" };
    if (raw === "offline" || s.status === "offline") return { cls: "offline", text: "失联" };
    return { cls: "online", text: "在线" };
  }
  function renderIndexManager() {
    const body = window.OPMgmt ? OPMgmt.pageHtml() : '<div class="page"><p>索引管理模块未加载。</p></div>';
    return titlebar() + toolbar("索引管理", "设置 · 演示索引，不写入原文件") + '<div class="body">' + sidebar() + '<div class="main">' + wrapMain(body) + "</div></div>";
  }

  function renderHelp() {
    const groups = [
      { title: "图库", keys: [
        ["⌘ / Ctrl + F", "聚焦搜索（焦点在输入框时，图库/查看器快捷键全部停用）"],
        ["Esc", "关闭弹层；查看器返回上一视图"],
        ["Enter", "打开当前选中照片"],
        ["空格（图库）", "打开当前选中照片"],
        ["⌘ / Ctrl + A", "选中当前视图全部"],
        ["⌘ / Ctrl + D", "取消选择"],
        ["⌘ / Ctrl + I", "反选当前视图"],
        ["⌘ / Ctrl + 点击", "多选切换"],
        ["Shift + 点击", "范围选择"],
        ["从空白拖拽", "矩形框选（⌘ 拖为追加；占位格不计入）"],
        ["⌘ / Ctrl + H", "放入或移出照片托盘"],
        ["⌘ / Ctrl + Enter", "显示位置（演示：不打开系统访达）"],
        ["Delete / Backspace", "移到废纸篓（产品会移动原文件；本原型只模拟）"],
        ["⌘ / Ctrl + − / +", "缩小 / 放大缩略图（与底栏最右侧滑块同一状态，22–512 px）"],
        ["⌘ / Ctrl + 0", "缩略图回到 96 px"],
        ["⌘ / Ctrl + 1 或 ⌘−", "缩略图小一档（与滑块同一状态）"],
        ["⌘ / Ctrl + 2 或 ⌘+", "缩略图大一档（与滑块同一状态）"],
        ["Home / End（图库）", "选中当前视图第一张 / 最后一张"],
        ["F11", "全屏"],
        ["⌘ / Ctrl + ,", "打开设置"],
        ["?", "打开本页"]
      ]},
      { title: "查看器", keys: [
        ["← → 或 J / K", "上一张 / 下一张"],
        ["Home / End", "当前序列第一张 / 最后一张"],
        ["+ / = / ↑ / PageUp，滚轮", "放大（5%–800%）"],
        ["− / ↓，滚轮", "缩小"],
        ["PageDown 或「适合」或 0", "适合窗口"],
        ["1", "缩放到 100%"],
        ["C", "并排比较"],
        ["空格", "切换查看器背景"],
        ["I", "打开或关闭照片信息"],
        ["⌘ / Ctrl + Enter", "显示位置"],
        ["Delete / Backspace", "移到废纸篓确认"]
      ]},
      { title: "演示扩展（不是产品已实现能力）", keys: [
        ["信息栏 1–5 星评分", "仅作演示扩展，产品没有 1–5 评分；查看器数字键 1 用于 100% 缩放"],
        ["幻灯片播放", "顶栏播放为演示扩展；产品该入口为「下一版开放」"],
        ["列表布局、从相册移出、最近导入", "列表不是实装三种布局之一；从相册移出与「最近导入」为扩展入口，不能替代「最近更改」"]
      ]}
    ];
    let keysHtml = "";
    groups.forEach(function (g) {
      keysHtml += '<div class="set-sec"><h3>' + esc(g.title) + '</h3><div class="help-grid">' +
        g.keys.map(function (k) { return "<div><span>" + k[1] + "</span><kbd>" + k[0] + "</kbd></div>"; }).join("") +
        "</div></div>";
    });
    const body = '<div class="page"><h2>帮助与快捷键</h2>' +
      '<p class="note" style="padding:0 4px 14px">这是可供评审的交互原型 v1.3，不是已发布的 OpenPicasa 桌面产品。收藏、说明、标签和相册只写入演示资料库，不会改原照片。导入与索引使用演示目录 / 演示 JSON，不会读取或移动你的真实文件。' +
      "拍摄信息除本地演示文件的尺寸与体积外均为<strong>界面示例</strong>，不是从文件读出的 EXIF。</p>" +
      keysHtml +
      '<div class="set-sec"><h3>覆盖说明</h3><p>必须保留的实装能力（A）在界面中可走完入口、状态与确认。用户已确认的新设计（B）包括设置下级索引管理页、底栏最右侧连续缩略图滑块、展厅查看器与胶片条、亮暗主题。v1.4 按用户最新要求收拢入口：侧栏去掉设置组，文件夹排在标签和相册之下；底栏去掉「索引管理」按钮与角标（运行中进度仍显示在底栏左侧或原浮层）；顶栏去掉缩略图 −/+；更多菜单去掉「添加文件夹…」（改用顶栏导入）。此前相冲突的旧入口布局以本轮为准。标有「演示扩展」的交互（C）可以操作，但不代表产品已经实现。未接通的「编辑」等入口会提示「下一版开放」。危险操作已按范围拆开：移到废纸篓、单根删除索引、全库删除索引、左栏从图库移除（留下可恢复行）、从导入的索引中移除。</p></div></div>';
    return titlebar() + toolbar("帮助与快捷键", "本原型演示说明 · 快捷键按实装行为列出") + '<div class="body">' + sidebar() + '<div class="main">' + wrapMain(body) + "</div></div>";
  }

  function renderImport() {
    const w = App.importW || (App.importW = { step: 1, src: null, running: false, pct: 0, result: null });
    const steps = ["选择来源", "确认", "索引", "完成"];
    let main = '<div class="page"><h2>添加受监控文件夹</h2><div class="wizard">';
    main += '<div class="steps">' + steps.map(function (s, i) {
      const n = i + 1;
      return '<span class="' + (w.step === n ? "on" : w.step > n ? "done" : "") + '">' + n + " · " + s + "</span>";
    }).join("") + "</div>";
    if (w.step === 1) {
      main += '<p class="note">请选择一个演示来源。本向导不会打开系统文件选择器，也不会读取你磁盘上的真实照片。</p><div class="src-list">';
      OPData.IMPORT_PRESETS.forEach(function (s) {
        main += '<div class="src-item' + (w.src === s.id ? " on" : "") + '" data-act="pick-src" data-id="' + s.id + '"><div>' +
          "<b>" + esc(s.name) + "</b><span>" + esc(s.path) + " · 约 " + s.count + " 张 · " + esc(s.note) + "</span></div></div>";
      });
      main += '</div><div class="modal-acts"><button class="btn ghost" data-act="go-all">取消</button>' +
        '<button class="btn primary" data-act="import-next"' + (w.src ? "" : " disabled") + ">下一步</button></div>";
    } else if (w.step === 2) {
      const s = OPData.IMPORT_PRESETS.find(function (x) { return x.id === w.src; }) || {};
      main += "<p>即将把「" + esc(s.name) + "」加入资料库并开始模拟索引。</p>" +
        '<p class="note">照片仍留在原位置。本原型只在演示资料库里登记条目，不会复制、移动或改写原文件。</p>' +
        '<div class="modal-acts"><button class="btn" data-act="import-back">上一步</button><button class="btn primary" data-act="import-run">开始索引</button></div>';
    } else if (w.step === 3) {
      const job = indexSnapshot();
      main += '<p>正在建立索引…</p><div class="pill" style="margin:16px 0" data-index-status="' + job.status + '" data-pct="' + job.pct + '" data-done="' + job.done + '" data-total="' + job.total + '"><div class="bar" style="width:240px;height:8px"><i style="width:' + job.pct + '%"></i></div><span class="tnum">' + job.done + " / " + job.total + "（" + job.pct + "%）</span></div>" +
        '<p class="note">界面保持可返回。这是演示进度，不是真实扫描。底栏左侧与设置中的索引进度同步（不再作为进入索引管理的按钮）。</p>' +
        '<div class="modal-acts"><button class="btn" data-act="import-cancel">取消演示</button></div>';
    } else {
      const res = w.result || { ok: 0, skip: 0, fail: 0, note: "" };
      main += "<p>索引完成。</p><p><b>成功 " + res.ok + "</b> · 跳过 " + res.skip + " · 失败 " + res.fail + "</p>" +
        '<p class="note">' + esc(res.note || "") + "</p>" +
        '<div class="modal-acts"><button class="btn primary" data-act="go-recent">查看最近导入</button></div>';
    }
    main += "</div></div>";
    return titlebar() + toolbar("添加受监控文件夹", "演示向导 · 不读取真实文件") + '<div class="body">' + sidebar() + '<div class="main">' + wrapMain(main) + "</div></div>";
  }
  function renderOverlays() {
    const root = document.getElementById("overlay-root");
    if (!root) return;
    let h = "";
    if (App.modal) {
      const m = App.modal;
      h += '<div class="modal-bg" data-act="backdrop"><div class="modal" role="dialog" aria-modal="true">';
      h += "<h3>" + esc(m.title) + "</h3>";
      if (m.body) h += "<p>" + m.body + "</p>";
      if (m.kind === "prompt") h += '<div class="field"><input id="modal-input" type="text" value="' + escAttr(m.value || "") + '" placeholder="' + escAttr(m.placeholder || "") + '"></div>';
      h += '<div class="modal-acts"><button class="btn ghost" data-act="modal-cancel">' + esc(m.cancel || "取消") + "</button>" +
        '<button class="btn ' + (m.danger ? "danger" : "primary") + '" data-act="modal-ok">' + esc(m.ok || "确定") + "</button></div></div></div>";
    }
    if (App.menu) h += '<div class="menu ctx" style="left:' + App.menu.x + "px;top:" + App.menu.y + 'px">' + App.menu.html + "</div>";
    if (App.toasts.length) h += '<div class="toast-wrap">' + App.toasts.map(function (t) { return '<div class="toast">' + esc(t.msg) + "</div>"; }).join("") + "</div>";
    if (App.slideshow) {
      const list = visiblePhotos().length ? visiblePhotos() : S().photos;
      const p = list[App.slideIdx % Math.max(list.length, 1)];
      if (p && p.file) h += '<div class="slides" data-act="stop-slides"><img src="' + OPData.photoSrc(p.file) + '" alt=""><div class="hint">演示幻灯片（扩展，非产品已实现）· 点击或按 Esc 退出 · ← → 翻页</div></div>';
    }
    if (App.picker) h += pickerHtml();
    if (window.OPMgmt) {
      h += OPMgmt.overlayHtml() || "";
      h += OPMgmt.progressHtml() || "";
    }
    if (App.marquee && App.marquee.box) {
      const b = App.marquee.box;
      h += '<div class="marquee" style="left:' + b.x + "px;top:" + b.y + "px;width:" + b.w + "px;height:" + b.h + 'px"></div>';
    }
    root.innerHTML = h;
    root.style.pointerEvents = (App.modal || App.menu || App.slideshow) ? "auto" : "none";
  }
  function sortMenuHtml() {
    const st = S().settings;
    const key = st.sortKey || (String(st.sort || "date-desc").split("-")[0]) || "date";
    const dir = st.sortDir || ((String(st.sort || "").indexOf("asc") >= 0) ? "asc" : "desc");
    const keys = [["date", "拍摄日期"], ["name", "文件名"], ["changed", "修改日期"], ["size", "文件大小"], ["dim", "图片尺寸"]];
    let h = '<div class="mh">排序</div>';
    keys.forEach(function (it) {
      h += '<div class="mi' + (key === it[0] ? " on" : "") + '" data-act="set-sort-key" data-v="' + it[0] + '">' + it[1] + (key === it[0] ? " ✓" : "") + "</div>";
    });
    h += '<div class="sep"></div>';
    h += '<div class="mi' + (dir === "desc" ? " on" : "") + '" data-act="set-sort-dir" data-v="desc">改为降序' + (dir === "desc" ? " ✓" : "") + "</div>";
    h += '<div class="mi' + (dir === "asc" ? " on" : "") + '" data-act="set-sort-dir" data-v="asc">改为升序' + (dir === "asc" ? " ✓" : "") + "</div>";
    h += '<div class="sep"></div>';
    h += '<div class="mi' + (st.thumbsFirst ? " on" : "") + '" data-act="toggle-thumbs-first" title="没有缩略图的（含云端未下载、生成失败）会集中排在后面">有缩略图的排在前面' + (st.thumbsFirst ? " ✓" : "") + "</div>";
    return h;
  }
  function filterMenuHtml() {
    const f = App.filters;
    const cams = [];
    const lenses = [];
    S().photos.forEach(function (p) {
      if (p.cam && cams.indexOf(p.cam) < 0) cams.push(p.cam);
      if (p.lens && lenses.indexOf(p.lens) < 0) lenses.push(p.lens);
    });
    let h = '<div class="filter-panel"><div class="mh">筛选（可叠加）</div>';
    h += '<div class="fz"><label><input type="checkbox" data-act="f-starred"' + (f.starred ? " checked" : "") + "> 只看已收藏</label></div>";
    h += '<div class="fz"><label><input type="checkbox" data-act="f-nodate"' + (f.noDate ? " checked" : "") + "> 无日期</label></div>";
    h += '<div class="fz"><label><input type="checkbox" data-act="f-nometa"' + (f.noMeta ? " checked" : "") + "> 无拍摄信息</label></div>";
    h += '<div class="fl">类型</div>';
    h += '<div class="mi' + (!f.type ? " on" : "") + '" data-act="f-type" data-v="">全部</div>';
    ["JPEG", "PNG", "GIF", "WebP", "TIFF", "SVG", "HEIC", "AVIF", "RAW"].forEach(function (tp) {
      h += '<div class="mi' + (f.type === tp ? " on" : "") + '" data-act="f-type" data-v="' + tp + '">' + tp + "</div>";
    });
    h += '<div class="fl">尺寸</div>';
    h += '<div class="mi' + (!f.sizeClass ? " on" : "") + '" data-act="f-size" data-v="">全部</div>';
    [["small", "小图 (≤640px)"], ["medium", "中图 (640–1920px)"], ["large", "大图 (>1920px)"]].forEach(function (it) {
      h += '<div class="mi' + (f.sizeClass === it[0] ? " on" : "") + '" data-act="f-size" data-v="' + it[0] + '">' + it[1] + "</div>";
    });
    h += '<div class="fl">标签</div>';
    h += '<div class="fz"><input id="filter-tag-input" type="text" placeholder="标签名" value="' + escAttr(f.tagInput || f.tag || "") + '" style="width:120px"> <button class="btn filter-apply" data-act="f-tag-apply">按此标签筛</button></div>';
    h += '<div class="fl">相机 / 镜头（示例元数据，须点应用）</div>';
    h += '<div class="fz"><input id="filter-cam-input" type="text" placeholder="相机型号" value="' + escAttr(f.cameraInput || f.camera || "") + '" style="width:110px"> ';
    h += '<input id="filter-lens-input" type="text" placeholder="镜头" value="' + escAttr(f.lensInput || f.lens || "") + '" style="width:110px"> <button class="btn filter-apply" data-act="f-cam-apply">应用</button></div>';
    h += '<div class="fl">文件大小 (KB)</div>';
    h += '<div class="fz"><input id="filter-sz-min" type="number" placeholder="最小" value="' + escAttr(f.sizeMinInput != null ? f.sizeMinInput : (f.sizeMin || "")) + '" style="width:72px"> ～ <input id="filter-sz-max" type="number" placeholder="最大" value="' + escAttr(f.sizeMaxInput != null ? f.sizeMaxInput : (f.sizeMax || "")) + '" style="width:72px"> <button class="btn filter-apply" data-act="f-size-apply">应用</button></div>';
    h += '<div class="sep"></div><div class="mi" data-act="clear-filters">清除全部筛选</div></div>';
    return h;
  }
  function moreMenuHtml() {
    const st = S();
    const layout = st.settings.layout || "grid";
    return '<div class="mi' + (st.settings.recurseSubfolders !== false ? " on" : "") + '" data-act="toggle-recurse">包含子文件夹' + (st.settings.recurseSubfolders !== false ? " ✓" : "") + "</div>" +
      '<div class="mi' + (App.showFilenames ? " on" : "") + '" data-act="toggle-filenames">显示文件名' + (App.showFilenames ? " ✓" : "") + "</div>" +
      '<div class="mi' + (layout === "rows" ? " on" : "") + '" data-act="set-layout-ext" data-v="rows">列表布局（演示扩展）' + (layout === "rows" ? " ✓" : "") + "</div>" +
      '<div class="sep"></div>' +
      '<div class="mi" data-act="go-settings">' + icon("settings", 14) + '设置…<span class="sp"></span><kbd>⌘,</kbd></div>' +
      '<div class="mi" data-act="go-index">' + icon("index", 14) + "索引管理…</div>" +
      '<div class="mi" data-act="go-help">' + icon("help", 14) + '帮助与快捷键<span class="sp"></span><kbd>?</kbd></div>' +
      '<div class="sep"></div>' +
      '<div class="mi" data-act="set-theme" data-theme="' + (theme() === "light" ? "dark" : "light") + '">' +
        (theme() === "light" ? icon("moon", 14) + "切换到暗色" : icon("sun", 14) + "切换到亮色") + "</div>" +
      '<div class="mi" data-act="about-app">关于 OpenPicasa</div>' +
      '<div class="sep"></div><div class="mi" data-act="reset-demo">恢复演示数据</div>';
  }

  function albumMenuHtml() {
    let h = '<div class="mh">加入相册（仅资料库）</div>';
    S().albums.forEach(function (a) { h += '<div class="mi" data-act="add-album" data-id="' + a.id + '">' + esc(a.name) + "</div>"; });
    h += '<div class="sep"></div><div class="mi" data-act="new-album">新建相册…</div>';
    return h;
  }
  function tagState(tagId, ids) {
    ids = ids || App.sel.slice();
    if (!ids.length) return "none";
    let n = 0;
    ids.forEach(function (id) {
      const p = OPStore.photo(id);
      if (p && (p.tags || []).indexOf(tagId) >= 0) n++;
    });
    if (n === 0) return "none";
    if (n === ids.length) return "all";
    return "some";
  }
  function tagMenuHtml() {
    const n = App.sel.length;
    let h = '<div class="tag-panel"><div class="mh">' + (n ? ("批量标签 · " + n + " 张照片") : "批量标签") + "</div>";
    if (!n) {
      h += '<div class="hint">请先选择照片</div></div>';
      return h;
    }
    if (!S().tags.length) h += '<div class="hint">（还没有标签，可在下方新建）</div>';
    S().tags.forEach(function (tg) {
      const stt = tagState(tg.id, App.sel);
      const mark = stt === "all" ? "☑" : (stt === "some" ? "▣" : "☐");
      h += '<div class="tri" data-act="toggle-tag-sel" data-tag="' + escAttr(tg.id) + '"><span class="box" aria-hidden="true">' + mark + "</span>" + esc(tg.name) + "</div>";
    });
    h += '<div class="hint">取消勾选 = 从选中集同步移除该标签</div>';
    h += '<div class="newrow"><input id="tag-new-input" type="text" placeholder="新标签名"><button class="btn" data-act="tag-new-add">新建并添加</button></div></div>';
    return h;
  }
  function albumCtxHtml(id) {
    return '<div class="mi" data-act="open-album" data-id="' + id + '">打开相册</div>' +
      '<div class="mi" data-act="rename-album" data-id="' + id + '">重命名…</div>' +
      (App.sel.length ? '<div class="mi" data-act="add-album" data-id="' + id + '">加入所选照片</div>' : "") +
      '<div class="sep"></div><div class="mi danger" data-act="delete-album" data-id="' + id + '">删除相册…</div>';
  }
  function tagCtxHtml(id) {
    return '<div class="mi" data-act="open-tag" data-id="' + escAttr(id) + '">查看照片</div>' +
      '<div class="mi" data-act="rename-tag" data-id="' + escAttr(id) + '">重命名…</div>' +
      '<div class="sep"></div><div class="mi danger" data-act="delete-tag" data-id="' + escAttr(id) + '">删除标签…</div>';
  }
  function render() {
    document.documentElement.setAttribute("data-theme", theme());
    applyThumbVar();
    const root = document.getElementById("app");
    const r = parseRoute();
    let html;
    if (r.kind === "viewer") html = renderViewer();
    else if (r.kind === "settings" && (r.id === "index" || r.id === "indexing")) html = renderIndexManager();
    else if (r.kind === "settings") html = renderSettings();
    else if (r.kind === "help") html = renderHelp();
    else if (r.kind === "import") html = renderImport();
    else html = renderLibrary();
    const bg = viewerBg();
    const err = (S().ui && S().ui.lastError) || App.libError;
    if (err) html += '<div class="lib-error" role="alert"><span>' + esc(err) + '</span><button type="button" data-act="dismiss-error" title="关闭" aria-label="关闭">×</button></div>';
    root.className = "win" + (r.kind === "viewer" ? " viewer bg-" + bg : "") + (r.kind === "viewer" && App.infoOpen ? " info-open" : "") + (r.kind === "viewer" && App.chromeHidden ? " chrome-hidden" : "");
    root.innerHTML = html;
    renderOverlays();
    if (r.kind !== "viewer") {
      const hv = location.hash || "#/all";
      if ((S().ui || {}).lastView !== hv) {
        OPStore.patch(function (s) { s.ui.lastView = hv; }, true);
      }
    }
    if (r.kind === "viewer") armViewerIdle();
    const rNow = parseRoute();
    if (rNow.kind !== "viewer") {
      App.lastVisible = visiblePhotos().map(function (p) { return p.id; });
    }
    const onRow = (rNow.kind === "settings" && document.querySelector('a.row[href="#/settings/index"]')) ||
      document.querySelector(".side a.row.on, .side a.cover.on");
    if (onRow && onRow.scrollIntoView) onRow.scrollIntoView({ block: "nearest" });
    const lay = S().settings.layout;
    if (lay === "masonry" || lay === "waterfall-h") requestAnimationFrame(layoutJustified);
    if (App.creating) { const inp = document.getElementById("create-input"); if (inp && document.activeElement !== inp) inp.focus(); }
  }
  function placeMenu(html, ev, el) {
    const wrap = document.querySelector(".stage-wrap") || document.getElementById("app");
    const box = wrap.getBoundingClientRect();
    el = el || (ev && ev.target);
    let srcX = ev && ev.clientX, srcY = ev && ev.clientY;
    if (el && el.getBoundingClientRect && (!srcX || (ev && ev.detail === 0))) {
      const r = el.getBoundingClientRect();
      srcX = r.left;
      srcY = r.bottom;
    }
    App.menu = { html: html, x: srcX || (box.left + 8), y: srcY || (box.top + 8) };
    renderOverlays();
    const node = document.querySelector("#overlay-root .menu");
    if (node) {
      const maxH = Math.max(160, box.height - 16);
      node.style.maxHeight = maxH + "px";
      node.style.overflowY = "auto";
      const w = node.offsetWidth, h = Math.min(node.scrollHeight, maxH);
      const x = Math.min(Math.max(box.left + 8, App.menu.x), Math.max(box.left + 8, box.right - w - 8));
      const y = (App.menu.y + h > box.bottom - 8) ? Math.max(box.top + 8, box.bottom - h - 8) : Math.max(box.top + 8, App.menu.y);
      node.style.left = x + "px";
      node.style.top = y + "px";
      App.menu.x = x;
      App.menu.y = y;
    }
  }
  function onAct(act, el, ev) {
    const r = parseRoute();
    if (act === "toggle-side") { OPStore.patch(function (st) { st.ui.sidebarOpen = !st.ui.sidebarOpen; }); return; }
    if (act === "import") { startImport(); return; }
    if (act === "slideshow") {
      const list = visiblePhotos();
      if (!list.length) { toast("当前没有可播放的照片。"); return; }
      App.slideshow = true; App.slideIdx = 0; renderOverlays(); return;
    }
    if (act === "stop-slides") { App.slideshow = false; renderOverlays(); return; }
    if (act === "clear-search") { App.q = ""; render(); return; }
    if (act === "clear-filters") { App.q = ""; App.filters = emptyFilters(); render(); return; }
    if (act === "sort-menu") { placeMenu(sortMenuHtml(), ev); return; }
    if (act === "filter-menu") { placeMenu(filterMenuHtml(), ev); return; }
    if (act === "more-menu") { placeMenu(moreMenuHtml(), ev); return; }
    if (act === "album-menu") { placeMenu(albumMenuHtml(), ev); return; }
    if (act === "tag-menu") { placeMenu(tagMenuHtml(), ev); return; }
    if (act === "set-sort") { OPStore.patch(function (st) { st.settings.sort = el.getAttribute("data-v"); }); closeMenu(); return; }
    if (act === "f-starred") { App.filters.starred = !App.filters.starred; render(); return; }
    if (act === "f-tag") { const v = el.getAttribute("data-v"); App.filters.tag = App.filters.tag === v ? "" : v; render(); return; }
    if (act === "f-month") { const v = el.getAttribute("data-v"); App.filters.month = App.filters.month === v ? "" : v; render(); return; }
    if (act === "clear-sel") { clearSel(); return; }
    if (act === "star-sel") { toggleStar(App.sel.slice(), true); toast("已加星标。仅写入资料库。"); return; }
    if (act === "star-one") { toggleStar([el.getAttribute("data-id")]); return; }
    if (act === "tray-sel") { addToTray(); return; }
    if (act === "toggle-tray") { OPStore.patch(function (st) { st.ui.trayOpen = !st.ui.trayOpen; }); return; }
    if (act === "clear-tray") {
      confirmDlg({ title: "清空照片托盘？", body: "只清空托盘里的临时集合，不会从资料库或磁盘移除照片。", ok: "清空托盘", onOk: function () {
        OPStore.patch(function (st) { st.tray = []; }); toast("托盘已清空。");
      }});
      return;
    }
    if (act === "remove-lib") { removeFromLibrary(); return; }
    if (act === "remove-from-album" && r.kind === "album") { removeFromAlbum(r.id); return; }
    if (act === "add-album") { addToAlbum(el.getAttribute("data-id")); closeMenu(); return; }
    if (act === "toggle-tag-sel") { toggleTag(el.getAttribute("data-tag"), App.sel.slice()); closeMenu(); return; }
    if (act === "toggle-tag") { toggleTag(el.getAttribute("data-tag"), [el.getAttribute("data-id")]); return; }
    if (act === "rate-bar") {
      const id = el.getAttribute("data-id");
      const rect = el.getBoundingClientRect();
      const n = Math.max(1, Math.min(5, Math.ceil((ev.clientX - rect.left) / (rect.width / 5))));
      setRating([id], n);
      return;
    }
    if (act === "new-album") {
      closeMenu();
      promptDlg({ title: "新建相册", body: "相册只保存在资料库，不会在照片文件夹里新建目录。", placeholder: "相册名称", onOk: function (val) {
        const err = createAlbum(val); if (err) toast(err);
      }});
      return;
    }
    if (act === "new-tag") {
      closeMenu();
      if (!App.sel.length) { toast("先选中照片再新建标签"); return; }
      promptDlg({ title: "新建标签", body: "标签依附照片：将为当前选中集打上该标签", placeholder: "标签名称", ok: "创建", onOk: function (val) {
        const err = createTag(val, true); if (err) toast(err);
      }});
      return;
    }
    if (act === "rename-album") {
      const a = OPStore.album(el.getAttribute("data-id"));
      closeMenu();
      promptDlg({ title: "重命名相册", value: a && a.name, onOk: function (val) {
        const err = renameAlbum(a.id, val); if (err) toast(err); else toast("已重命名。仅更新资料库。");
      }});
      return;
    }
    if (act === "rename-tag") {
      const id = el.getAttribute("data-id");
      closeMenu();
      promptDlg({ title: "重命名标签", value: id, onOk: function (val) {
        const err = renameTag(id, val); if (err) toast(err); else toast("已重命名。仅更新资料库。");
      }});
      return;
    }
    if (act === "delete-album") { closeMenu(); deleteAlbum(el.getAttribute("data-id")); return; }
    if (act === "delete-tag") { closeMenu(); deleteTag(el.getAttribute("data-id")); return; }
    if (act === "open-album") { closeMenu(); go("#/album/" + el.getAttribute("data-id")); return; }
    if (act === "open-tag") { closeMenu(); go("#/tag/" + encodeURIComponent(el.getAttribute("data-id"))); return; }
    if (act === "back") { go("#/all"); return; }
    if (act === "toggle-info") { App.infoOpen = !App.infoOpen; OPStore.patch(function (s) { s.settings.viewerInfo = App.infoOpen; }, true); render(); return; }
    if (act === "prev" || act === "next") {
      const list = viewerList();
      let idx = list.findIndex(function (p) { return p.id === r.id; });
      idx = act === "next" ? Math.min(list.length - 1, idx + 1) : Math.max(0, idx - 1);
      if (list[idx]) go("#/viewer/" + list[idx].id + (App.infoOpen ? "?info=1" : ""));
      return;
    }
    if (act === "set-theme") { OPStore.setTheme(el.getAttribute("data-theme")); closeMenu(); return; }
    if (act === "go-settings") { closeMenu(); go("#/settings"); return; }
    if (act === "go-index") { closeMenu(); go("#/settings/index"); return; }
    if (act === "go-help") { closeMenu(); go("#/help"); return; }
    if (act === "go-all") { go("#/all"); return; }
    if (act === "go-recent") { go("#/recent"); return; }
    if (act === "reset-demo") {
      closeMenu();
      confirmDlg({ title: "恢复演示数据？", body: "将重置资料库中的星标、标签、相册、托盘和演示导入，并退出空库 / 失联等演示状态。主题外观会保留。", ok: "恢复演示数据", onOk: function () {
        OPStore.reset();
        App.sel = []; App.q = ""; App.filters = emptyFilters(); App.importW = null; App.picker = null; App.compare = false; App.libError = "";
        toast("已恢复演示数据。");
        go("#/all");
      }});
      return;
    }
    if (act === "demo-empty") { OPStore.clearLibraryDemo(); App.sel = []; go("#/all"); return; }
    if (act === "demo-offline") {
      OPStore.patch(function (st) {
        st.ui.forceOffline = true; st.ui.forceDenied = false; st.ui.forceLoading = false;
        const s = st.sources.find(function (x) { return x.id === "sd"; }); if (s) s.status = "offline";
      });
      go("#/source/sd"); return;
    }
    if (act === "demo-denied") {
      OPStore.patch(function (st) { st.ui.forceDenied = true; st.ui.forceOffline = false; st.ui.forceLoading = false; });
      go("#/source/sd"); return;
    }
    if (act === "demo-loading") {
      OPStore.patch(function (st) { st.ui.forceLoading = true; });
      go("#/all");
      setTimeout(function () { OPStore.patch(function (st) { st.ui.forceLoading = false; }); }, 1600);
      return;
    }
    if (act === "clear-demo-flags") {
      OPStore.patch(function (st) {
        st.ui.forceOffline = false; st.ui.forceDenied = false; st.ui.forceLoading = false; st.ui.libraryEmpty = false;
        st.photos = st.photos.filter(function (p) { return !/^ph-/.test(p.id); });
        if (window.OPData && OPData.seed) {
          const mg = OPData.seed().mgmt;
          st.mgmt.failures = mg.failures;
          st.mgmt.failedItems = mg.failedItems;
          st.mgmt.tasks = mg.tasks;
          st.mgmt.roots = mg.roots;
          st.mgmt.exclusions = mg.exclusions;
        }
      });
      toast("已退出演示状态，图库恢复为正常演示数据。");
      return;
    }
    if (act === "thumb") { applyThumbSize(el.getAttribute("data-v"), false); return; }
    if (act === "thumb-range") { return; }
    if (act === "rebuild-index") {
      const cur = S().indexJob;
      if (cur && cur.status === "running") { toast("正在索引。请等待结束，或先暂停再重建。"); return; }
      const total = Math.max(1, S().photos.length);
      const jobId = "job-" + Date.now();
      toast("演示：开始重建索引…");
      OPStore.patch(function (st) {
        st.indexJob = {
          id: jobId, kind: "rebuild", label: "重建索引", status: "running",
          pct: 6, done: 0, total: total, added: 0,
          note: "演示索引进行中，不会扫描磁盘，也不会改写原文件。"
        };
      });
      rememberIndexTick(jobId, "rebuild");
      scheduleIndexTick();
      return;
    }
    if (act === "pause-index") {
      const job = S().indexJob;
      if (!job || job.status !== "running") return;
      OPStore.patch(function (st) {
        if (!st.indexJob) return;
        st.indexJob.status = "paused";
        st.indexJob.note = "演示索引已暂停。可继续，不会扫描磁盘，也不会写入原文件。";
      });
      if (App.importW && App.importW.jobId === job.id) App.importW.running = false;
      toast("已暂停演示索引。");
      return;
    }
    if (act === "resume-index") {
      const job = S().indexJob;
      if (!job || job.status !== "paused") return;
      OPStore.patch(function (st) {
        if (!st.indexJob) return;
        st.indexJob.status = "running";
        st.indexJob.note = "演示索引进行中，不会扫描磁盘，也不会写入原文件。";
      });
      if (App.importW && App.importW.jobId === job.id) App.importW.running = true;
      rememberIndexTick(job.id, job.kind || "rebuild", job.presetId);
      scheduleIndexTick();
      toast("已继续演示索引。");
      return;
    }
    if (act === "remove-source") {
      confirmDlg({ title: "从资料库移除该来源？", body: "只移除资料库中的来源登记，不会删除磁盘上的照片文件夹。", danger: true, ok: "移除来源", onOk: function () {
        const id = el.getAttribute("data-id");
        OPStore.patch(function (st) { st.sources = st.sources.filter(function (s) { return s.id !== id; }); });
        toast("来源已从资料库移除。原文件夹未改动。");
      }});
      return;
    }
    if (act === "pick-src") { App.importW.src = el.getAttribute("data-id"); render(); return; }
    if (act === "import-next") { if (App.importW && App.importW.src) { App.importW.step = 2; render(); } return; }
    if (act === "import-back") { App.importW.step = 1; render(); return; }
    if (act === "import-run") { runImport(); return; }
    if (act === "import-cancel") {
      App.importW.running = false;
      App.importW.step = 2;
      OPStore.patch(function (st) { st.indexJob = OPData.defaultIndexJob(st.photos.length); });
      toast("已取消演示索引，资料库未改动。");
      return;
    }
    if (act === "modal-cancel" || act === "backdrop") { closeModal(); return; }
    if (act === "modal-ok") {
      const m = App.modal;
      if (!m) return;
      if (m.kind === "prompt") {
        const val = (document.getElementById("modal-input") || {}).value || "";
        closeModal(); if (m.onOk) m.onOk(val);
      } else { const fn = m.onOk; closeModal(); if (fn) fn(); }
      return;
    }

    if (act === "noop") { return; }
    if (window.OPMgmt && typeof OPMgmt.onAct === "function") {
      if (OPMgmt.onAct(act, el, ev)) return;
    }
    if (act === "demo-mgmt") {
      if (window.OPMgmt) OPMgmt.demo(el.getAttribute("data-v"));
      return;
    }
    if (act === "trash-sel") { moveToTrash(); return; }
    if (act === "show-loc") { showLocation(el.getAttribute("data-id") ? [el.getAttribute("data-id")] : null); return; }
    if (act === "fav-toggle") {
      const ids = App.sel.length ? App.sel.slice() : [];
      if (!ids.length) { toast("请先选择照片"); return; }
      const allOn = ids.every(function (id) { const p = OPStore.photo(id); return p && p.starred; });
      toggleStar(ids, !allOn);
      toast(allOn ? ("已取消收藏 " + ids.length + " 张") : ("已收藏 " + ids.length + " 张"));
      return;
    }
    if (act === "open-caption") {
      const ids = App.sel.length ? App.sel.slice() : [];
      if (!ids.length) { toast("请先选中照片。"); return; }
      const first = OPStore.photo(ids[0]);
      promptDlg({
        title: ids.length > 1 ? ("为 " + ids.length + " 张编辑说明") : "编辑说明",
        body: "说明只写入资料库，不会改原文件。",
        value: (ids.length === 1 && first && first.caption) || "",
        placeholder: "说明",
        onOk: function (val) {
          OPStore.patch(function (st) {
            st.photos.forEach(function (p) { if (ids.indexOf(p.id) >= 0) p.caption = val; });
          });
          toast("已更新说明。仅写入资料库。");
        }
      });
      return;
    }
    if (act === "tray-toggle") {
      if (!App.sel.length) { toast("请先选中照片。"); return; }
      const st = S();
      const allIn = App.sel.every(function (id) { return st.tray.indexOf(id) >= 0; });
      if (allIn) {
        OPStore.patch(function (s) { s.tray = s.tray.filter(function (id) { return App.sel.indexOf(id) < 0; }); });
        toast("已从托盘移出。");
      } else addToTray();
      return;
    }
    if (act === "tray-remove-one") {
      const id = el.getAttribute("data-id");
      OPStore.patch(function (s) { s.tray = s.tray.filter(function (x) { return x !== id; }); });
      closeMenu();
      return;
    }
    if (act === "edit-nyi") { toast("编辑将在下一版开放。"); return; }
    if (act === "thumb-minus") { applyThumbSize(clampThumb(((S().settings && S().settings.thumbSize) || 144) - 16), false); return; }
    if (act === "thumb-plus") { applyThumbSize(clampThumb(((S().settings && S().settings.thumbSize) || 144) + 16), false); return; }
    if (act === "toggle-scope") {
      OPStore.patch(function (s) { s.ui.searchScope = s.ui.searchScope === "folder" ? "all" : "folder"; });
      return;
    }
    if (act === "toggle-sec") {
      const key = el.getAttribute("data-sec");
      OPStore.patch(function (s) {
        s.settings.collapsed = s.settings.collapsed || {};
        s.settings.collapsed[key] = !s.settings.collapsed[key];
      });
      return;
    }
    if (act === "toggle-recurse") {
      OPStore.patch(function (s) { s.settings.recurseSubfolders = s.settings.recurseSubfolders === false; });
      closeMenu();
      return;
    }
    if (act === "toggle-filenames") { App.showFilenames = !App.showFilenames; closeMenu(); render(); return; }
    if (act === "toggle-thumbs-first") {
      OPStore.patch(function (s) { s.settings.thumbsFirst = !s.settings.thumbsFirst; });
      closeMenu();
      return;
    }
    if (act === "set-layout-ext") {
      OPStore.patch(function (s) { s.settings.layout = el.getAttribute("data-v"); });
      closeMenu();
      toast("已切换到列表布局（演示扩展，不是产品已实装的三种布局之一）。");
      return;
    }
    if (act === "set-sort-key") {
      OPStore.patch(function (s) { s.settings.sortKey = el.getAttribute("data-v"); s.settings.sort = el.getAttribute("data-v") + "-" + (s.settings.sortDir || "desc"); });
      closeMenu();
      return;
    }
    if (act === "set-sort-dir") {
      OPStore.patch(function (s) { s.settings.sortDir = el.getAttribute("data-v"); s.settings.sort = (s.settings.sortKey || "date") + "-" + el.getAttribute("data-v"); });
      closeMenu();
      return;
    }
    if (act === "f-nodate") { App.filters.noDate = !App.filters.noDate; render(); return; }
    if (act === "f-nometa") { App.filters.noMeta = !App.filters.noMeta; render(); return; }
    if (act === "f-type") { const v = el.getAttribute("data-v") || ""; App.filters.type = (!v || App.filters.type === v) ? "" : v; render(); return; }
    if (act === "f-size") { const v = el.getAttribute("data-v") || ""; App.filters.sizeClass = (!v || App.filters.sizeClass === v) ? "" : v; render(); return; }
    if (act === "f-camera") { const v = el.getAttribute("data-v"); App.filters.camera = App.filters.camera === v ? "" : v; render(); return; }
    if (act === "f-lens") { const v = el.getAttribute("data-v"); App.filters.lens = App.filters.lens === v ? "" : v; render(); return; }
    if (act === "filter-year") { const y = el.getAttribute("data-year"); App.filters.year = App.filters.year === y ? "" : y; go("#/all"); return; }
    if (act === "tag-sort-menu") {
      placeMenu('<div class="mh">标签显示</div>' +
        '<div class="mi" data-act="set-tag-sort" data-v="name">按名称</div>' +
        '<div class="mi" data-act="set-tag-sort" data-v="count">按数量</div>' +
        '<div class="sep"></div>' +
        '<div class="mi" data-act="toggle-zero-tags">显示空标签</div>', ev);
      return;
    }
    if (act === "set-tag-sort") { OPStore.patch(function (s) { s.settings.tagsSort = el.getAttribute("data-v"); }); closeMenu(); return; }
    if (act === "toggle-zero-tags") { OPStore.patch(function (s) { s.settings.showZeroTags = !s.settings.showZeroTags; }); closeMenu(); return; }
    if (act === "zoom-fit") { setZoomFit(); render(); return; }
    if (act === "zoom-100") { setZoomPct(100); render(); return; }
    if (act === "zoom-in") { bumpZoom(1); render(); return; }
    if (act === "zoom-out") { bumpZoom(-1); render(); return; }
    if (act === "toggle-compare") { App.compare = !App.compare; render(); return; }
    if (act === "cycle-bg") {
      const order = ["black", "gray", "white", "check"];
      OPStore.patch(function (s) {
        const cur = viewerBg();
        const i = Math.max(0, order.indexOf(cur));
        s.settings.viewerBg = order[(i + 1) % order.length];
        s.settings.viewerBgExplicit = true;
      });
      return;
    }
    if (act === "picker-sel") { if (App.picker) App.picker.sel = el.getAttribute("data-id"); renderOverlays(); return; }
    if (act === "picker-cancel") {
      const kind = App.picker && App.picker.kind;
      App.picker = null;
      renderOverlays();
      if (kind === "folder") {
        const mac = /Mac/i.test(navigator.platform || "") || /Mac OS/i.test(navigator.userAgent || "");
        toast(mac ? "已取消选择文件夹" : "未选择文件夹：已取消，或系统文件夹选择器未能弹出");
      }
      return;
    }
    if (act === "picker-ok") {
      const p = App.picker;
      if (!p || !p.sel) return;
      if (p.kind === "folder") {
        const preset = (OPData.ADD_FOLDER_PRESETS || []).find(function (x) { return x.id === p.sel; });
        App.picker = null;
        renderOverlays();
        applyFolderPreset(preset);
        return;
      }
      if (p.kind === "pack") {
        const pack = (OPData.DEMO_PACKS || []).find(function (x) { return x.id === p.sel; });
        applyPackPick(pack);
        return;
      }
      if (p.kind === "remap") {
        const preset = (OPData.ADD_FOLDER_PRESETS || []).find(function (x) { return x.id === p.sel; });
        applyRemap(preset);
        renderOverlays();
        return;
      }
      return;
    }
    if (act === "folder-refresh" || act === "folder-reindex") {
      closeMenu();
      const f = S().folders.find(function (x) { return x.id === el.getAttribute("data-id"); });
      if (!f) return;
      if (act === "folder-reindex" && window.OPMgmt) {
        OPApp.confirmIndex({
          title: "确认全量重建", ok: "确认",
          body: "范围：此文件夹及其子文件夹。路径：" + f.path + "。将重新生成缩略图；原图不受影响。本原型只模拟。",
          onOk: function () { OPMgmt.startTask({ scope: f.path, kind: "重建", target: 20 }); toast("已开始演示重建：" + f.path); }
        });
      } else if (window.OPMgmt) {
        OPMgmt.startTask({ scope: f.path, kind: "刷新", target: 16 });
        toast("已加入刷新队列");
      }
      return;
    }
    if (act === "folder-close-watch") {
      closeMenu();
      const id = el.getAttribute("data-id");
      const f = S().folders.find(function (x) { return x.id === id; });
      if (!f) return;
      if (!isFolderRoot(f)) { toast("根目录不能这样关闭"); return; }
      OPStore.patch(function (st) {
        const root = (st.mgmt.roots || []).find(function (r) { return r.path === f.path || r.id === f.id || r.sourceId === f.source; });
        if (root) { root.status = "closed"; root.watch = "none"; root.watchReason = "已关闭"; }
      });
      toast("已关闭，重新添加同一目录可秒开");
      go("#/all");
      return;
    }
    if (act === "folder-remove") {
      closeMenu();
      const id = el.getAttribute("data-id");
      const f = S().folders.find(function (x) { return x.id === id; });
      if (!f) return;
      OPStore.patch(function (st) {
        const root = (st.mgmt.roots || []).find(function (r) { return r.path === f.path || r.id === f.id || r.sourceId === f.source; });
        const parked = st.photos.filter(function (p) {
          const pf = st.folders.find(function (x) { return x.id === p.folder; });
          const path = (pf && pf.path) || "";
          return p.source === f.source || path === f.path || (f.path && path.indexOf(f.path + "/") === 0);
        });
        st.ui.parkedPhotos = st.ui.parkedPhotos || {};
        const key = (root && root.id) || f.id;
        st.ui.parkedPhotos[key] = (st.ui.parkedPhotos[key] || []).concat(parked);
        const ids = parked.map(function (p) { return p.id; });
        st.photos = st.photos.filter(function (p) { return ids.indexOf(p.id) < 0; });
        st.tray = st.tray.filter(function (id) { return ids.indexOf(id) < 0; });
        if (root) { root.status = "removed"; root.watch = "none"; root.watchReason = ""; root.photos = 0; }
      });
      toast("已从图库移除「" + f.name + "」的索引（原图未动）。可在索引管理中「重新索引（恢复）」。这不是「移到废纸篓」，也不是全库删除索引。");
      go("#/all");
      return;
    }
    if (act === "folder-exclude") {
      closeMenu();
      const id = el.getAttribute("data-id");
      const f = S().folders.find(function (x) { return x.id === id; });
      if (!f) return;
      if (isFolderRoot(f)) { toast("根目录不能这样关闭"); return; }
      OPStore.patch(function (st) {
        st.mgmt.exclusions = st.mgmt.exclusions || [];
        if (!st.mgmt.exclusions.some(function (e) { return e.path === f.path; })) {
          st.mgmt.exclusions.push({ id: "ex-" + Date.now().toString(36), path: f.path, invalid: false });
        }
      });
      toast("已关闭。可在「排除项」里恢复");
      go("#/all");
      return;
    }
    if (act === "impsrc-hide") {
      closeMenu();
      const id = el.getAttribute("data-id");
      const s = ((S().mgmt && S().mgmt.importSources) || []).find(function (x) { return x.id === id; });
      OPStore.patch(function (st) {
        (st.mgmt.importSources || []).forEach(function (x) { if (x.id === id) x.hidden = true; });
        st.mgmt.hiddenImportSources = (st.mgmt.hiddenImportSources || []).concat([id]);
      });
      toast("已从「导入的索引」分组隐藏（可随时恢复）");
      return;
    }
    if (act === "folder-restore") {
      closeMenu();
      const id = el.getAttribute("data-id");
      const f = S().folders.find(function (x) { return x.id === id; });
      OPStore.patch(function (st) {
        if (st.ui.removedFolders) delete st.ui.removedFolders[id];
        const root = (st.mgmt.roots || []).find(function (r) { return f && (r.path === f.path || r.sourceId === f.source); });
        if (root && root.status === "closed") { root.status = "normal"; root.watch = "live"; root.watchReason = ""; }
      });
      toast("已重新加入图库。");
      return;
    }
    if (act === "impsrc-remove") {
      closeMenu();
      const id = el.getAttribute("data-id");
      const s = ((S().mgmt && S().mgmt.importSources) || []).find(function (x) { return x.id === id; });
      confirmDlg({
        title: "从导入的索引中移除？",
        body: "只从「导入的索引」分组拿掉「" + esc(s && s.name || id) + "」及其带入的条目。不删除、不移动原文件，也不删除本机已有的关注文件夹。本原型只模拟资料库变化。",
        danger: true, ok: "移除该来源",
        onOk: function () {
          OPStore.patch(function (st) {
            st.photos = st.photos.filter(function (p) { return p.source !== id; });
            (st.mgmt.importSources || []).forEach(function (x) { if (x.id === id) x.hidden = true; });
            st.mgmt.hiddenImportSources = (st.mgmt.hiddenImportSources || []).concat([id]);
          });
          toast("已从导入的索引中移除。原文件未改动。");
          go("#/all");
        }
      });
      return;
    }

    if (act === "f-tag-apply") {
      const inp = document.getElementById("filter-tag-input");
      const name = ((inp && inp.value) || "").trim();
      App.filters.tag = name;
      App.filters.tagInput = name;
      closeMenu();
      render();
      return;
    }
    if (act === "f-cam-apply") {
      const cam = ((document.getElementById("filter-cam-input") || {}).value || "").trim();
      const lens = ((document.getElementById("filter-lens-input") || {}).value || "").trim();
      App.filters.camera = cam;
      App.filters.lens = lens;
      App.filters.cameraInput = cam;
      App.filters.lensInput = lens;
      closeMenu();
      render();
      return;
    }
    if (act === "f-size-apply") {
      App.filters.sizeMin = ((document.getElementById("filter-sz-min") || {}).value || "").trim();
      App.filters.sizeMax = ((document.getElementById("filter-sz-max") || {}).value || "").trim();
      closeMenu();
      render();
      return;
    }
    if (act === "tag-new-add") {
      const inp = document.getElementById("tag-new-input");
      const err = createTag((inp && inp.value) || "", true);
      if (err) toast(err);
      else closeMenu();
      return;
    }
    if (act === "dismiss-error") {
      OPStore.patch(function (s) { s.ui.lastError = ""; });
      App.libError = "";
      render();
      return;
    }
    if (act === "demo-lib-error") {
      OPStore.patch(function (s) { s.ui.lastError = "打开资料库失败：演示错误（磁盘未挂载或文件不可读）"; });
      App.libError = "打开资料库失败：演示错误（磁盘未挂载或文件不可读）";
      render();
      return;
    }
    if (act === "demo-resume-pending") {
      if (window.OPMgmt && OPMgmt.startTask) OPMgmt.startTask({ scope: "~/Pictures", kind: "刷新", target: 40 });
      toast("已写入未完成的索引任务。刷新页面将提示自动续跑（暂停/取消的任务保持原状）。");
      return;
    }
    if (act === "about-app") { closeMenu(); toast("OpenPicasa 0.4.2-beta.1 · 本地照片库（只读不改原文件）"); return; }
    if (act === "nyi") { closeMenu(); toast(el.getAttribute("data-why") || "此动作在本原型中未实现。"); return; }
  }

  function bind() {
    document.addEventListener("mousemove", function () {
      if (parseRoute().kind === "viewer") armViewerIdle();
    });
    document.addEventListener("click", function (ev) {
      const t = ev.target;
      const lay = t.closest && t.closest("[data-layout]");
      if (lay && lay.getAttribute("data-layout")) { OPStore.patch(function (st) { st.settings.layout = lay.getAttribute("data-layout"); }); return; }
      const z = t.closest && t.closest("[data-zoom]");
      if (z && z.getAttribute("data-zoom")) { App.zoom = z.getAttribute("data-zoom"); render(); return; }
      const open = t.closest && t.closest("[data-open]");
      if (open) {
        App.viewList = (App.lastVisible && App.lastVisible.length) ? App.lastVisible.slice() : visiblePhotos().map(function (p) { return p.id; });
        go("#/viewer/" + open.getAttribute("data-open") + (App.infoOpen ? "?info=1" : ""));
        return;
      }
      const actEl = t.closest && t.closest("[data-act]");
      if (actEl) {
        const act = actEl.getAttribute("data-act");
        if (act === "thumb-range") return;
        if (act === "backdrop" && ev.target !== actEl) return;
        ev.preventDefault();
        onAct(act, actEl, ev);
        return;
      }
      const cell = t.closest && t.closest("[data-id].cell, [data-id].lrow");
      if (cell && cell.getAttribute("data-id")) {
        if (cell.getAttribute("data-ph") === "1") { ev.stopPropagation(); return; }
        selectOne(cell.getAttribute("data-id"), ev.metaKey || ev.ctrlKey, ev.shiftKey);
        ev.stopPropagation();
        return;
      }
      if (App.menu && !t.closest(".menu")) closeMenu();
      if (!t.closest(".cell, .lrow, a, button, input, .toolbar, .side, .menu, .modal, .batch, .status, .vbtns, .strip, .info-panel, .navhit")) {
        if (App.sel.length && parseRoute().kind !== "viewer") clearSel();
      }
    });
    document.addEventListener("dblclick", function (ev) {
      const cell = ev.target.closest && ev.target.closest("[data-id].cell, [data-id].lrow");
      if (cell) {
        if (cell.getAttribute("data-ph") === "1") return;
        ev.preventDefault();
        App.viewList = (App.lastVisible && App.lastVisible.length) ? App.lastVisible.slice() : visiblePhotos().map(function (p) { return p.id; });
        go("#/viewer/" + cell.getAttribute("data-id") + (App.infoOpen ? "?info=1" : ""));
      }
    });
    document.addEventListener("contextmenu", function (ev) {
      const row = ev.target.closest && ev.target.closest("[data-ctx]");
      if (row) {
        ev.preventDefault();
        const kind = row.getAttribute("data-ctx"), id = row.getAttribute("data-id");
        let html = "";
        if (kind === "album") html = albumCtxHtml(id);
        else if (kind === "tag") html = tagCtxHtml(id);
        else if (kind === "folder") html = folderCtxHtml(id);
        else if (kind === "impsrc") html = importSrcCtxHtml(id);
        else if (kind === "tray") html = trayCtxHtml(id);
        if (html) placeMenu(html, ev);
        return;
      }
      const alb = ev.target.closest && ev.target.closest('a[href^="#/album/"]');
      if (alb) {
        ev.preventDefault();
        const id = decodeURIComponent((alb.getAttribute("href").split("/album/")[1] || "").split("?")[0]);
        placeMenu(albumCtxHtml(id), ev);
      }
    });
    document.addEventListener("input", function (ev) {
      if (ev.target && ev.target.getAttribute && ev.target.getAttribute("data-act") === "thumb-range") {
        applyThumbSize(ev.target.value, true);
        return;
      }
      if (ev.target && ev.target.getAttribute && (ev.target.getAttribute("data-act") === "f-size-min" || ev.target.getAttribute("data-act") === "f-size-max")) {
        const act = ev.target.getAttribute("data-act");
        if (act === "f-size-min") App.filters.sizeMin = ev.target.value;
        else App.filters.sizeMax = ev.target.value;
        clearTimeout(App._fq);
        App._fq = setTimeout(function () { render(); }, 200);
        return;
      }
      if (ev.target && ev.target.id === "search-input") {
        App.q = ev.target.value;
        ev.target.parentElement.classList.toggle("has-val", !!App.q);
        clearTimeout(App._sq);
        App._sq = setTimeout(function () {
          render();
          const el = document.getElementById("search-input");
          if (el) { el.focus(); el.setSelectionRange(el.value.length, el.value.length); }
        }, 60);
      }
    });
    document.addEventListener("keydown", function (ev) {
      const tag = (ev.target && ev.target.tagName) || "";
      const inField = /INPUT|TEXTAREA|SELECT/.test(tag) || (ev.target && ev.target.isContentEditable);
      if (ev.key === "Escape") {
        if (App.slideshow) { App.slideshow = false; renderOverlays(); ev.preventDefault(); return; }
        if (App.picker) { App.picker = null; renderOverlays(); ev.preventDefault(); return; }
        if (App.modal) { closeModal(); ev.preventDefault(); return; }
        if (App.menu) { closeMenu(); ev.preventDefault(); return; }
        if (App.creating) { App.creating = null; App.createErr = ""; render(); ev.preventDefault(); return; }
        if (inField) {
          ev.target.blur();
          ev.preventDefault();
          return;
        }
        if (parseRoute().kind === "viewer") {
          go(App.lastLibHash || "#/all");
          ev.preventDefault();
          return;
        }
        if (App.q || filterActive()) { App.q = ""; App.filters = emptyFilters(); render(); ev.preventDefault(); }
        return;
      }
      if (inField) {
        if (ev.key === "Enter" && ev.target.id === "create-input") {
          ev.preventDefault();
          const val = ev.target.value;
          const err = App.creating === "album" ? createAlbum(val) : createTag(val, true);
          if (err) { App.createErr = err; render(); } else { App.creating = null; App.createErr = ""; render(); }
        }
        if (ev.key === "Enter" && ev.target.id === "modal-input") { ev.preventDefault(); onAct("modal-ok", ev.target, ev); }
        if (ev.key === "Enter" && ev.target.id === "filter-tag-input") { ev.preventDefault(); onAct("f-tag-apply", ev.target, ev); }
        if (ev.key === "Enter" && ev.target.id === "tag-new-input") { ev.preventDefault(); onAct("tag-new-add", ev.target, ev); }
        if (ev.key === "Enter" && (ev.target.id === "filter-cam-input" || ev.target.id === "filter-lens-input")) { ev.preventDefault(); onAct("f-cam-apply", ev.target, ev); }
        if (ev.key === "Enter" && (ev.target.id === "filter-sz-min" || ev.target.id === "filter-sz-max")) { ev.preventDefault(); onAct("f-size-apply", ev.target, ev); }
        return;
      }
      const meta = ev.metaKey || ev.ctrlKey;
      const viewerOn = parseRoute().kind === "viewer";
      if (meta && ev.key.toLowerCase() === "f") { ev.preventDefault(); const el = document.getElementById("search-input"); if (el) el.focus(); return; }
      if (meta && ev.key === ",") { ev.preventDefault(); go("#/settings"); return; }
      if (meta && ev.key.toLowerCase() === "a") { ev.preventDefault(); App.sel = visiblePhotos().map(function (p) { return p.id; }); render(); return; }
      if (meta && ev.key.toLowerCase() === "d") { ev.preventDefault(); clearSel(); return; }
      if (meta && ev.key.toLowerCase() === "i" && !viewerOn) {
        ev.preventDefault();
        const ids = visiblePhotos().map(function (p) { return p.id; });
        const set = {};
        App.sel.forEach(function (id) { set[id] = true; });
        App.sel = ids.filter(function (id) { return !set[id]; });
        render();
        return;
      }
      if (meta && ev.key.toLowerCase() === "h") { ev.preventDefault(); addToTray(); OPStore.patch(function (s) { s.ui.trayOpen = true; }); return; }
      if (meta && ev.key === "Enter") { ev.preventDefault(); showLocation(); return; }
      if (meta && (ev.key === "-" || ev.key === "_" || ev.key === "=" || ev.key === "+" || ev.key === "0" || ev.key === "1" || ev.key === "2")) {
        if (!viewerOn) {
          ev.preventDefault();
          const cur = clampThumb((S().settings && S().settings.thumbSize) || 144);
          if (ev.key === "0") applyThumbSize(OPData.THUMB_RESET || 96, false);
          else if (ev.key === "-" || ev.key === "_" || ev.key === "1") applyThumbSize(cur - 16, false);
          else applyThumbSize(cur + 16, false);
          return;
        }
      }
      if (meta && ev.shiftKey && ev.key.toLowerCase() === "r") {
        ev.preventDefault();
        OPStore.patch(function (s) { s.settings.recurseSubfolders = s.settings.recurseSubfolders === false; });
        toast(S().settings.recurseSubfolders !== false ? "已包含子文件夹" : "只看本文件夹");
        return;
      }
      if (ev.key === "F11" || (viewerOn && ev.key === "Enter")) { ev.preventDefault(); toggleFullscreen(); return; }
      if (ev.key === "?" || (ev.shiftKey && ev.key === "/")) { go("#/help"); ev.preventDefault(); return; }
      if (ev.key === "Delete" || ev.key === "Backspace") {
        if (parseRoute().kind === "viewer" || App.sel.length) { ev.preventDefault(); moveToTrash(); }
        return;
      }
      if (ev.key === "Enter" && !viewerOn) {
        const ids = App.sel.length ? App.sel.slice() : visiblePhotos().filter(function (ph) { return ph.thumbState === "ready"; }).map(function (ph) { return ph.id; });
        if (ids.length) {
          ev.preventDefault();
          App.viewList = visiblePhotos().map(function (ph) { return ph.id; });
          go("#/viewer/" + ids[0] + (App.infoOpen ? "?info=1" : ""));
        }
        return;
      }
      if (ev.key === " " || ev.key === "Spacebar") {
        ev.preventDefault();
        if (viewerOn) { onAct("cycle-bg", document.body, ev); }
        else if (App.sel.length) {
          App.viewList = visiblePhotos().map(function (p) { return p.id; });
          go("#/viewer/" + App.sel[0] + (App.infoOpen ? "?info=1" : ""));
        }
        return;
      }
      if (viewerOn && (ev.key === "i" || ev.key === "I") && !meta) {
        App.infoOpen = !App.infoOpen;
        OPStore.patch(function (s) { s.settings.viewerInfo = App.infoOpen; }, true);
        render(); ev.preventDefault(); return;
      }
      if (viewerOn && (ev.key === "c" || ev.key === "C") && !meta) {
        ev.preventDefault(); App.compare = !App.compare; render(); return;
      }
      if (viewerOn && (ev.key === "0")) { ev.preventDefault(); setZoomFit(); render(); return; }
      if (viewerOn && ev.key === "1" && !meta) { ev.preventDefault(); setZoomPct(100); render(); return; }
      if (viewerOn && ev.key === "PageDown") { ev.preventDefault(); setZoomFit(); render(); return; }
      if (viewerOn && (ev.key === "+" || ev.key === "=" || ev.key === "ArrowUp" || ev.key === "PageUp")) { ev.preventDefault(); bumpZoom(1); render(); return; }
      if (viewerOn && (ev.key === "-" || ev.key === "_" || ev.key === "ArrowDown")) { ev.preventDefault(); bumpZoom(-1); render(); return; }
      if ((ev.key === "Home" || ev.key === "End") && !viewerOn && !App.slideshow) {
        ev.preventDefault();
        const ids = visiblePhotos().filter(function (ph) { return ph.thumbState !== "pending" && ph.thumbState !== "failed" && ph.thumbState !== "cloud"; }).map(function (ph) { return ph.id; });
        if (!ids.length) return;
        App.sel = [ev.key === "Home" ? ids[0] : ids[ids.length - 1]];
        App.lastId = App.sel[0];
        render();
        return;
      }
      if (ev.key === "ArrowLeft" || ev.key === "ArrowRight" || ev.key === "j" || ev.key === "J" || ev.key === "k" || ev.key === "K" || ev.key === "Home" || ev.key === "End") {
        if (App.slideshow) {
          ev.preventDefault();
          const list = visiblePhotos().length ? visiblePhotos() : S().photos;
          const dir = (ev.key === "ArrowRight" || ev.key === "j" || ev.key === "J" || ev.key === "End") ? 1 : -1;
          App.slideIdx = (App.slideIdx + (dir === 1 ? 1 : list.length - 1)) % list.length;
          if (ev.key === "Home") App.slideIdx = 0;
          if (ev.key === "End") App.slideIdx = list.length - 1;
          renderOverlays(); return;
        }
        if (viewerOn) {
          ev.preventDefault();
          if (ev.key === "Home" || ev.key === "End") {
            const list = viewerList();
            const p = ev.key === "Home" ? list[0] : list[list.length - 1];
            if (p) go("#/viewer/" + p.id + (App.infoOpen ? "?info=1" : ""));
            return;
          }
          const next = (ev.key === "ArrowRight" || ev.key === "j" || ev.key === "J");
          onAct(next ? "next" : "prev", document.body, ev);
        }
        return;
      }
      // 评分 2–5 仅作为演示扩展，且不在查看器与数字键 1（100%）冲突
      if (!viewerOn && "2345".indexOf(ev.key) >= 0) {
        const ids = App.sel.slice();
        if (ids.length) setRating(ids, parseInt(ev.key, 10));
        return;
      }
    });
    document.addEventListener("wheel", function (ev) {
      if (parseRoute().kind !== "viewer") return;
      if (ev.target && ev.target.closest && ev.target.closest(".info-panel, .strip, .toolbar, .menu, .modal")) return;
      ev.preventDefault();
      bumpZoom(ev.deltaY < 0 ? 1 : -1);
      render();
    }, { passive: false });
    document.addEventListener("mousedown", function (ev) {
      if (ev.button !== 0) return;
      if (ev.target.closest && ev.target.closest("[data-act=side-resize]")) {
        ev.preventDefault();
        const startX = ev.clientX, startW = S().settings.sidebarWidth || 228;
        function move(e) {
          const w = Math.max(180, Math.min(360, startW + (e.clientX - startX)));
          document.documentElement.style.setProperty("--side-w", w + "px");
          const side = document.querySelector(".side");
          if (side) side.style.width = w + "px";
          App._sideW = w;
        }
        function up() {
          document.removeEventListener("mousemove", move);
          document.removeEventListener("mouseup", up);
          if (App._sideW) OPStore.patch(function (s) { s.settings.sidebarWidth = App._sideW; });
        }
        document.addEventListener("mousemove", move);
        document.addEventListener("mouseup", up);
        return;
      }
      const grid = ev.target.closest && ev.target.closest(".grid, .waterfall-v, .justified, .wf-h");
      if (!grid) return;
      if (parseRoute().kind === "viewer") return;
      if (ev.target.closest && ev.target.closest(".cell, .lrow, a, button, input, .toolbar, .side, .menu, .modal")) return;
      ev.preventDefault();
      const origin = { x: ev.clientX, y: ev.clientY };
      const additive = ev.metaKey || ev.ctrlKey;
      const prev = additive ? App.sel.slice() : [];
      App.marquee = { ox: origin.x, oy: origin.y, box: { x: origin.x, y: origin.y, w: 0, h: 0 } };
      renderOverlays();
      const scroller = document.querySelector(".main, .content, .card");
      function frame(e) {
        const x1 = Math.min(origin.x, e.clientX), y1 = Math.min(origin.y, e.clientY);
        const x2 = Math.max(origin.x, e.clientX), y2 = Math.max(origin.y, e.clientY);
        App.marquee.box = { x: x1, y: y1, w: x2 - x1, h: y2 - y1 };
        if (scroller) {
          const r = scroller.getBoundingClientRect();
          if (e.clientY > r.bottom - 28) scroller.scrollTop += 18;
          else if (e.clientY < r.top + 28) scroller.scrollTop -= 18;
        }
        const box = App.marquee.box;
        const hit = [];
        Array.prototype.forEach.call(document.querySelectorAll(".cell[data-id], .lrow[data-id]"), function (cell) {
          if (cell.getAttribute("data-ph") === "1") return;
          const cr = cell.getBoundingClientRect();
          const overlap = !(cr.right < box.x || cr.left > box.x + box.w || cr.bottom < box.y || cr.top > box.y + box.h);
          if (overlap) hit.push(cell.getAttribute("data-id"));
        });
        const map = {};
        prev.concat(hit).forEach(function (id) { map[id] = true; });
        App.sel = Object.keys(map);
        const cells = document.querySelectorAll(".cell[data-id], .lrow[data-id]");
        Array.prototype.forEach.call(cells, function (cell) {
          cell.classList.toggle("sel", App.sel.indexOf(cell.getAttribute("data-id")) >= 0);
        });
        renderOverlays();
      }
      function up() {
        document.removeEventListener("mousemove", frame);
        document.removeEventListener("mouseup", up);
        App.marquee = null;
        render();
      }
      document.addEventListener("mousemove", frame);
      document.addEventListener("mouseup", up);
    });
    window.addEventListener("hashchange", render);
    window.addEventListener("resize", function () { const lay = S().settings.layout; if (lay === "masonry" || lay === "waterfall-h") layoutJustified(); });
    OPStore.on(function () { render(); });
  }

  g.OPApp = {
    render: render, go: go,
    getSel: function () { return App.sel.slice(); },
    setSel: function (ids) { App.sel = ids.slice(); render(); },
    setQ: function (q) { App.q = q; render(); },
    getQ: function () { return App.q; },
    filters: function () { return App.filters; },
    infoOpen: function () { return App.infoOpen; },
    setInfo: function (v) { App.infoOpen = !!v; render(); },
    visible: visiblePhotos,
    toast: toast,
    createAlbum: createAlbum,
    addToAlbum: addToAlbum,
    startImport: startImport,
    theme: theme,
    setThumb: applyThumbSize,
    thumbSize: function () { return clampThumb((S().settings && S().settings.thumbSize) || 144); },
    indexJob: function () { return indexSnapshot(); },
    confirmIndex: confirmIndex,
    confirmDlg: confirmDlg,
    openFolderPicker: openFolderPicker,
    openDemoPackPicker: openDemoPackPicker,
    openRemapPicker: openRemapPicker,
    moveToTrash: moveToTrash,
    showLocation: showLocation,
    emptyFilters: emptyFilters
  };
  document.addEventListener("DOMContentLoaded", function () {
    bind();
    try {
      const last = (S().ui && S().ui.lastView) || "";
      if ((!location.hash || location.hash === "#") && last && last !== "#/all" && last.indexOf("#/viewer") !== 0) {
        location.hash = last;
      }
    } catch (e) {}
    const running = (((S().mgmt || {}).tasks || {}).running || []).filter(function (task) { return !task.userPaused && task.state !== "已完成"; });
    if (running.length) {
      toast("检测到 " + running.length + " 个上次未完成的索引任务，已自动续跑（暂停/取消的任务保持原状）");
      if (window.OPMgmt && OPMgmt.scheduleTick) OPMgmt.scheduleTick();
    }
    App.libError = (S().ui && S().ui.lastError) || "";
    if (S().settings && S().settings.viewerInfo) App.infoOpen = true;
    render();
  });
})(window);
