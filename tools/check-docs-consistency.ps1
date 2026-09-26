# P-12 文档-代码一致性核对（docs/review-2026-09-25/07 §3 落地）。
# 断言：①DESIGN §6.3 代码块 `fn` 命令名 ⊆ generate_handler! 注册表；
#      ②docs/DECISIONS.md 详情条目 D-xx 全部出现在 §1 摘要表；
#      ③README/文档引用的 `cargo test -p <crate>` 的 crate 真实存在；
#      ④显式限定的 `目标 §x.y` 引用可解析到目标文档的编号标题（DESIGN §3/§4 收口）；
#      ⑤README "N 个功能模块" == src/layout/modules.ts MODULES 条目数。
$ErrorActionPreference = 'Stop'
$fail = 0

# ① DESIGN §6.3 代码块内出现的 `fn 名` ⊆ generate_handler! 注册表（07 §3 原口径）
$lib = Get-Content 'src-tauri\src\lib.rs' -Raw
$m = [regex]::Match($lib, 'generate_handler!\[(?s)(.*?)\]\)')
$registered = [regex]::Matches($m.Groups[1].Value, '(\w+)::(\w+)') |
    ForEach-Object { $_.Groups[2].Value }
if (-not $registered) { $registered = [regex]::Matches($m.Groups[1].Value, '\b([a-z][a-z0-9_]{3,})\b') | ForEach-Object { $_.Groups[1].Value } }
$registered = $registered | Sort-Object -Unique

$design = Get-Content 'docs\DESIGN.md' -Raw
$sec = [regex]::Match($design, '(?ms)### 6\.3.*?### 6\.4').Groups[0].Value
$docRefs = [regex]::Matches($sec, '\bfn\s+([a-z][a-z0-9_]{3,})\b') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
$bad = $docRefs | Where-Object { $registered -notcontains $_ }
if ($bad) { $fail = 1; Write-Host 'FAIL DESIGN §6.3 引用了未注册命令:' -ForegroundColor Red; $bad | ForEach-Object { Write-Host "  $_" } }
else { Write-Host "ok   DESIGN §6.3 命令引用 ⊆ 注册表（§6.3 面 $($docRefs.Count) / 注册面 $($registered.Count)）" }

# ② DECISIONS 摘要表覆盖全部详情条目
$dec = Get-Content 'docs\DECISIONS.md' -Raw
$details = [regex]::Matches($dec, '(?m)^###\s+(D-\d+)') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
$summary = [regex]::Matches($dec, '(?m)^\|\s*(D-\d+)\s*\|') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
$missing = $details | Where-Object { $summary -notcontains $_ }
if ($missing) { $fail = 1; Write-Host 'FAIL DECISIONS §1 摘要表缺行:' -ForegroundColor Red; $missing | ForEach-Object { Write-Host "  $_" } }
else { Write-Host "ok   DECISIONS §1 覆盖全部 $($details.Count) 条详情" }

# ③ 文档中的 cargo test -p <crate> 必须存在（review-*/ 为历史审查存档，引文不核）；
#    artifact-core 为 D-32 暂缓的规划 crate，放行须随 B9 立项落地后移除
$crateDirs = (Get-ChildItem 'crates' -Directory).Name + 'nexusforge' + 'artifact-core'
$testRefs = Get-ChildItem 'docs' -Recurse -Include *.md |
    Where-Object { $_.FullName -notmatch '\\review-' } |
    Select-String -Pattern 'cargo test -p ([\w-]+)' -AllMatches |
    ForEach-Object { $_.Matches } | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
$ghost = $testRefs | Where-Object { $crateDirs -notcontains $_ }
if ($ghost) { $fail = 1; Write-Host 'FAIL 文档引用不存在的 crate:' -ForegroundColor Red; $ghost | ForEach-Object { Write-Host "  $_" } }
else { Write-Host "ok   文档 crate 引用全部存在（$($testRefs.Count) 个）" }

