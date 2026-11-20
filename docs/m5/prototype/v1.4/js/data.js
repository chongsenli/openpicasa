/* Demo catalog for the offline prototype. EXIF/camera/date/path are SAMPLE data unless noted as local file size. */
(function (g) {
  function kb(n) { return Math.round(n / 1024); }

  const PHOTOS = [
    { id:'01-mtn', file:'01-mtn.jpg', title:'云海上的山脊', w:1400, h:933, bytes:191367, date:'2024-05-21', time:'06:18', folder:'2024-05', starred:true, rating:4, tags:['旅行'], albums:[], recent:false, offline:false },
    { id:'19-fog', file:'19-fog.jpg', title:'雪峰下的河谷', w:1400, h:933, bytes:346120, date:'2024-05-21', time:'14:32', folder:'2024-05', starred:true, rating:4, tags:['旅行','2024夏'], albums:[], recent:true, offline:false },
    { id:'05-forest', file:'05-forest.jpg', title:'林间光斑', w:1400, h:932, bytes:429952, date:'2024-05-21', time:'10:05', folder:'2024-05', starred:false, rating:0, tags:['2024夏'], albums:[], recent:false, offline:false },
    { id:'06-desert', file:'06-desert.jpg', title:'岩壁与沙丘', w:1400, h:927, bytes:256827, date:'2024-05-21', time:'16:40', folder:'2024-05', starred:false, rating:3, tags:['旅行'], albums:[], recent:false, offline:false },
    { id:'07-city', file:'07-city.jpg', title:'暮色中的塔楼', w:1400, h:850, bytes:236680, date:'2024-05-21', time:'19:12', folder:'2024-05', starred:true, rating:5, tags:['旅行'], albums:[], recent:true, offline:false },
    { id:'08-flower', file:'08-flower.jpg', title:'庭院黄花', w:1400, h:934, bytes:196011, date:'2024-05-21', time:'09:22', folder:'2024-05', starred:true, rating:4, tags:['家人','2024夏'], albums:['still'], recent:true, offline:false },
    { id:'03-ocean', file:'03-ocean.jpg', title:'俯瞰海岸线', w:1400, h:2492, bytes:635286, date:'2024-05-21', time:'11:03', folder:'2024-05', starred:false, rating:0, tags:['旅行','2024夏'], albums:['coast'], recent:false, offline:false },
    { id:'04-beach', file:'04-beach.jpg', title:'退潮后的沙滩', w:1400, h:931, bytes:183981, date:'2024-05-21', time:'07:48', folder:'2024-05', starred:false, rating:3, tags:['旅行','2024夏'], albums:['coast'], recent:true, offline:false },
    { id:'21-spring', file:'21-spring.jpg', title:'春日庭院', w:1400, h:927, bytes:145738, date:'2024-05-21', time:'08:11', folder:'2024-05', starred:false, rating:0, tags:['家人'], albums:['kyoto'], recent:true, offline:false },
    { id:'02-lake', file:'02-lake.jpg', title:'栈道与湖面', w:1400, h:1051, bytes:215523, date:'2024-05-21', time:'15:27', folder:'2024-05', starred:false, rating:0, tags:[], albums:['coast'], recent:false, offline:false },
    { id:'16-interior', file:'16-interior.jpg', title:'窗边的房间', w:1400, h:2100, bytes:326582, date:'2024-04-18', time:'13:05', folder:'2024-04', starred:false, rating:0, tags:['家人'], albums:['kyoto'], recent:false, offline:false },
    { id:'09-arch', file:'09-arch.jpg', title:'石拱与天空', w:1400, h:933, bytes:293777, date:'2024-04-18', time:'12:16', folder:'2024-04', starred:false, rating:2, tags:['旅行'], albums:[], recent:false, offline:false },
    { id:'22-valley', file:'22-valley.jpg', title:'远处的雪山', w:1400, h:930, bytes:179635, date:'2024-04-18', time:'08:44', folder:'2024-04', starred:false, rating:0, tags:['旅行'], albums:[], recent:false, offline:false },
    { id:'11-food', file:'11-food.jpg', title:'一桌家常菜', w:1400, h:933, bytes:410050, date:'2024-04-18', time:'18:30', folder:'2024-04', starred:false, rating:0, tags:['家人'], albums:['kyoto','still'], recent:false, offline:false },
    { id:'17-street', file:'17-street.jpg', title:'雨后的街巷', w:1400, h:861, bytes:407685, date:'2024-04-18', time:'17:02', folder:'2024-04', starred:false, rating:3, tags:['旅行'], albums:['kyoto'], recent:false, offline:false },
    { id:'14-canyon', file:'14-canyon.jpg', title:'河谷弯道', w:1400, h:933, bytes:414929, date:'2024-04-18', time:'16:08', folder:'2024-04', starred:true, rating:5, tags:['旅行'], albums:[], recent:false, offline:false },
    { id:'13-meadow', file:'13-meadow.jpg', title:'向晚的草地', w:1400, h:788, bytes:209387, date:'2024-04-18', time:'18:55', folder:'2024-04', starred:false, rating:0, tags:['2024夏'], albums:[], recent:false, offline:false },
    { id:'18-rocks', file:'18-rocks.jpg', title:'海边礁石', w:1400, h:933, bytes:228199, date:'2024-04-18', time:'09:41', folder:'2024-04', starred:false, rating:0, tags:[], albums:['coast'], recent:false, offline:false },
    { id:'20-night', file:'20-night.jpg', title:'夜空与城市', w:1400, h:890, bytes:216590, date:'2024-04-18', time:'21:14', folder:'2024-04', starred:false, rating:0, tags:[], albums:[], recent:false, offline:false, source:'sd' },
    { id:'24-path', file:'24-path.jpg', title:'林中小路', w:1400, h:834, bytes:233800, date:'2024-04-18', time:'07:33', folder:'2024-04', starred:false, rating:0, tags:['旅行'], albums:[], recent:false, offline:false }
  ];

  // Sample shooting info used only in the inspector, clearly labeled 示例
  const SAMPLE_EXIF = {
    '19-fog': { cam:'Sony α7R IV', lens:'FE 35mm F1.4', iso:'100', fl:'35mm', ap:'f/8', sh:'1/250' },
    '01-mtn': { cam:'Sony α7R IV', lens:'FE 24-70mm F2.8 GM', iso:'200', fl:'35mm', ap:'f/8', sh:'1/400' },
    '07-city': { cam:'Fujifilm X-T5', lens:'XF 33mm F1.4', iso:'800', fl:'33mm', ap:'f/2', sh:'1/80' },
    '03-ocean': { cam:'DJI Mini 3', lens:'24mm equiv.', iso:'100', fl:'24mm', ap:'f/1.7', sh:'1/800' },
    '14-canyon': { cam:'Sony α7R IV', lens:'FE 16-35mm F2.8 GM', iso:'100', fl:'24mm', ap:'f/11', sh:'1/200' },
    '08-flower': { cam:'Fujifilm X-T5', lens:'XF 56mm F1.2', iso:'250', fl:'56mm', ap:'f/2', sh:'1/500' },
    '16-interior': { cam:'Sony α7R IV', lens:'FE 35mm F1.4', iso:'400', fl:'35mm', ap:'f/2', sh:'1/60' }
  };

  const FOLDERS = [
    { id:'pictures', name:'图片', parent:null, path:'~/Pictures', source:'pictures' },
    { id:'2024', name:'2024', parent:'pictures', path:'~/Pictures/2024', source:'pictures' },
    { id:'2024-05', name:'5 月', parent:'2024', path:'~/Pictures/2024/5月', source:'pictures' },
    { id:'2024-04', name:'4 月', parent:'2024', path:'~/Pictures/2024/4月', source:'pictures' }
  ];

  const ALBUMS = [
    { id:'kyoto', name:'京都之行', cover:'17-street' },
    { id:'coast', name:'海边', cover:'04-beach' },
    { id:'still', name:'静物', cover:'08-flower' }
  ];

  const TAGS = [
    { id:'旅行', name:'旅行', color:'#0485f7' },
    { id:'家人', name:'家人', color:'#e2457f' },
    { id:'2024夏', name:'2024 夏', color:'#d4a017' }
  ];

  const SOURCES = [
    { id:'pictures', name:'本机图库', path:'~/Pictures', status:'online', note:'演示来源，非真实扫描' },
    { id:'sd', name:'旅行 SD 卡', path:'/Volumes/TravelSD', status:'offline', note:'演示用失联目录' }
  ];

  const IMPORT_PRESETS = [
    { id:'desk', name:'桌面 / 2024 旅行', path:'~/Desktop/2024 旅行', count:8, note:'演示来源，不会读取你的真实文件' },
    { id:'scans', name:'下载 / 扫描旧照', path:'~/Downloads/扫描旧照', count:5, note:'演示来源，不会读取你的真实文件' },
    { id:'card', name:'外接盘 / SDCARD', path:'/Volumes/SDCARD', count:12, note:'可用于演示目录失联', risk:'offline' }
  ];

  const MONTH_META = {
    '2024-05': { label:'2024 年 5 月', countHint:186 },
    '2024-04': { label:'2024 年 4 月', countHint:220 }
  };

  const THUMB_MIN = 22, THUMB_MAX = 512, THUMB_DEFAULT = 144, THUMB_RESET = 96;

  const FAIL_KINDS = [
    { id: "permission", label: "无权限", retry: true },
    { id: "corrupt", label: "文件损坏", retry: true },
    { id: "missing", label: "目录失联", retry: true },
    { id: "unsupported", label: "格式不支持", retry: false },
    { id: "timeout", label: "超时", retry: true }
  ];
  const TASK_KINDS = ["首次索引", "刷新", "重建", "恢复", "回填尺寸", "校验"];
  const INDEX_STATES = ["等待", "扫描中", "生成缩略图中", "已完成", "部分失败", "暂停", "不可访问"];
  const ROOT_STATUS = { normal: "正常", closed: "已关闭", offline: "离线", removed: "索引已移除" };

  const ADD_FOLDER_PRESETS = [
    { id: "desk", name: "桌面 / 2024 旅行", path: "~/Desktop/2024 旅行", count: 8, kind: "new", note: "演示来源，不会读取你的真实文件" },
    { id: "pictures", name: "本机图库（已在库中）", path: "~/Pictures", count: 0, kind: "duplicate", note: "用于演示「已经在图库里」" },
    { id: "parent", name: "主文件夹 / 相册库", path: "~", count: 4, kind: "wraps", wraps: 2, note: "演示：新目录包住已有根" },
    { id: "child", name: "图片 / 2024 / 5 月（已在范围内）", path: "~/Pictures/2024/5月", count: 3, kind: "inside", note: "演示：新目录落在已有根里面" },
    { id: "denied", name: "系统目录 / 无权限", path: "~/Library/Application Support", count: 0, kind: "denied", err: "EPERM: permission denied", note: "演示权限失败，不会入库" },
    { id: "missing", name: "未挂载卷 / 旧备份", path: "/Volumes/OldBackup", count: 0, kind: "missing", err: "ENOENT: no such file or directory", note: "演示目录不存在或磁盘未挂载" },
    { id: "card", name: "外接盘 / SDCARD", path: "/Volumes/SDCARD", count: 6, kind: "new-offline", note: "可用于演示目录失联" }
  ];

  const DEMO_PACKS = [
    { id: "pack-ok", name: "photo-index-demo.json（演示包）", path: "demo/photo-index-demo.json", photos: 12, roots: 2, folders: 4, tags: 3, albums: 2, version: 4, dbVersion: 7, app: "OpenPicasa 0.4.2-beta.1（演示）", dups: 2, unmatched: 1, treeSrc: "manifest" },
    { id: "pack-derived", name: "legacy-index-demo.json（无清单树）", path: "demo/legacy-index-demo.json", photos: 6, roots: 1, folders: 2, tags: 1, albums: 1, version: 3, dbVersion: 6, app: "OpenPicasa 0.3（演示）", dups: 0, unmatched: 1, treeSrc: "derived" }
  ];

  function defaultIndexJob() {
    return {
      label: "",
      status: "idle",
      done: 0,
      discovered: 0,
      elapsed: 0,
      added: 0,
      note: "演示索引，未扫描真实磁盘，原文件未被写入。"
    };
  }

  function decoratePhotos(photos) {
    const recentChanged = ["07-city", "08-flower", "21-spring", "04-beach", "19-fog", "11-food"];
    return photos.map(function (p, i) {
      const ex = SAMPLE_EXIF[p.id] || SAMPLE_EXIF[p.file.replace(".jpg", "")];
      p.caption = p.caption || "";
      p.format = p.format || "JPEG";
      if (p.id === "09-arch") p.format = "PNG";
      if (p.id === "20-night") p.format = "GIF";
      if (p.id === "14-canyon") p.format = "WebP";
      if (p.id === "06-desert") p.format = "TIFF";
      if (p.id === "24-path") p.format = "SVG";
      if (p.id === "13-meadow") p.format = "HEIC";
      if (p.id === "18-rocks") p.format = "AVIF";
      if (p.id === "11-food") p.format = "RAW";
      p.thumbState = p.thumbState || "ready";
      p.hasExif = !!ex;
      p.cam = ex ? ex.cam : "";
      p.lens = ex ? ex.lens : "";
      p.mtime = p.date + "T" + (p.time || "12:00") + ":00";
      if (recentChanged.indexOf(p.id) >= 0) p.changedAt = "2024-05-22T18:" + String(10 + i).padStart(2, "0") + ":00";
      else p.changedAt = p.mtime;
      if (p.id === "16-interior") p.caption = "窗边下午的光线，演示说明只写索引。";
      return p;
    });
  }

  const PLACEHOLDERS = [
    { id: "ph-pending", file: "", title: "（待生成缩略图）", w: 1400, h: 933, bytes: 0, date: "2024-05-22", time: "11:00", folder: "2024-05", starred: false, rating: 0, tags: [], albums: [], recent: false, offline: false, thumbState: "pending", format: "JPEG", hasExif: false, changedAt: "2024-05-22T11:00:00" },
    { id: "ph-fail", file: "", title: "（生成失败）", w: 1400, h: 933, bytes: 0, date: "2024-05-22", time: "11:02", folder: "2024-05", starred: false, rating: 0, tags: [], albums: [], recent: false, offline: false, thumbState: "failed", format: "NEF", hasExif: false, changedAt: "2024-05-22T11:02:00" },
    { id: "ph-cloud", file: "", title: "", w: 1400, h: 933, bytes: 0, date: "2024-05-22", time: "11:04", folder: "2024-05", starred: false, rating: 0, tags: [], albums: [], recent: false, offline: false, thumbState: "cloud", format: "JPEG", hasExif: false, changedAt: "2024-05-22T11:04:00" }
  ];

  const IMPORT_PHOTOS = [
    { id: "imp-01", file: "17-street.jpg", title: "导入：雨后的街巷", w: 1400, h: 861, bytes: 407685, date: "2023-11-02", time: "16:20", folder: "imp-kyoto", starred: false, rating: 0, tags: ["旅行"], albums: [], recent: false, offline: true, source: "imp-trip", thumbState: "ready", format: "JPEG", hasExif: false, changedAt: "2023-11-02T16:20:00", importPath: "/Users/src/京都/街巷.jpg" },
    { id: "imp-02", file: "21-spring.jpg", title: "导入：庭院", w: 1400, h: 927, bytes: 145738, date: "2023-11-02", time: "09:10", folder: "imp-kyoto", starred: true, rating: 0, tags: ["旅行"], albums: [], recent: false, offline: true, source: "imp-trip", thumbState: "ready", format: "JPEG", hasExif: false, changedAt: "2023-11-02T09:10:00", importPath: "/Users/src/京都/庭院.jpg" }
  ];

  function ago(ms) { return Date.now() - ms; }

  function seedMgmt() {
    const now = Date.now();
    return {
      schema: 13,
      tab: "tasks",
      pausedBySystem: false,
      globalPaused: false,
      overlay: { batchId: null, dismissed: false, collapsed: false, shown: false, startedAt: 0 },
      progressOpen: false,
      failedOpen: false,
      completedOpen: false,
      frameStats: false,
      thumbsComputing: false,
      verify: null,
      hiddenImportSources: [],
      lastExportInterrupted: false,
      lastExportInterruptedMeta: { path: "~/Desktop/photo-index.opicasaidx", thumbs: false },
      lastImportRolledBack: false,
      lastImportCommitted: null,
      io: {
        state: "idle", phase: "", done: 0, total: 0, known: false, message: "", current: "",
        preview: null, step: 1, policy: "merge", ack: false, includeThumbs: false,
        lastBackup: null, errors: [], result: null, packId: null, cancelled: false,
        running: false, startedAt: 0
      },
      failures: {
        permission: { count: 0, ignored: 0, ids: [] },
        corrupt: { count: 0, ignored: 0, ids: [] },
        missing: { count: 0, ignored: 0, ids: [] },
        unsupported: { count: 0, ignored: 0, ids: [] },
        timeout: { count: 0, ignored: 0, ids: [] }
      },
      failedItems: [],
      tasks: {
        running: [],
        nextId: 5,
        history: [
          { id: 1, taskId: 1, scope: "~/Pictures", kind: "首次索引", state: "已完成", outcome: "成功", completed: 19, skipped: 0, failed: 0, discovered: 19, startedAt: now - 3 * 86400000, endedAt: now - 3 * 86400000 + 252000, cancelReason: "" },
          { id: 2, taskId: 2, scope: "/Volumes/TravelSD", kind: "刷新", state: "已完成", outcome: "成功", completed: 4, skipped: 0, failed: 0, discovered: 4, startedAt: now - 4 * 86400000, endedAt: now - 4 * 86400000 + 88000, cancelReason: "" },
          { id: 3, taskId: 3, scope: "~/Pictures", kind: "校验", state: "已完成", outcome: "成功", completed: 19, skipped: 0, failed: 0, discovered: 19, startedAt: now - 86400000, endedAt: now - 86400000 + 41000, cancelReason: "" },
          { id: 4, taskId: 4, scope: "~/Documents/扫描旧照", kind: "刷新", state: "已完成", outcome: "已取消", completed: 2, skipped: 1, failed: 0, discovered: 8, startedAt: now - 20 * 86400000, endedAt: now - 20 * 86400000 + 15000, cancelReason: "用户取消" }
        ]
      },
      roots: [
        { id: "pictures", path: "~/Pictures", status: "normal", photos: 20, thumbsBytes: 18432000, failed: 0, lastIndexed: now - 3600000, watch: "live", watchReason: "", watchNeed: 0, watchHave: 0, sourceId: "pictures", marks: 8 },
        { id: "sd", path: "/Volumes/TravelSD", status: "normal", photos: 1, thumbsBytes: 216590, failed: 0, lastIndexed: now - 4 * 86400000, watch: "none", watchReason: "", sourceId: "sd", marks: 0 },
        { id: "scans", path: "~/Documents/扫描旧照", status: "closed", photos: 8, thumbsBytes: 3200000, failed: 0, lastIndexed: now - 20 * 86400000, watch: "none", watchReason: "已关闭", marks: 2 },
        { id: "archive", path: "/Users/demo/Archive", status: "removed", photos: 0, thumbsBytes: 0, failed: 0, lastIndexed: now - 40 * 86400000, watch: "none", watchReason: "", marks: 12 }
      ],
      exclusions: [
        { id: "ex-1", path: "~/Pictures/2024/私密", invalid: false },
        { id: "ex-2", path: "~/Pictures/2024/导出暂存", invalid: false }
      ],
      storage: {
        db: 8818688, wal: 1258291, thumbs: 18432000, thumbsN: 22, photos: 23,
        byRoot: [
          { id: "pictures", path: "~/Pictures", bytes: 18432000 },
          { id: "sd", path: "/Volumes/TravelSD", bytes: 216590 },
          { id: "scans", path: "~/Documents/扫描旧照", bytes: 3200000 }
        ],
        computed: true
      },
      importSources: [
        { id: "imp-trip", name: "京都备份", photos: 2, thumbs: 2, importedAt: now - 10 * 86400000, paths: ["/Users/src/京都"], online: false, hidden: false }
      ],
      folderJobs: [],
      deleting: false,
      verifyRunning: false
    };
  }

  function seed() {
    const photos = decoratePhotos(JSON.parse(JSON.stringify(PHOTOS))).concat(
      JSON.parse(JSON.stringify(IMPORT_PHOTOS))
    );
    return {
      version: 1,
      proto: "1.3",
      fixtureRev: 2,
      theme: "light",
      photos: photos,
      albums: JSON.parse(JSON.stringify(ALBUMS)),
      tags: JSON.parse(JSON.stringify(TAGS)),
      folders: JSON.parse(JSON.stringify(FOLDERS)),
      sources: JSON.parse(JSON.stringify(SOURCES)),
      tray: [],
      recentSearches: [],
      settings: {
        thumbSize: THUMB_DEFAULT,
        layout: "grid",
        sortKey: "date",
        sortDir: "desc",
        sort: "date-desc",
        thumbsFirst: false,
        showFilenames: true,
        recurseSubfolders: true,
        tagsSort: "name",
        showZeroTags: false,
        sidebarWidth: 228,
        collapsed: {},
        viewerBg: "auto",
        viewerInfo: false,
        watchNewFolders: true
      },
      indexJob: defaultIndexJob(),
      mgmt: seedMgmt(),
      ui: {
        sidebarOpen: true,
        trayOpen: false,
        libraryEmpty: false,
        forceOffline: false,
        forceDenied: false,
        forceLoading: false,
        searchScope: "all",
        importBatchIds: null,
        lastView: "all"
      }
    };
  }


  const DEMO_FAILURES = {
    permission: { count: 3, ignored: 1, ids: ["f-p1", "f-p2", "f-p3"] },
    corrupt: { count: 2, ignored: 0, ids: ["f-c1", "f-c2"] },
    missing: { count: 1, ignored: 0, ids: ["f-m1"] },
    unsupported: { count: 2, ignored: 0, ids: ["f-u1", "f-u2"] },
    timeout: { count: 0, ignored: 0, ids: [] }
  };
  const DEMO_FAILED_ITEMS = [
        { id: "f-p1", kind: "permission", name: "private/DSC_1001.JPG", reason: "没有读取权限", ignored: false, folder: "~/Pictures/2024" },
        { id: "f-p2", kind: "permission", name: "private/DSC_1002.JPG", reason: "没有读取权限", ignored: false, folder: "~/Pictures/2024" },
        { id: "f-p3", kind: "permission", name: "private/DSC_1003.JPG", reason: "没有读取权限", ignored: true, folder: "~/Pictures/2024" },
        { id: "f-c1", kind: "corrupt", name: "raw/broken.NEF", reason: "文件损坏，无法解码", ignored: false, folder: "~/Pictures/2024" },
        { id: "f-c2", kind: "corrupt", name: "raw/truncated.CR3", reason: "文件损坏，无法解码", ignored: false, folder: "~/Pictures/2024" },
        { id: "f-m1", kind: "missing", name: "card/IMG_8801.JPG", reason: "目录失联", ignored: false, folder: "/Volumes/TravelSD" },
        { id: "f-u1", kind: "unsupported", name: "docs/notes.HEIC.bak", reason: "格式不支持", ignored: false, folder: "~/Pictures/2024" },
        { id: "f-u2", kind: "unsupported", name: "scan/file.XYZ", reason: "格式不支持", ignored: false, folder: "~/Pictures/2024" }
      ];
  const DEMO_FAIL_TASK = { id: 2, taskId: 2, scope: "/Volumes/TravelSD", kind: "刷新", state: "部分失败", outcome: "部分失败", completed: 1, skipped: 0, failed: 3, discovered: 4, startedAt: 0, endedAt: 0, cancelReason: "" };

  g.OPData = {
    PHOTOS, SAMPLE_EXIF, FOLDERS, ALBUMS, TAGS, SOURCES, IMPORT_PRESETS, MONTH_META, seed, kb,
    THUMB_MIN: THUMB_MIN, THUMB_MAX: THUMB_MAX, THUMB_DEFAULT: THUMB_DEFAULT, THUMB_RESET: THUMB_RESET,
    defaultIndexJob: defaultIndexJob, seedMgmt: seedMgmt, decoratePhotos: decoratePhotos,
    FAIL_KINDS: FAIL_KINDS, TASK_KINDS: TASK_KINDS, INDEX_STATES: INDEX_STATES, ROOT_STATUS: ROOT_STATUS,
    ADD_FOLDER_PRESETS: ADD_FOLDER_PRESETS, DEMO_PACKS: DEMO_PACKS, PLACEHOLDERS: PLACEHOLDERS, IMPORT_PHOTOS: IMPORT_PHOTOS, DEMO_FAILURES: DEMO_FAILURES, DEMO_FAILED_ITEMS: DEMO_FAILED_ITEMS, DEMO_FAIL_TASK: DEMO_FAIL_TASK,
    photoSrc: function (file) { return file ? ("photos/" + file) : ""; }
  };
})(window);
