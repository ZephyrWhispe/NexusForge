# GOV/P-01..P-10 源码范式断言（docs/review-2026-09-25/07 §3 落地）。
# 每条断言对应审查发现；命中即 exit 1 并列出行号。豁免必须写进本文件并附理由。
$ErrorActionPreference = 'Stop'
$fail = 0

function Invoke-Scan {
    param(
        $Label, $Pattern, $Paths, [string[]]$AllowPattern,
        [int]$Lookback = 1,
        [string[]]$Include = @('*.rs'),
        [string]$ExcludePathPattern = '\\tests\\',
        [string]$AllowFilePattern
    )
    $hits = @()
    $files = foreach ($p in @($Paths)) {
        if (Test-Path $p -PathType Leaf) { Get-Item $p } else {
            Get-ChildItem -Path $p -Recurse -Include $Include |
                Where-Object { $_.FullName -notmatch $ExcludePathPattern }
        }
    }
    foreach ($f in $files) {
        if ($AllowFilePattern -and $f.FullName -match $AllowFilePattern) { continue }
        # 显式 UTF-8：本仓 .rs 注释含中文，PS 5.1 默认 ANSI 读取会乱码
        # 致豁免锚匹配失效（配合本脚本 UTF-8 BOM 保存，二者同根修）
        $lines = @(Get-Content $f.FullName -Encoding UTF8)
        $inTest = $false
        for ($i = 0; $i -lt $lines.Count; $i++) {
            $ln = $lines[$i]
            # 启发式：文件级**顶格** #[cfg(test)]（测试模块声明）之后的内容一律视为测试面
            # （本仓测试模块均在文末）；缩进 cfg(test) 是生产代码内联特性，不得触发截断
            if ($ln -match '^#\[cfg\(test\)\]') { $inTest = $true; continue }
            if ($inTest) { continue }
            if ($ln -match $Pattern) {
                # 豁免锚允许在命中行前 Lookback 行至后两行窗口内（行上注释是 Rust 常态；rustfmt 会下移尾注）
                $window = @()
                $from = [Math]::Max(0, $i - $Lookback)
                $to = [Math]::Min($i + 2, $lines.Count - 1)
                for ($j = $from; $j -le $to; $j++) { $window += $lines[$j] }
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

# P-01/SEC-03..06：路径唯一入口——三 crate 生产代码禁 root.join( 直连用户输入。
# 豁免锚＝命中行上方 6 行回看窗内的段级校验调用（safe_rel_path/norm_rel/resolve_in_root）；
# 回看窗取 6 系实测：ops.rs 解压腿的 safe_rel_path 守卫在命中行上方 5 行（SEC-05 锚）。
Invoke-Scan 'P-01 root.join( 未经路径入口' 'root\.join\(' `
    @('crates\notes-core\src', 'crates\screenshot-core\src', 'crates\file-core\src') `
    @('safe_rel_path', 'norm_rel', 'resolve_in_root', 'P-01豁免') -Lookback 6

# SEC-07/STD 侧：dangerouslySetInnerHTML 全前端仅 MarkdownView.tsx 一处落点
# （其内部已过 DOMPurify；测试文件在 __tests__ 下天然排除，扫描面 ts/tsx）。
Invoke-Scan 'SEC-07 dangerouslySetInnerHTML 游离于 MarkdownView 之外' 'dangerouslySetInnerHTML' 'src' @() `
    -Include @('*.ts', '*.tsx') -ExcludePathPattern '__tests__' -AllowFilePattern 'MarkdownView\.tsx$'

# STD-10/PERF-10：TOPIC_REGISTRY 每个注册主题必须有发布证据（Rust publish/Event::new 邻接，
# 含 src-tauri 命令腿）与订阅证据（Rust subscribe* 邻接，或前端字面量/模块前缀订阅——
# KvmPanel 等按 startsWith("kvm.") 收流，nf:event 全主题转发，前缀即订阅证据）。
# 无证据者须列入 $TopicReserve 并附理由（形同 deny.toml ignore 台账：显式债不静默）。
# D-39：D-38 首跑抓出的四枚"仅发布腿"预留（host.module_crashed/automation.notify/
# screenshot.taken/ocr.failed）已全部接线落地、台账清零——机制常驻，未来新悬空主题
# 入此台账须附理由，接线或摘除后再删条目（反向过期检查会强制）。
$TopicReserve = @{}

$eventsRaw = Get-Content 'crates\host-core\src\events.rs' -Raw -Encoding UTF8
$reg = [regex]::Match($eventsRaw, '(?s)TOPIC_REGISTRY[^=]*=\s*&\[(.*?)\];')
if (-not $reg.Success) {
    $fail = 1
    Write-Host 'FAIL STD-10 无法解析 TOPIC_REGISTRY（events.rs 形状漂移，随源更新本扫描）' -ForegroundColor Red
} else {
    $topics = [regex]::Matches($reg.Groups[1].Value, '"([a-z][a-z0-9_]*\.[a-z][a-z0-9_]*)"') |
        ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique

    $rustCorpus = ''
    $eventsFull = (Resolve-Path 'crates\host-core\src\events.rs').Path
    Get-ChildItem 'crates', 'src-tauri\src' -Recurse -Include *.rs |
        Where-Object { $_.FullName -notmatch '\\tests\\' -and $_.FullName -ne $eventsFull } |
        ForEach-Object {
            $txt = Get-Content $_.FullName -Raw -Encoding UTF8
            # 只从**顶格** #[cfg(test)]（测试模块声明）处截尾；缩进的 cfg(test) 是
            # 生产代码内联特性（proxy-core/service.rs 多处），从它截会吞掉后段生产腿
            $m = [regex]::Match($txt, '(?m)^#\[cfg\(test\)\]')
            if ($m.Success) { $txt = $txt.Substring(0, $m.Index) }
            $rustCorpus += $txt
        }
    $jsCorpus = ''
    Get-ChildItem 'src' -Recurse -Include *.ts, *.tsx |
        Where-Object { $_.FullName -notmatch '__tests__|\.test\.|\.spec\.' } |
        ForEach-Object { $jsCorpus += (Get-Content $_.FullName -Raw -Encoding UTF8) }

    $odd = @()
    foreach ($t in $topics) {
        $esc = [regex]::Escape($t)
        # 发布证据三分支：Event::new/publish* 邻接；或 topic("<t>") 注册表取名口
        # （模块层发布入口——host-core/events.rs 注释口径，取到名即进发布链）
        $hasPub = ([regex]::IsMatch($rustCorpus, "(?s)(?:Event::new|publish[_a-z]*)[^""]{0,90}""$esc""") `
            -or [regex]::IsMatch($rustCorpus, "topic\(\s*""$esc"""))
        $prefix = ($t -split '\.')[0]
        $hasSub = ([regex]::IsMatch($rustCorpus, "(?s)subscribe[_a-z]*[^""]{0,90}""$esc""") `
            -or $jsCorpus.Contains("""$t""") -or $jsCorpus.Contains("""$prefix."""))
        $reserved = $TopicReserve.ContainsKey($t)
        if ((-not $hasPub -or -not $hasSub) -and -not $reserved) {
            $why = @()
            if (-not $hasPub) { $why += '无发布证据' }
            if (-not $hasSub) { $why += '无订阅证据' }
            $odd += ("  {0}: {1}" -f $t, ($why -join ' + '))
        }
        if ($reserved -and $hasPub -and $hasSub) {
            $odd += ("  {0}: 预留清单已过期（证据齐备，删条目）" -f $t)
        }
    }
    if ($odd.Count -gt 0) {
        $fail = 1
        Write-Host "FAIL STD-10 主题接线证据（$($odd.Count) 项/$($topics.Count) 主题）:" -ForegroundColor Red
        $odd | ForEach-Object { Write-Host $_ }
    } else {
        Write-Host "ok   STD-10 TOPIC_REGISTRY $($topics.Count) 主题发布/订阅证据齐（预留 $($TopicReserve.Count)）"
    }
}

exit $fail
