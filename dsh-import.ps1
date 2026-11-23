# DSH 数据导入 —— Windows 版（配套 dsh-export.sh 的 win-chong 预设导出）
#
# 用法（PowerShell，以普通用户身份运行，不要管理员）：
#   powershell -ExecutionPolicy Bypass -File .\dsh-import.ps1 -Src D:\dsh-bak
#   powershell -ExecutionPolicy Bypass -File .\dsh-import.ps1 -Src D:\dsh-bak -DryRun
#
# 前置条件:
#   1. Mac 上用 `./dsh-export.sh <目录> win-chong` 导出（用户名固定为 chong）。
#      该预设已在 Mac 端把会话日志头 cwd、workspace.json 路径、会话目录名、
#      package.json 的 link: 路径全部改成 C:/Users/chong/Documents/...，
#      所以 Windows 端只需「拷贝」，不要再改路径/重命名目录（Windows 没有
#      zstd，也改不了压缩日志）。
#   2. Windows 上已安装并至少启动过一次同版本 DSH（让它生成 %USERPROFILE%\.dsh 骨架）。
#   3. 完全退出 DeepSeek Harness（任务栏图标 → 退出，不是关窗口）。
#   4. 本机 Windows 用户名必须是 chong（导出路径按此硬编码，见下方校验）。
#
# 关键保护：package.json 不整文件覆盖，只把源机器多出的 dependencies 合并进
#      来、保留目标机的 dsh.profile.bundles 基线。dsh-sync 文档明确记载
#      「跨机整文件覆盖插件清单曾致宿主 crash loop」。
param(
    [Parameter(Mandatory=$true)][string]$Src,
    [switch]$DryRun,
    # 导出预设里写死的目标用户名。若本机不是它，会话会打不开（日志里的
    # cwd 是 zstd 压缩的，Windows 端无法改写），因此默认硬校验。
    [string]$TargetUser = "chong"
)

$ErrorActionPreference = "Stop"
$DSH = Join-Path $env:USERPROFILE ".dsh"
$Src = (Resolve-Path $Src).Path

if (-not (Test-Path (Join-Path $Src ".dsh"))) {
    throw "导出目录里没有 .dsh 子目录：$Src（应为 dsh-export.sh 生成的目录）"
}

Write-Host "== DSH 导入 (Windows / $TargetUser) ==" -ForegroundColor Cyan
Write-Host "源:   $Src"
Write-Host "目标: $DSH"
Write-Host "当前 Windows 用户: $env:USERNAME  ($env:USERPROFILE)"
if ($DryRun) { Write-Host "*** DryRun：只打印计划，不写文件 ***" -ForegroundColor Yellow }
Write-Host ""

# ---------- 0a. 校验导出预设 ----------
$infoPath = Join-Path $Src "MIGRATION_INFO.txt"
$preset = "(未知)"
if (Test-Path $infoPath) {
    $m = Select-String -Path $infoPath -Pattern '^targetPreset\s*=\s*(.+)$' | Select-Object -First 1
    if ($m) { $preset = $m.Matches[0].Groups[1].Value.Trim() }
}
Write-Host "导出预设 targetPreset = $preset"
if ($preset -ne "win-chong") {
    $msg = @"
⚠️  这个导出不是 win-chong 预设（当前为 '$preset'）。
    会话日志是 zstd 压缩的，Windows 端没有 zstd、无法改写日志头里的 cwd，
    直接导入会导致会话列不出 / 打不开。
    请回到 Mac 重新执行：  ./dsh-export.sh <目录> win-chong
    然后重新拷贝整个目录过来。
"@
    Write-Host $msg -ForegroundColor Red
    $ans = Read-Host "仍要继续（只拷文件、不修会话）？输入 y 继续，其它取消"
    if ($ans -ne 'y') { Write-Host "已取消"; exit 1 }
}

# ---------- 0b. 校验用户名 ----------
if ($env:USERNAME -ne $TargetUser) {
    $msg = @"
⚠️  当前 Windows 用户名是 '$env:USERNAME'，但导出预设按 '$TargetUser'
    把会话 cwd 写死成了 C:/Users/$TargetUser/Documents/...。
    用户名不一致时 DSH 的 sameDirectory() 校验会失败，旧会话打不开。
    请改用用户名 $env:USERNAME 重新在 Mac 导出（改 dsh-export.sh 里的 NEW_ROOT），
    或换到用户名为 $TargetUser 的账户下运行本脚本。
"@
    Write-Host $msg -ForegroundColor Red
    $ans = Read-Host "仍要继续？输入 y 继续，其它取消"
    if ($ans -ne 'y') { Write-Host "已取消"; exit 1 }
}