# ④ 显式限定的 `目标 §x.y` 引用可解析：目标 ∈ {路径.md | DESIGN|IMPLEMENTATION|DECISIONS|
#    CONTRIBUTING|SECURITY（docs/ 同名）| README（仓库根）| impl/NN（docs/impl/NN-*.md）}；
#    命中判据＝目标文档存在编号标题恰等于引用号、或以其为父级前缀（§11 命中 "## 11."，§6 命中 "### 6.3"）。
#    裸 §x.y 仅当引用号在本档案内不可解析、但在 DESIGN.md 可解析时按漂移计数
#    ——本仓裸 § 的主流口径是 DESIGN 条文号（"§11 修订表"家族），其余裸引用不核（误报面大）。
$docFiles = Get-ChildItem 'docs' -Recurse -Include *.md | Where-Object { $_.FullName -notmatch '\\review-' }
$headCache = @{}
function Get-HeadingNums {
    param($path)
    if (-not $headCache.ContainsKey($path)) {
        if ($null -eq $path -or -not (Test-Path $path)) { $headCache[$path] = $null }
        else {
            $raw = Get-Content $path -Raw -Encoding UTF8
            # 编号标题口径：`# N.`／`### N.M 题文` 两式皆收（DESIGN 顶级带点、
            # panels 子级以空格分隔——不得要求尾点，否则整面编号标题被误杀）
            $headCache[$path] = @([regex]::Matches($raw, '(?m)^#{1,6}\s+([0-9]+(?:\.[0-9]+)*)\.?\s') |
                ForEach-Object { $_.Groups[1].Value })
        }
    }
    return $headCache[$path]
}
function Test-Anchor {
    param($heads, $ref)
    if ($null -eq $heads) { return $false }
    foreach ($h in $heads) { if ($h -eq $ref -or $h.StartsWith("$ref.")) { return $true } }
    return $false
}
$anchorOdd = @()
$anchorNote = @()
$anchorCount = 0
foreach ($d in $docFiles) {
    $raw = Get-Content $d.FullName -Raw -Encoding UTF8
    foreach ($m in [regex]::Matches($raw, '(?m)(?<t>[\w./-]+\.md|\bDESIGN\b|\bIMPLEMENTATION\b|\bDECISIONS\b|\bCONTRIBUTING\b|\bSECURITY\b|\bREADME\b|impl/\d{2})\s*§\s*(?<sec>\d+(?:\.\d+)*)')) {
        $anchorCount++
        $ref = $m.Groups['sec'].Value
        $tgt = $m.Groups['t'].Value
        $path = $null
        if ($tgt -like '*.md') {
            # 引用文件名解析链：同目录 → docs/ → docs 全树递归（panels/impl 就近放名的常态）→ 仓库根
            $cands = @((Join-Path $d.DirectoryName $tgt), (Join-Path 'docs' $tgt), 'README.md')
            $hit = $cands | Where-Object { Test-Path $_ } | Select-Object -First 1
            if (-not $hit) { $hit = Get-ChildItem 'docs' -Recurse -Filter (Split-Path $tgt -Leaf) | Where-Object { $_.FullName -notmatch '\\review-' } | Select-Object -First 1 }
            $path = if ($hit) { if ($hit -is [string]) { $hit } else { $hit.FullName } }
        } elseif ($tgt -match '^impl/\d{2}$') {
            $f = Get-ChildItem ("docs\{0}-*.md" -f $tgt) -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($f) { $path = $f.FullName }
        } elseif ($tgt -eq 'README') {
            # 面板文档的 "README §0.x" 指本目录 README（panels/README 总则家族），就近解析
            $local = Join-Path $d.DirectoryName 'README.md'
            $path = if (Test-Path $local) { $local } else { Join-Path (Get-Location) 'README.md' }
        } else {
            $path = "docs\$tgt.md"
        }
        if (-not $path -or -not (Test-Path $path)) { $anchorOdd += "$($d.Name): 『$($m.Value)』 目标文档不存在（$tgt）"; continue }
        $heads = Get-HeadingNums $path
        if ($heads.Count -eq 0) { $anchorNote += "$($d.Name): 『$($m.Value)』 目标未采用编号标题（$tgt），锚点不核仅显影"; continue }
        if (-not (Test-Anchor $heads $ref)) { $anchorOdd += "$($d.Name): 『$($m.Value)』 锚点悬空（$tgt §$ref 无编号标题）" }
    }
}
if ($anchorOdd.Count -gt 0) {
    $fail = 1
    Write-Host "FAIL §锚点核对（$($anchorOdd.Count) 项 / 显式引用 $anchorCount 处）:" -ForegroundColor Red
    $anchorOdd | ForEach-Object { Write-Host "  $_" }
} else { Write-Host "ok   §锚点核对：$anchorCount 处显式引用全部可解析（$($anchorNote.Count) 处目标未编号仅显影）" }
$anchorNote | ForEach-Object { Write-Host "  note $_" -ForegroundColor DarkGray }

# ⑤ README "N 个功能模块" == MODULES 条目数（07 §3 "README 模块数==MODULES.length"）
$readmeRaw = Get-Content 'README.md' -Raw -Encoding UTF8
$rmMatch = [regex]::Match($readmeRaw, '(\d+)\s*个功能模块')
$modsRaw = Get-Content 'src\layout\modules.ts' -Raw -Encoding UTF8
$modsArr = [regex]::Match($modsRaw, '(?s)export const MODULES[^=]*=\s*\[(.*?)\];').Groups[1].Value
$modsCount = @([regex]::Matches($modsArr, '\{\s*id:')).Count
if (-not $rmMatch.Success) { $fail = 1; Write-Host 'FAIL README 未找到 "N 个功能模块" 口径句（随源更新本断言）' -ForegroundColor Red }
elseif ([int]$rmMatch.Groups[1].Value -ne $modsCount) {
    $fail = 1
    Write-Host "FAIL README 模块数 $($rmMatch.Groups[1].Value) ≠ MODULES.length $modsCount" -ForegroundColor Red
} else { Write-Host "ok   README 模块数 == MODULES.length（$modsCount）" }

exit $fail
