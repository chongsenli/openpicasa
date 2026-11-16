/* localStorage-backed prototype store. All organization data stays in the mock library; never writes photo folders. */
(function (g) {
  const KEY = 'openpicasa.prototype.v1';

  function clone(v) { return JSON.parse(JSON.stringify(v)); }

  function clampThumb(n) {
    n = parseInt(n, 10);
    if (isNaN(n)) n = OPData.THUMB_DEFAULT;
    return Math.max(OPData.THUMB_MIN, Math.min(OPData.THUMB_MAX, n));
  }

  function migrateLayout(layout) {
    if (layout === "masonry") return "waterfall-h";
    if (layout === "justified") return "waterfall-h";
    if (layout === "rows" || layout === "list") return "rows";
    if (layout === "waterfall-v" || layout === "waterfall-h" || layout === "grid") return layout;
    return "grid";
  }

  function parseSort(settings) {
    if (settings.sortKey) {
      settings.sortDir = settings.sortDir === "asc" ? "asc" : "desc";
      settings.sort = settings.sortKey + "-" + settings.sortDir;
      return;
    }
    const s = settings.sort || "date-desc";
    if (s === "date-asc") { settings.sortKey = "date"; settings.sortDir = "asc"; }
    else if (s === "name") { settings.sortKey = "name"; settings.sortDir = "asc"; }
    else if (s === "size") { settings.sortKey = "size"; settings.sortDir = "desc"; }
    else if (s === "rating") { settings.sortKey = "rating"; settings.sortDir = "desc"; }
    else if (s === "mtime" || s === "mtime-desc") { settings.sortKey = "mtime"; settings.sortDir = "desc"; }
    else if (s === "mtime-asc") { settings.sortKey = "mtime"; settings.sortDir = "asc"; }
    else if (s === "dim" || s === "dim-desc") { settings.sortKey = "dim"; settings.sortDir = "desc"; }
    else { settings.sortKey = "date"; settings.sortDir = "desc"; }
    settings.sort = settings.sortKey + "-" + settings.sortDir;
  }

  function normalize(state) {
    if (!state.settings) state.settings = {};
    state.settings.thumbSize = clampThumb(state.settings.thumbSize);
    state.settings.layout = migrateLayout(state.settings.layout);
    parseSort(state.settings);
    if (state.settings.showFilenames == null) state.settings.showFilenames = true;
    if (state.settings.recurseSubfolders == null) state.settings.recurseSubfolders = true;
    if (state.settings.thumbsFirst == null) state.settings.thumbsFirst = false;
    if (!state.settings.tagsSort) state.settings.tagsSort = "name";
    if (state.settings.showZeroTags == null) state.settings.showZeroTags = false;
    if (!state.settings.sidebarWidth) state.settings.sidebarWidth = 228;
    if (!state.settings.collapsed) state.settings.collapsed = {};
    if (!state.settings.viewerBg) state.settings.viewerBg = "auto";
    if (!state.ui) state.ui = {};
    if (!state.ui.searchScope) state.ui.searchScope = "all";
    if (state.ui.importBatchIds && !Array.isArray(state.ui.importBatchIds)) state.ui.importBatchIds = null;
    if (!state.mgmt || state.mgmt.schema !== 13) {
      state.mgmt = OPData.seedMgmt();
    }
    if (!state.indexJob || !state.indexJob.status) {
      state.indexJob = OPData.defaultIndexJob();
    }
    // drop leftover fake totals from v1.2 jobs so idle does not show 20/20
    if (state.indexJob && state.indexJob.status === "idle") {
      state.indexJob.total = 0;
      state.indexJob.pct = 0;
      if (state.indexJob.done && !state.mgmt.tasks.running.length) {
        /* keep done as 0 when idle */
        state.indexJob.done = 0;
      }
    }
    if (Array.isArray(state.photos)) {
      const have = {};
      state.photos.forEach(function (p) { have[p.id] = true; });
      // import demo photos only when the import source still exists in mgmt
      const srcs = ((state.mgmt && state.mgmt.importSources) || []).map(function (s) { return s.id; });
      (OPData.IMPORT_PHOTOS || []).forEach(function (p) {
        if (!have[p.id] && srcs.indexOf(p.source) >= 0) { state.photos.push(JSON.parse(JSON.stringify(p))); have[p.id] = true; }
      });
      state.photos.forEach(function (p) {
        if (!p.thumbState) p.thumbState = "ready";
        if (p.hasExif == null) p.hasExif = !!(OPData.SAMPLE_EXIF[p.id] || (p.file && OPData.SAMPLE_EXIF[p.file.replace(".jpg", "")]));
        if (!p.changedAt) p.changedAt = (p.date || "2024-01-01") + "T" + (p.time || "12:00") + ":00";
        if (!p.format) p.format = "JPEG";
        if (p.caption == null) p.caption = "";
      });
    }

    if (state.fixtureRev !== 2 && window.OPData && OPData.seed) {
      const seed = OPData.seed();
      state.photos = seed.photos;
      if (!state.mgmt) state.mgmt = seed.mgmt;
      else {
        state.mgmt.failures = seed.mgmt.failures;
        state.mgmt.failedItems = seed.mgmt.failedItems;
        state.mgmt.tasks = seed.mgmt.tasks;
        state.mgmt.roots = seed.mgmt.roots;
        state.mgmt.exclusions = seed.mgmt.exclusions;
      }
      if (!state.settings.viewerBgExplicit && (state.settings.viewerBg === "dark" || state.settings.viewerBg === "black" || !state.settings.viewerBg)) {
        state.settings.viewerBg = "auto";
      }
      state.fixtureRev = 2;
    }
    if (!state.settings.viewerBg) state.settings.viewerBg = "auto";
    return state;
  }

  function load() {
    try {
      const raw = localStorage.getItem(KEY);
      if (!raw) return normalize(OPData.seed());
      const data = JSON.parse(raw);
      if (!data || data.version !== 1 || !Array.isArray(data.photos)) return normalize(OPData.seed());
      const seed = OPData.seed();
      const merged = Object.assign(seed, data, {
        settings: Object.assign(seed.settings, data.settings || {}),
        ui: Object.assign(seed.ui, data.ui || {}),
        mgmt: (data.mgmt && data.mgmt.schema === 13) ? data.mgmt : seed.mgmt
      });
      // if coming from pre-1.3 library without placeholders/import photos, keep user photos as-is
      return normalize(merged);
    } catch (e) {
      return normalize(OPData.seed());
    }
  }

  let state = load();
  const listeners = [];

  function persist() {
    try { localStorage.setItem(KEY, JSON.stringify(state)); }
    catch (e) { /* private mode / quota — prototype still works in-memory */ }
  }

  function emit() {
    persist();
    listeners.forEach(function (fn) { try { fn(state); } catch (e) {} });
  }

  const Store = {
    get: function () { return state; },
    on: function (fn) { listeners.push(fn); return function () { const i = listeners.indexOf(fn); if (i >= 0) listeners.splice(i, 1); }; },
    patch: function (mutator, silent) {
      mutator(state);
      if (silent) persist();
      else emit();
    },
    setThumbSize: function (n, silent) {
      state.settings.thumbSize = clampThumb(n);
      persist();
      if (!silent) emit();
      return state.settings.thumbSize;
    },
    setTheme: function (theme) {
      state.theme = theme === "dark" ? "dark" : "light";
      emit();
    },
    reset: function () {
      const theme = state.theme;
      const thumb = state.settings && state.settings.thumbSize;
      state = normalize(OPData.seed());
      state.theme = theme;
      if (thumb) state.settings.thumbSize = clampThumb(thumb);
      emit();
      return state;
    },
    clearLibraryDemo: function () {
      state.photos = [];
      state.tray = [];
      state.ui.libraryEmpty = true;
      state.indexJob = OPData.defaultIndexJob();
      if (state.mgmt) {
        state.mgmt.roots.forEach(function (r) { r.photos = 0; });
        state.mgmt.storage.photos = 0;
      }
      emit();
    },
    photo: function (id) { return state.photos.find(function (p) { return p.id === id; }); },
    album: function (id) { return state.albums.find(function (a) { return a.id === id; }); },
    tag: function (id) { return state.tags.find(function (t) { return t.id === id; }); },
    photosInAlbum: function (id) {
      return state.photos.filter(function (p) { return (p.albums || []).indexOf(id) >= 0; });
    },
    photosWithTag: function (id) {
      return state.photos.filter(function (p) { return (p.tags || []).indexOf(id) >= 0; });
    },
    folderPhotos: function (fid, recurse) {
      if (recurse == null) recurse = !!(state.settings && state.settings.recurseSubfolders);
      if (fid === "pictures" || fid === "2024" || recurse) {
        if (fid === "pictures" || fid === "2024" || (recurse && hasChildren(fid))) {
          const ids = descendants(fid);
          return state.photos.filter(function (p) {
            if ((p.source && p.source.indexOf("imp") === 0)) return false;
            return ids.indexOf(p.folder) >= 0 || p.folder === fid;
          });
        }
      }
      return state.photos.filter(function (p) { return p.folder === fid; });
    },
    folderLayerCount: function (fid) {
      // 本层张数：只计直接落在该文件夹 id 上的照片（产品口径）
      return state.photos.filter(function (p) { return p.folder === fid; }).length;
    },
    countFolder: function (fid) { return Store.folderLayerCount(fid); },
    persist: persist,
    clampThumb: clampThumb
  };

  function hasChildren(fid) {
    return state.folders.some(function (f) { return f.parent === fid; });
  }

  function descendants(fid) {
    const out = [fid];
    state.folders.forEach(function (f) {
      if (f.parent === fid) out.push.apply(out, descendants(f.id));
    });
    return out;
  }

  g.OPStore = Store;
})(window);