# ---------- 0c. 检查 DSH 是否仍在运行 ----------
$procs = Get-Process -ErrorAction SilentlyContinue | Where-Object {
    $_.ProcessName -match 'DeepSeek|dsh' -and $_.Path -match 'DeepSeek Harness|dsh'
}
if ($procs) {
    Write-Host "⚠️  检测到 DSH 似乎仍在运行，请先完全退出。" -ForegroundColor Yellow
    $ans = Read-Host "继续请输 y，其它键取消"
    if ($ans -ne 'y') { Write-Host "已取消"; exit 1 }
}

function Copy-Tree([string]$From, [string]$To) {
    if (-not (Test-Path $From)) { return }
    if ($DryRun) {
        Write-Host "   [dry] robocopy $From -> $To"
    } else {
        New-Item -ItemType Directory -Force -Path $To | Out-Null
        # /E 含空目录，/COPY:DAT 不复制 ACL/属主（跨机器拷来的 ACL 会捣乱），
        # /XF 排除锁文件与 DS_Store，/XD 排除 node_modules、缓存、日志
        & robocopy $From $To /E /COPY:DAT /R:1 /W:1 /NFL /NDL /NJH /NJS /NP `
            /XF "*.lock" ".DS_Store" /XD "node_modules" "cache" "logs" | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "robocopy 失败 ($LASTEXITCODE): $From" }
    }
}

# ---------- 1. 备份新机器现状（可回滚） ----------
if (-not $DryRun -and (Test-Path $DSH)) {
    $bak = "$env:USERPROFILE\.dsh.before-import-$(Get-Date -Format yyyyMMdd-HHmmss)"
    Write-Host "-- 备份当前 .dsh → $bak"
    Copy-Item -Path $DSH -Destination $bak -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $DSH | Out-Null

# ---------- 2. 核心数据（会话日志已由 Mac 端改写好，原样拷回） ----------
foreach ($item in @('sessions','storages','skills','skill-manager','attachments',
                    'task-board','dsh-claude-style','workbuddy-vault','dsh-market','llm-deepseek')) {
    $from = Join-Path $Src ".dsh\$item"
    if (Test-Path $from) {
        Write-Host "-- .dsh\$item"
        Copy-Tree $from (Join-Path $DSH $item)
    }
}

# ---------- 3. 配置 + 插件清单 ----------
Write-Host "-- .dsh 顶层配置文件"
foreach ($f in @('settings.yaml.imported','.anonymous-user-id','.buddy-checkin.json',
                 '.workbuddy-ai-catalog.json','.workbuddy-catalog.json',
                 '.workbuddy-app-version.json','dsh-skill-picker-state.json','.credentials.yaml')) {
    $p = Join-Path $Src ".dsh\$f"
    if (Test-Path $p) {
        if ($DryRun) { Write-Host "   [dry] copy $f" }
        else { Copy-Item -Path $p -Destination (Join-Path $DSH $f) -Force }
    }
}

Write-Host "-- profiles\desktop 与 profiles\web"
foreach ($p in @('desktop','web')) {
    $from = Join-Path $Src ".dsh\profiles\$p"
    $to   = Join-Path $DSH "profiles\$p"
    if (-not (Test-Path $from)) { continue }

    $srcPkg = Join-Path $from "package.json"
    $dstPkg = Join-Path $to   "package.json"

    # 关键：Copy-Tree 里的 robocopy 会用源清单「覆盖」目标清单，所以必须
    # 在拷贝「之前」先把目标机器原有的 package.json 读入内存当合并基线。
    # 否则读到的已是覆盖后的源文件，合并等于空操作，crash-loop 保护形同虚设。
    $baseline = $null
    if (Test-Path $dstPkg) {
        try { $baseline = Get-Content $dstPkg -Raw -Encoding UTF8 | ConvertFrom-Json } catch {}
    }

    Copy-Tree $from $to

    if ($DryRun) {
        Write-Host "   [dry] package.json：拷贝后做 union 合并（保留本机 bundles 基线）"
        continue
    }
    if (-not (Test-Path $srcPkg)) { continue }
    if ($null -eq $baseline) {
        Write-Host "   package.json：目标机原本无清单，直接使用源清单（已含 Windows link 路径）"
        continue
    }

    # 合并策略（依据 dsh-sync README「跨机整文件覆盖插件清单曾致宿主 crash loop」）：
    # 以源清单为底（用户插件齐全、link: 路径已改写成 C:/Users/chong/...），
    # 再把目标机独有的 dependencies 与 bundles 并进来（union，绝不删宿主自带的
    # 内置 bundle）。源值优先，因为这是要迁移过来的用户清单。
    # 注意：必须同时合并 bundles，否则插件虽装了却不激活，用户插件等于没迁过来。
    try {
        $s = Get-Content $srcPkg -Raw -Encoding UTF8 | ConvertFrom-Json

        # 1) dependencies 并集：补进「目标机有、源机器没有」的依赖
        $srcDepNames = @()
        if ($s.dependencies) { $srcDepNames = @($s.dependencies.PSObject.Properties.Name) }
        $extraDeps = @()
        if ($baseline.dependencies) {
            foreach ($dep in $baseline.dependencies.PSObject.Properties) {
                if ($srcDepNames -notcontains $dep.Name) { $extraDeps += $dep }
            }
        }
        if ($extraDeps.Count -gt 0) {
            if (-not $s.dependencies) {
                $s | Add-Member -NotePropertyName 'dependencies' -NotePropertyValue (New-Object PSCustomObject)
            }
            foreach ($dep in $extraDeps) {
                $s.dependencies | Add-Member -NotePropertyName $dep.Name -NotePropertyValue $dep.Value
            }
        }

        # 2) bundles 并集：源顺序在前，补进目标机独有的内置/基线 bundle
        #    （crash loop 的根因就是「删掉了宿主自带 bundle」，union 永不删）
        $merged = New-Object System.Collections.Generic.List[string]
        if ($s.dsh.profile.bundles) {
            foreach ($b in @($s.dsh.profile.bundles)) {
                if (-not $merged.Contains([string]$b)) { $merged.Add([string]$b) }
            }
        }
        $addedBundle = 0
        if ($baseline.dsh.profile.bundles) {
            foreach ($b in @($baseline.dsh.profile.bundles)) {
                if (-not $merged.Contains([string]$b)) { $merged.Add([string]$b); $addedBundle++ }
            }
        }
        if ($s.dsh -and $s.dsh.profile) { $s.dsh.profile.bundles = @($merged.ToArray()) }

        $json = $s | ConvertTo-Json -Depth 20
        [System.IO.File]::WriteAllText($dstPkg, $json, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host "   package.json：union 合并完成（补依赖 $($extraDeps.Count)、补内置 bundle $addedBundle；源值优先）"
    } catch {
        Write-Host "   ⚠️  package.json 合并失败：$($_.Exception.Message)" -ForegroundColor Yellow
        # 失败时把目标机原清单写回，避免留下被 robocopy 覆盖的源清单
        $json = $baseline | ConvertTo-Json -Depth 20
        [System.IO.File]::WriteAllText($dstPkg, $json, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host "      已回写目标机原清单，请手工检查插件是否齐全"
    }
}

# ---------- 4. HOME 里的技能 / 记忆 ----------
$homeSrc = Join-Path $Src "home"
if (Test-Path $homeSrc) {
    $map = @{
        '.agents\skills'                 = '.agents\skills'
        '.workbuddy\skills'              = '.workbuddy\skills'
        '.mnemon'                        = '.mnemon'
        '.openviking'                    = '.openviking'
        '.config\billion-context'        = '.config\billion-context'
        '.local\share\billion-context'   = '.local\share\billion-context'
    }
    foreach ($k in $map.Keys) {
        $from = Join-Path $homeSrc $k
        if (Test-Path $from) {
            Write-Host "-- ~\$($map[$k])"
            Copy-Tree $from (Join-Path $env:USERPROFILE $map[$k])
        }
    }
}

# ---------- 5. 本地 link: 插件 ----------
$linked = Join-Path $Src "extras\deepseek workspace\dsh-full-width"
if (Test-Path $linked) {
    # 与导出预设写死的位置一致：C:\Users\chong\Documents\deepseek workspace\dsh-full-width
    $target = Join-Path $env:USERPROFILE "Documents\deepseek workspace\dsh-full-width"
    Write-Host "-- 本地插件 dsh-full-width → $target"
    Copy-Tree $linked $target
}

$helper = Join-Path $Src "extras\Application Support\dsh-sync"
if (Test-Path $helper) {
    $target = Join-Path $env:APPDATA "dsh-sync"
    Write-Host "-- 辅助脚本 → $target"
    Copy-Tree $helper $target
}

# ---------- 6. 一致性校验（只校验，不改文件） ----------
# 复刻 DSH 的 projectKey(cwd)：/ \ : 都变 '-'，其它非安全字符变 ~XXXX，
# 整体包在 '--...--' 中。用来确认 workspace.json 的 path 与会话目录名吻合。
function Get-ProjectKey([string]$cwd) {
    $sb = New-Object System.Text.StringBuilder
    $sep = $false
    foreach ($ch in $cwd.ToCharArray()) {
        $c = [string]$ch
        if ($c -eq '/' -or $c -eq '\' -or $c -eq ':') {
            if (-not $sep) { [void]$sb.Append('-') }
            $sep = $true
        } elseif ($c -ne '~' -and $c -match '^[A-Za-z0-9._-]$') {
            [void]$sb.Append($c); $sep = $false
        } else {
            # 与 JS/导出脚本一致：~ + 大写 4 位十六进制码元，如空格 → ~0020
            [void]$sb.Append('~' + ([int][char]$c).ToString('X4').ToUpperInvariant())
            $sep = $false
        }
    }
    $s = ($sb.ToString() -replace '^-+','')
    if (-not $s) { $s = 'root' }
    if ($s.Length -gt 251) { $s = $s.Substring(0,251) }
    return "--$s--"
}

Write-Host ""
Write-Host "-- 一致性校验（workspace.json path ↔ 会话目录名）"
$wsJson = Join-Path $DSH "storages\workspace.json"
if (-not $DryRun -and (Test-Path $wsJson)) {
    $ws = Get-Content $wsJson -Raw -Encoding UTF8 | ConvertFrom-Json
    foreach ($w in $ws.tables.workspaces.PSObject.Properties.Value) {
        $expect = Get-ProjectKey $w.path
        $dir = Join-Path $DSH "sessions\$expect"
        $ok = Test-Path $dir
        $flag = if ($ok) { "OK" } else { "缺失" }
        $color = if ($ok) { "Green" } else { "Red" }
        Write-Host "   [$flag] $($w.path)  →  $expect" -ForegroundColor $color
        if (-not $ok) {
            Write-Host "         ⚠️  该工作区的会话目录不存在，旧会话不会出现在侧栏。" -ForegroundColor Yellow
            Write-Host "            可能原因：用户名不是 $TargetUser，或项目路径与导出预设不一致。" -ForegroundColor Yellow
        }
    }
}

Write-Host ""
Write-Host "== 导入完成 ==" -ForegroundColor Green
$finalMsg = @"

接下来（重要）：
1. 用记事本打开 %USERPROFILE%\.dsh\profiles\desktop\package.json，检查
   "dsh-full-width": "link:C:/Users/$TargetUser/Documents/deepseek workspace/dsh-full-width"
   是否指向本机真实存在的目录；不想用这个本地插件就直接删掉该行。
2. 启动 DeepSeek Harness。它读取 package.json 后会自动用 pnpm 重装插件
   （首次启动较慢、需联网）。不要手工拷 node_modules —— Mac 的 arm64
   原生模块在 Windows 上一定跑不起来。
3. 会话已在 Mac 端按 C:/Users/$TargetUser/Documents/<项目> 改写并重命名目录。
   在 DSH 里打开/新建工作区时请选择对应路径（例如
   C:\Users\$TargetUser\Documents\OpenPIcasa），旧会话才会出现在侧栏。
4. 若某些会话仍不显示：这些会话的 cwd 在导出时就不属于被改写的工作区
   （例如 /Users/chongsenli/... 或 /Users/starinight 这类「孤儿」目录），
   它们仍保留 Mac 路径，Windows 的 isAbsolute() 不认，属预期现象。
5. 回滚：把 %USERPROFILE%\.dsh.before-import-<时间戳> 改回 .dsh 即可。
"@
Write-Host $finalMsg
