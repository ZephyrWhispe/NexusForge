# GOV/P-01..P-09 源码范式断言（docs/review-2026-09-25/07 §3 落地）。
# 每条断言对应审查发现；命中即 exit 1 并列出行号。豁免必须写进本文件并附理由。
$ErrorActionPreference = 'Stop'
$fail = 0

function Invoke-Scan {
    param($Label, $Pattern, $Path, [string[]]$AllowPattern)
    $hits = @()
    $files = if (Test-Path $Path -PathType Leaf) { Get-Item $Path } else {
        Get-ChildItem -Path $Path -Recurse -Include *.rs |
            Where-Object { $_.FullName -notmatch '\\tests\\' }
    }
    foreach ($f in $files) {
        $lines = @(Get-Content $f.FullName)
        $inTest = $false
        for ($i = 0; $i -lt $lines.Count; $i++) {
            $ln = $lines[$i]
            # 启发式：文件级 #[cfg(test)] 之后的内容一律视为测试面（本仓测试模块均在文末）
            if ($ln -match '#\[cfg\(test\)\]') { $inTest = $true; continue }
            if ($inTest) { continue }
            if ($ln -match $Pattern) {
                # 豁免锚允许在命中行上一行至后两行窗口内（行上注释是 Rust 常态；rustfmt 会下移尾注）
                $window = @($ln)
                if ($i -ge 1) { $window += $lines[$i - 1] }
                if ($i + 1 -le $lines.Count - 1) {
                    $end = [Math]::Min($i + 2, $lines.Count - 1)
                    $window += $lines[($i + 1)..$end]
                }
                $allowed = $false
                foreach ($a in $AllowPattern) {
                    foreach ($w in $window) { if ($w -match $a) { $allowed = $true; break } }
                    if ($allowed) { break }
                }
                if (-not $allowed) {
                    $hits += [pscustomobject]@{ Path = $f.FullName; Line = ($i + 1); Text = $ln.Trim() }
                }
            }
        }
    }
    if ($hits.Count -gt 0) {
        $script:fail = 1
        Write-Host "FAIL $Label :" -ForegroundColor Red
        $hits | ForEach-Object { Write-Host ("  {0}:{1}: {2}" -f $_.Path, $_.Line, $_.Text) }
    } else { Write-Host "ok   $Label" }
}

# P-02/COR-04/11：生产路径禁裸 std::fs::write——唯一豁免是 write_atomic 工具本体
# 与 "tmp 落地 + rename" 手工原子模式（tmp 命名即证据）。
Invoke-Scan 'P-02 生产代码裸 std::fs::write' 'std::fs::write\(' 'crates' @('tmp', 'write_atomic', 'P-02豁免')

# P-03/SEC-02：提权面禁 args 透传；:120 的 to_vec 已过 args_are_templated 模板白名单（SEC-02 注释锚）
Invoke-Scan 'P-03 maintenance.rs 裸 args.to_vec()' 'args\.to_vec\(\)' 'crates\win-integration\src\maintenance.rs' @('SEC-02')

# D-35 挂账的机器可判面在 deny.toml [advisories].ignore 台账（逐 ID 附理由）；
# wasmtime/russh/lopdf 升级立项落地后须同步删除对应 ignore 行。

# P-09/COR-25：命令层禁裸 limit.unwrap_or（统一 clamp_limit 预算）
Invoke-Scan 'P-09 commands 裸 limit.unwrap_or' 'limit\.unwrap_or\(' 'src-tauri\src\commands' @('clamp\(1, 500\)\s*$')

exit $fail
