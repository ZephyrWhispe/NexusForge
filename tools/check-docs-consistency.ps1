# P-12 文档-代码一致性核对（docs/review-2026-09-25/07 §3 落地）。
# 断言：①DESIGN §6.3 代码块 `fn` 命令名 ⊆ generate_handler! 注册表；
#      ②docs/DECISIONS.md 详情条目 D-xx 全部出现在 §1 摘要表；
#      ③README/文档引用的 `cargo test -p <crate>` 的 crate 真实存在。
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

exit $fail
