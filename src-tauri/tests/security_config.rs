//! D-28 发布安全面配置回归：updater 真实公钥 / NSIS+MSI 双目标 / CSP / 按窗最小授权。
//!
//! 判定对象是**落盘配置本身**（tauri.conf.json + permissions/*.toml + capabilities/*.json
//! + lib.rs generate_handler 注册表），因为 tauri 2 的 app 命令 ACL 语义是"权限文件一经
//!   定义即全量强制"——配置形状正确 = 运行时行为正确（构建期 tauri-build 另校验 capability
//!   引用的权限标识存在性）。断言函数保持纯函数并自带失败负例，证明判定不是恒真。

use std::collections::BTreeSet;

use base64::Engine as _;

fn read(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读取 {} 失败: {e}", path.display()))
}

fn config() -> serde_json::Value {
    serde_json::from_str(&read("tauri.conf.json")).expect("tauri.conf.json 应为合法 JSON")
}

/// 公钥必须是可解码的 minisign ed25519 公钥。tauri.conf 存的是整个 .pub 文件内容
/// 的一行 base64，解码后为两行文本：untrusted comment + 公钥体的 base64。
fn valid_minisign_pubkey(pubkey: &str) -> bool {
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(pubkey.trim()) else {
        return false;
    };
    let Ok(text) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((comment, key_b64)) = text.split_once('\n') else {
        return false;
    };
    if !comment.starts_with("untrusted comment: minisign public key:") {
        return false;
    }
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(key_b64.trim_end()) else {
        return false;
    };
    // 实测平台事实：minisign ed25519 公钥体解码为 42 字节，前两字节为密钥类型 "Ed"
    raw.len() == 42 && &raw[0..2] == b"Ed"
}

#[test]
fn valid_minisign_pubkey_rejects_placeholders_and_garbage() {
    let b64 = |s: &[u8]| base64::engine::general_purpose::STANDARD.encode(s);
    let wrap = |text: &str| b64(text.as_bytes());
    // 负例：占位串（非法 base64）
    assert!(!valid_minisign_pubkey(
        "REPLACE_WITH_TAURI_SIGNING_PUBKEY_AT_RELEASE"
    ));
    // 解码后无换行 / 注释不对 / 内层公钥体非法
    assert!(!valid_minisign_pubkey(&wrap("single-line-no-newline")));
    assert!(!valid_minisign_pubkey(&wrap(
        "untrusted comment: ssh public key: X\nAAAA\n"
    )));
    assert!(!valid_minisign_pubkey(&wrap(
        "untrusted comment: minisign public key: X\n!!!!\n"
    )));
    // 42 字节但密钥类型头错（全零 ≠ "Ed"）
    assert!(!valid_minisign_pubkey(&wrap(&format!(
        "untrusted comment: minisign public key: X\n{}\n",
        b64(&[0u8; 42])
    ))));
    // 头对但长度错
    assert!(!valid_minisign_pubkey(&wrap(&format!(
        "untrusted comment: minisign public key: X\n{}\n",
        b64(b"Ed")
    ))));
}

#[test]
fn updater_pubkey_is_a_real_minisign_key() {
    let cfg = config();
    let pubkey = cfg["plugins"]["updater"]["pubkey"]
        .as_str()
        .expect("updater.pubkey 应为字符串");
    assert!(
        valid_minisign_pubkey(pubkey),
        "D-28：updater 公钥必须是真实 minisign ed25519 密钥，不得保留占位串"
    );
    let endpoints = cfg["plugins"]["updater"]["endpoints"]
        .as_array()
        .expect("endpoints 应为数组");
    assert_eq!(endpoints.len(), 1);
    assert!(endpoints[0]
        .as_str()
        .unwrap()
        .starts_with("https://github.com/"));
}

#[test]
fn bundle_targets_ship_nsis_and_msi() {
    let cfg = config();
    let targets: Vec<&str> = cfg["bundle"]["targets"]
        .as_array()
        .expect("targets 应为数组")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        targets.contains(&"nsis"),
        "DESIGN §7 双格式缺一不可: {targets:?}"
    );
    assert!(
        targets.contains(&"msi"),
        "DESIGN §7 双格式缺一不可: {targets:?}"
    );
    assert_eq!(
        cfg["bundle"]["createUpdaterArtifacts"].as_bool(),
        Some(true),
        "签名更新工件必须随构建产出"
    );
    // MSI 打包硬性要求 .ico（无 bundle.icon 时 light.exe 直接报 Couldn't find a .ico icon，实测教训）
    let icons: Vec<&str> = cfg["bundle"]["icon"]
        .as_array()
        .expect("bundle.icon 必须显式配置")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        icons.contains(&"icons/icon.ico"),
        "NSIS/MSI 图标缺失则双格式不可交付: {icons:?}"
    );
    assert!(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("icons/icon.ico")
            .exists(),
        "声明的 .ico 文件必须真实存在"
    );
    // WiX culture 只认 BCP-47（zh-CN/en-US），NSIS 式名称（SimpChinese）会让 bundler 直接 panic（实测 mod.rs:810）
    for lang in cfg["bundle"]["windows"]["wix"]["language"]
        .as_array()
        .expect("wix.language 应为数组")
    {
        let l = lang.as_str().unwrap();
        let bytes = l.as_bytes();
        let bcp47 = bytes.len() == 5
            && bytes[0..2].iter().all(u8::is_ascii_lowercase)
            && bytes[2] == b'-'
            && bytes[3..5].iter().all(u8::is_ascii_uppercase);
        assert!(
            bcp47,
            "wix.language 必须是 BCP-47 区域名，实测 SimpChinese 直接 panic: {l}"
        );
    }
}

/// CSP 判定：必需指令齐全且无 'unsafe-eval'（纯函数自带负例）
fn csp_ok(csp: &str) -> bool {
    if csp.contains("unsafe-eval") {
        return false;
    }
    let mut directives = csp.split(';').map(str::trim).collect::<Vec<_>>();
    directives.sort();
    let required = [
        "connect-src",
        "default-src",
        "font-src",
        "img-src",
        "script-src",
        "style-src",
        "worker-src",
    ];
    let present: Vec<String> = directives
        .iter()
        .map(|d| d.split_whitespace().next().unwrap_or_default().to_string())
        .collect();
    required.iter().all(|r| present.iter().any(|p| p == r))
        && directives
            .iter()
            .any(|d| d.starts_with("default-src") && d.contains("'self'"))
        && directives
            .iter()
            .any(|d| d.starts_with("img-src") && d.contains("data:"))
        && directives.iter().any(|d| {
            d.starts_with("connect-src") && d.contains("ipc:") && d.contains("http://ipc.localhost")
        })
}

#[test]
fn csp_predicate_itself_can_fail() {
    assert!(!csp_ok("default-src 'self'")); // 缺必需指令
    assert!(!csp_ok(
        "default-src 'self' 'unsafe-eval'; style-src 'self'; img-src 'self' data:; \
         font-src 'self'; worker-src 'self'; connect-src 'self' ipc: http://ipc.localhost; \
         script-src 'self'"
    )); // unsafe-eval 一票否决
    assert!(csp_ok(
        "default-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; \
         worker-src 'self'; connect-src 'self' ipc: http://ipc.localhost; script-src 'self'"
    ));
}

#[test]
fn csp_is_landed_in_config() {
    let cfg = config();
    let csp = cfg["app"]["security"]["csp"]
        .as_str()
        .expect("D-28：csp 不得为 null");
    assert!(csp_ok(csp), "CSP 指令集不完整: {csp}");
}

fn load_capabilities() -> Vec<(String, serde_json::Value)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("capabilities 目录应存在") {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        out.push((
            name,
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap(),
        ));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn defined_permission_identifiers() -> BTreeSet<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("permissions");
    let mut ids = BTreeSet::new();
    for entry in
        std::fs::read_dir(dir).expect("D-28：permissions 目录必须存在（app ACL 全量强制的开关）")
    {
        let src = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        let doc: toml::Value = src.parse().expect("permissions/*.toml 应为合法 TOML");
        for perm in doc["permission"].as_array().expect("[[permission]] 数组") {
            ids.insert(
                perm["identifier"]
                    .as_str()
                    .expect("identifier 必填")
                    .to_string(),
            );
        }
    }
    ids
}

/// lib.rs generate_handler 注册表 = 命令面的唯一事实来源
fn registered_commands() -> Vec<String> {
    let lib = read("src/lib.rs");
    let start = lib
        .find("generate_handler![")
        .expect("invoke_handler 注册块必须存在");
    let block = &lib[start..];
    let end = block.find("])").expect("注册块应闭合");
    block[..end]
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            t.strip_prefix("commands::")
                .and_then(|rest| rest.split(',').next())
                .map(str::to_string)
        })
        .collect()
}

fn kebab(cmd: &str) -> String {
    cmd.replace('_', "-")
}

#[test]
fn every_registered_command_is_acl_declared_and_main_granted() {
    let defined = defined_permission_identifiers();
    let caps = load_capabilities();
    let main = &caps
        .iter()
        .find(|(n, _)| n == "main")
        .expect("main capability 必须存在")
        .1;
    let main_perms: BTreeSet<String> = main["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let cmds = registered_commands();
    assert!(cmds.len() > 150, "注册表解析异常（得到 {} 条）", cmds.len());
    for cmd in &cmds {
        let id = format!("allow-{}", kebab(cmd));
        assert!(
            defined.contains(&id),
            "命令 {cmd} 缺 allow-* 权限定义（D-28 默认全拒要求全量声明）"
        );
        assert!(
            main_perms.contains(&id),
            "命令 {cmd} 未进入 main capability 白名单"
        );
    }
}

#[test]
fn capabilities_are_split_per_window_and_reference_only_defined_permissions() {
    let defined = defined_permission_identifiers();
    let caps = load_capabilities();
    let names: Vec<&str> = caps.iter().map(|(n, _)| n.as_str()).collect();
    for expect in [
        "main",
        "quickpanel",
        "overlay",
        "pin",
        "launcher",
        "notebar",
    ] {
        assert!(
            names.contains(&expect),
            "缺 {expect} capability（D-28 按窗拆分）"
        );
    }
    for (name, cap) in &caps {
        let windows = cap["windows"].as_array().unwrap();
        assert!(
            windows.len() <= 2,
            "capability {name} 覆盖窗标签过多（{windows:?}），单文件扁平授权正是 D-28 消灭的形态"
        );
        assert!(cap["identifier"].as_str() == Some(name.as_str()));
        for perm in cap["permissions"].as_array().unwrap() {
            let p = perm.as_str().unwrap();
            if p.starts_with("core:") {
                continue;
            }
            assert!(
                defined.contains(p),
                "capability {name} 引用未定义权限 {p}（tauri-build 同样会失败）"
            );
        }
    }
}

#[test]
fn aux_windows_never_reach_main_only_commands() {
    let caps = load_capabilities();
    let banned = [
        "allow-vault-create",
        "allow-vault-unlock",
        "allow-winops-apply",
        "allow-kvm-pair-with",
        "allow-sys-clean-execute",
        "allow-term-ssh-connect",
        "allow-file-enqueue",
    ];
    for (name, cap) in &caps {
        if name == "main" {
            continue;
        }
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for b in banned {
            assert!(
                !perms.contains(&b),
                "辅助窗 capability {name} 不得持有 main-only 权限 {b}（跨窗攻击面收敛是 D-28 的核心负例）"
            );
        }
    }
    // 形状钉死：quickpanel 只可能触达 clipboard 三面 + 宿主日志/配色
    let qp = &caps.iter().find(|(n, _)| n == "quickpanel").unwrap().1;
    let qp_perms: Vec<&str> = qp["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for expect in [
        "allow-clipboard-search",
        "allow-clipboard-paste",
        "allow-clipboard-get-image",
    ] {
        assert!(qp_perms.contains(&expect), "quickpanel 缺正向授权 {expect}");
    }
    assert_eq!(
        qp_perms.len(),
        8,
        "quickpanel 权限面只增不减，扩张须显式改此断言"
    );
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantProxyKernelSelect() {
    // T-B2-2 红线负例：换核命令可停/起内核进程，只允许主窗代理页触达
    let caps = load_capabilities();
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&"allow-proxy-kernel-select");
            continue;
        }
        assert!(
            !perms.contains(&"allow-proxy-kernel-select"),
            "辅助窗 capability {name} 不得持有 allow-proxy-kernel-select（内核生命周期是 main-only 面）"
        );
    }
    assert!(
        main_granted,
        "正对照：main 必须持有 allow-proxy-kernel-select，否则本负例是空洞"
    );
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantProxyKernelRestart() {
    // T-B2-3 红线负例：重启命令同样停/起内核进程，只允许主窗代理页触达
    let caps = load_capabilities();
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&"allow-proxy-kernel-restart");
            continue;
        }
        assert!(
            !perms.contains(&"allow-proxy-kernel-restart"),
            "辅助窗 capability {name} 不得持有 allow-proxy-kernel-restart（内核生命周期是 main-only 面）"
        );
    }
    assert!(
        main_granted,
        "正对照：main 必须持有 allow-proxy-kernel-restart，否则本负例是空洞"
    );
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantProxyRulesV2() {
    // T-B2-9 红线负例：分流规则 v2 读写（可改全局出口路由）只允许主窗代理页触达
    let caps = load_capabilities();
    for ident in ["allow-proxy-rules-get", "allow-proxy-rules-set"] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（分流路由面是 main-only）"
            );
        }
        assert!(
            main_granted,
            "正对照：main 必须持有 {ident}，否则本负例是空洞"
        );
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantProxyNodeSelectAndEgressProbe() {
    // T-B2-11 红线负例：手动选出口会停/起内核并改写全局路由，出口自检触网——
    // 三者（node-select/node-auto/egress-probe）均为 main-only 面
    let caps = load_capabilities();
    for ident in [
        "allow-proxy-node-select",
        "allow-proxy-node-auto",
        "allow-proxy-egress-probe",
    ] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（出口选择/自检是 main-only）"
            );
        }
        assert!(
            main_granted,
            "正对照：main 必须持有 {ident}，否则本负例是空洞"
        );
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §5.2）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantProxyArtifactInstall() {
    // T-B2-10 红线负例：geo 数据资产安装会向 proxy 目录写盘并触网下载，main-only
    let caps = load_capabilities();
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&"allow-proxy-artifact-install");
            continue;
        }
        assert!(
            !perms.contains(&"allow-proxy-artifact-install"),
            "辅助窗 capability {name} 不得持有 allow-proxy-artifact-install（二进制/数据写盘通道是 main-only 面）"
        );
    }
    assert!(
        main_granted,
        "正对照：main 必须持有 allow-proxy-artifact-install，否则本负例是空洞"
    );
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §8.2）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantClipboardCapture() {
    // T-B3-2 红线负例：暂停/恢复捕获改写运行态与持久配置（决定新内容是否入库），
    // 只允许主窗剪切板页触达；quickpanel 等辅助窗连读口都不给
    let caps = load_capabilities();
    for ident in ["allow-clipboard-capture-get", "allow-clipboard-capture-set"] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（捕获开关是 main-only 面）"
            );
        }
        assert!(
            main_granted,
            "正对照：main 必须持有 {ident}，否则本负例是空洞"
        );
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §8.2）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantClipboardStack() {
    // T-B3-3 红线负例：堆栈投递会写系统剪贴板并向焦点应用注入 Ctrl+V（等同代打），
    // 七命令全为 main-only；quickpanel 只留既有 search/paste/get-image 三面
    let caps = load_capabilities();
    for ident in [
        "allow-clipboard-stack-push",
        "allow-clipboard-stack-list",
        "allow-clipboard-stack-move",
        "allow-clipboard-stack-remove",
        "allow-clipboard-stack-clear",
        "allow-clipboard-stack-paste-next",
        "allow-clipboard-stack-paste-all",
    ] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（堆栈投递面是 main-only）"
            );
        }
        assert!(
            main_granted,
            "正对照：main 必须持有 {ident}，否则本负例是空洞"
        );
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §8.2）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantClipboardGroupsAndSuggestions() {
    // T-B3-4 红线负例：分组写口会改用户自己起的组名并驱动「采纳」落库，
    // 六命令全为 main-only（辅助窗只读事件流，不持写面）
    let caps = load_capabilities();
    for ident in [
        "allow-clipboard-entry-set-group",
        "allow-clipboard-group-rename",
        "allow-clipboard-group-delete",
        "allow-clipboard-suggestions",
        "allow-clipboard-suggestion-apply",
        "allow-clipboard-stats",
    ] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（分组与建议写面是 main-only）"
            );
        }
        assert!(
            main_granted,
            "正对照：main 必须持有 {ident}，否则本负例是空洞"
        );
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-5）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantClipboardSecretReveal() {
    // T-B3-5 红线负例：揭示口是敏感明文的唯一出口，且成功即写审计日志——
    // quickpanel / launcher / notebar / overlay / pin 五窗永不得持有（辅助窗可被
    // 全局热键在任意上下文呼出，等于把明文送到无人看管的界面上）。
    let caps = load_capabilities();
    let ident = "allow-clipboard-secret-reveal";
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&ident);
            continue;
        }
        assert!(
            !perms.contains(&ident),
            "辅助窗 capability {name} 不得持有 {ident}（明文揭示口是 main-only）"
        );
    }
    assert!(main_granted, "正对照：main 必须持有 {ident}");
    for window in ["quickpanel", "launcher", "notebar", "overlay", "pin"] {
        assert!(
            caps.iter().any(|(name, _)| name == window),
            "负例须覆盖的辅助窗 {window} 不在册，本断言会空洞"
        );
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-8）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantClipboardHtmlGet() {
    // T-B3-8 红线负例：HTML 源文读口把最多 512KB 的正文原样交给调用方，
    // 辅助窗（热键可在任意上下文呼出）永不得持有——列表侧只有 has_html 布尔，
    // 正文口是 main-only。clipboard_paste 虽在 quickpanel 在册（八条冻结未破），
    // 它只把内容投进系统剪贴板、不把 HTML 回给 webview，两回事。
    let caps = load_capabilities();
    let ident = "allow-clipboard-html-get";
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&ident);
            continue;
        }
        assert!(
            !perms.contains(&ident),
            "辅助窗 capability {name} 不得持有 {ident}（HTML 正文读口是 main-only）"
        );
    }
    assert!(main_granted, "正对照：main 必须持有 {ident}");
    for window in ["quickpanel", "launcher", "notebar", "overlay", "pin"] {
        assert!(
            caps.iter().any(|(name, _)| name == window),
            "负例须覆盖的辅助窗 {window} 不在册，本断言会空洞"
        );
    }
    // quickpanel 权限面冻结在 8 条（D-28 判据）：本枚新命令不得把它撑大
    let qp = caps
        .iter()
        .find(|(name, _)| name == "quickpanel")
        .map(|(_, cap)| cap["permissions"].as_array().unwrap().len())
        .expect("quickpanel capability 在册");
    assert_eq!(qp, 8, "quickpanel 权限面须停在 8 条，多一条即破 D-28 冻结");
}

/// T-B3-9 红线负例：导出=整库批量外发（口令面），导入=整库改写。
/// 两枚都 main-only，辅助窗永不持有；quickpanel 八条冻结随批再钉一次。
#[test]
#[allow(non_snake_case)] // 任务书（09 §8.2 T-B3-9）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantClipboardBackup() {
    let caps = load_capabilities();
    for ident in ["allow-clipboard-export", "allow-clipboard-import"] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（批量外发/整库改写是 main-only）"
            );
        }
        assert!(main_granted, "正对照：main 必须持有 {ident}");
    }
    for window in ["quickpanel", "launcher", "notebar", "overlay", "pin"] {
        assert!(
            caps.iter().any(|(name, _)| name == window),
            "负例须覆盖的辅助窗 {window} 不在册，本断言会空洞"
        );
    }
    let qp = caps
        .iter()
        .find(|(name, _)| name == "quickpanel")
        .map(|(_, cap)| cap["permissions"].as_array().unwrap().len())
        .expect("quickpanel capability 在册");
    assert_eq!(qp, 8, "quickpanel 权限面须停在 8 条，多一条即破 D-28 冻结");
}

/// T-B4-10 红线负例：`ocr_config_get` 读的是用户偏好语言/引擎（设置内容）。
/// 覆盖层要语言由后端在请求路径内按配置解析（engine::resolve_langs），不读设置 →
/// overlay 等六窗永不持有，main 正对照防空洞。
#[test]
#[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-10）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantOcrConfigGet() {
    let caps = load_capabilities();
    let ident = "allow-ocr-config-get";
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&ident);
            continue;
        }
        assert!(
            !perms.contains(&ident),
            "辅助窗 capability {name} 不得持有 {ident}（设置读面是 main-only）"
        );
    }
    assert!(
        caps.iter().any(|(name, _)| name == "overlay"),
        "负例须覆盖 overlay，否则本断言空洞"
    );
    assert!(main_granted, "正对照：main 必须持有 {ident}");
}

/// T-B4-12 红线负例：`ocr_export` 是批量识别文本的**出盘面**（落 `{app_data}/export/`）。
/// 覆盖层虽可识别（allow-ocr-recognize 在册），但不持有把整批文字写进磁盘的口子；
/// 六辅助窗永不，main 正对照防空洞。
#[test]
#[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-12）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantOcrExport() {
    let caps = load_capabilities();
    let ident = "allow-ocr-export";
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&ident);
            continue;
        }
        assert!(
            !perms.contains(&ident),
            "辅助窗 capability {name} 不得持有 {ident}（批量文本出盘是 main-only）"
        );
    }
    assert!(
        caps.iter().any(|(name, _)| name == "overlay"),
        "负例须覆盖 overlay（它有识别权，看出识别≠出盘），否则本断言空洞"
    );
    assert!(main_granted, "正对照：main 必须持有 {ident}");
}

/// T-B4-6 红线负例：`screenshot_beautify_apply` 是**历史内容批量出盘面**（读任意一条
/// 历史字节 → 美化 → 落盘/上剪贴板）。覆盖层只有当场完成的口子（allow-screenshot-finish），
/// 不得回头翻历史；六辅助窗永不，main 正对照防空洞。
#[test]
#[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-6）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantScreenshotBeautifyApply() {
    let caps = load_capabilities();
    let ident = "allow-screenshot-beautify-apply";
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&ident);
            continue;
        }
        assert!(
            !perms.contains(&ident),
            "辅助窗 capability {name} 不得持有 {ident}（历史内容出盘是 main-only）"
        );
    }
    assert!(
        caps.iter().any(|(name, _)| name == "overlay"),
        "负例须覆盖 overlay（它能完成截图但翻不到历史），否则本断言空洞"
    );
    assert!(main_granted, "正对照：main 必须持有 {ident}");
}

/// html,body 基底块是否同时携带 background:transparent 与 margin:0（纯函数，自带负例）。
fn transparent_base_ok(css: &str) -> bool {
    let norm: String = css.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(idx) = norm.find("html, body {") else {
        return false;
    };
    let block = match norm[idx..].split_once('}') {
        Some((b, _)) => b,
        None => return false,
    };
    block.contains("background: transparent") && block.contains("margin: 0")
}

#[test]
fn global_css_carries_transparent_window_base() {
    // D-28 发布后修复：页面基底透明规则从 index.html 内联 <style> 迁到链接 CSS。
    // 内联 <style> 会让打包期 Tauri 给 style-src 注入 nonce,进而使 'unsafe-inline' 失效、
    // 拒掉 Fluent UI 运行时注入样式（安装包 UI 全乱）。规则本身必须仍随 global.css 送达。
    assert!(
        !transparent_base_ok("body { margin: 0; }"),
        "判定函数必须可否例"
    );
    assert!(
        !transparent_base_ok("html, body { color: red; }"),
        "缺声明应判否"
    );
    assert!(transparent_base_ok(
        "html, body {\n background: transparent;\n margin: 0;\n}"
    ));
    assert!(
        transparent_base_ok(&read("../src/styles/global.css")),
        "global.css 必须携带 html,body 基底透明规则（U1-4，勿改回 index.html 内联 <style>）"
    );
}

#[test]
fn dead_permission_surface_is_trimmed() {
    let caps = load_capabilities();
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // updater 纯 Rust 端消费，JS 无 @tauri-apps/plugin-* 调用；set-always-on-top 全仓无调用点
        assert!(
            !perms.contains(&"updater:default"),
            "capability {name} 不应再携带 updater:default"
        );
        assert!(
            !perms.contains(&"core:window:allow-set-always-on-top"),
            "capability {name} 不应再携带无调用点的 set-always-on-top"
        );
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-4）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantScreenshotWindows() {
    // T-B4-4 红线负例：**窗口标题列举是跨应用隐私面**。截图覆盖层/贴图/快捷面板等辅助窗
    // 没有列他人窗口标题的理由；main-only + 正对照防空洞（本负例若 main 也没拿到就恒真）
    let caps = load_capabilities();
    let ident = "allow-screenshot-windows";
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&ident);
            continue;
        }
        assert!(
            !perms.contains(&ident),
            "辅助窗 capability {name} 不得持有 {ident}（窗口标题枚举是 main-only 隐私面）"
        );
    }
    assert!(
        main_granted,
        "正对照：main 必须持有 {ident}，否则本负例是空洞"
    );
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-5）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantScreenshotScroll() {
    // T-B4-5 红线：**抓帧会话只能由发起它的那两个窗驱动**。滚动会话每一步都是一次
    // 全屏 GDI 抓取（EnumWindows 那张表里的每个窗都会被读进去），贴图窗/快捷面板/
    // 启动器/便签条手里都没有正在进行的截图，却可以凭一个 id 让宿主反复截屏。
    //
    // 白名单取 {main, overlay} 而不是任务书括号的"main-only"，是因为"滚动"那颗钮
    // 长在覆盖层的编辑阶段上——覆盖层本来就是 confirm/discard/finish 的持有者，
    // 把 scroll 只给 main 会让这条通路在唯一的调用窗上直接 BadState。
    // 正对照因此落在 overlay 上（它没拿到的话本负例就是空洞）。
    let caps = load_capabilities();
    let idents = [
        "allow-screenshot-scroll-begin",
        "allow-screenshot-scroll-append",
        "allow-screenshot-scroll-finish",
        "allow-screenshot-scroll-discard",
    ];
    let mut overlay_granted = 0;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" || name == "overlay" {
            if name == "overlay" {
                overlay_granted = idents.iter().filter(|i| perms.contains(i)).count();
            }
            continue;
        }
        for ident in idents {
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（无会话在手却能驱动逐帧截屏）"
            );
        }
    }
    assert_eq!(
        overlay_granted,
        idents.len(),
        "正对照：overlay 必须四枚齐备（缺一枚则滚动条在那条通路上半路 BadState，本负例也变空洞）"
    );
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-9）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantScreenshotUpload() {
    // T-B4-9 红线（§9.1-⑩）：**数据出机面**。上传会把用户刚截的屏幕内容发到外部端点，
    // 而贴图窗/快捷面板/启动器/便签条上没有任何"把它传出去"的入口——给它们这一枚，
    // 等于给任何一个后来长在那些窗上的小按钮开了一条外发通道。
    //
    // 与 scroll 那枚的差别是刻意的：覆盖层**不**在本白名单里。上传发生在完成之后
    // （覆盖层已经关掉），唯一的调用点是主窗口的截图面板那一行。
    let caps = load_capabilities();
    let idents = ["allow-screenshot-upload", "allow-screenshot-upload-targets"];
    let mut main_granted = 0;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = idents.iter().filter(|i| perms.contains(i)).count();
            continue;
        }
        for ident in idents {
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（数据出机面 main-only）"
            );
        }
    }
    assert_eq!(
        main_granted,
        idents.len(),
        "正对照：main 必须两枚齐备，否则本负例对任何配置都成立=空洞"
    );
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-14）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantScreenshotHistoryDelete() {
    // T-B4-14 红线：**删除面辅助窗永不**。`screenshot_history_delete` 一步就同时抹掉
    // 历史行和磁盘上那张图，不进回收站、不可撤销。贴图窗/快捷面板/启动器/便签条/
    // 覆盖层上都没有"这条不要了"的入口——给它们这一枚，等于让任何一个后来长在
    // 那些窗上的小按钮拥有不可逆删除。唯一的调用点是主窗口截图面板的溢出菜单。
    let caps = load_capabilities();
    let ident = "allow-screenshot-history-delete";
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&ident);
            continue;
        }
        assert!(
            !perms.contains(&ident),
            "辅助窗 capability {name} 不得持有 {ident}（不可逆删除面 main-only）"
        );
    }
    assert!(
        main_granted,
        "正对照：main 必须持有 {ident}，否则本负例是空洞"
    );
}

/// T-B5-2 红线负例：冲突历史读的是用户笔记内容快照，恢复会把旧内容重新入流并推给对端
/// （改写数据集 + 产生新变更）。覆盖层/快速面板等六辅助窗永不持有，main 正对照防空洞。
#[test]
#[allow(non_snake_case)] // 任务书（09 §10.2 T-B5-2）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantSyncConflictsAndRestore() {
    let caps = load_capabilities();
    for ident in ["allow-sync-conflicts-get", "allow-sync-conflict-restore"] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（冲突快照读与恢复是 main-only）"
            );
        }
        assert!(
            main_granted,
            "正对照：main 必须持有 {ident}，否则本负例是空洞"
        );
    }
}

/// T-B5-3 红线负例：同步流水是运维诊断面（对端身份 + 逐轮推送/拉取量 + 失败原因原文，
/// 握手前失败的行 peer 列还带着对端 socket 地址）。六个辅助窗都没有"看同步历史"的入口，
/// 给它们这一枚 = 让任意窗口获得读用户同步行为的能力。main 正对照防空洞。
#[test]
#[allow(non_snake_case)] // 任务书（09 §10.2 T-B5-3）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantSyncRuns() {
    let caps = load_capabilities();
    let ident = "allow-sync-runs-get";
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&ident);
            continue;
        }
        assert!(
            !perms.contains(&ident),
            "辅助窗 capability {name} 不得持有 {ident}（同步活动流水 main-only）"
        );
    }
    assert!(
        main_granted,
        "正对照：main 必须持有 {ident}，否则本负例是空洞"
    );
}

/// T-B5-6 红线负例：`sync_set_paused` 写的就是内核那枚暂停位——它决定"用户的笔记还要不要
/// 继续往外发"。六个辅助窗（快速面板/覆盖层等）没有任何同步入口，给它们这一枚 = 任意窗口
/// 都能悄悄把出账闸门掐掉或放开，而用户在主窗口看到的徽章还以为是自己在管。main 正对照防空洞。
#[test]
#[allow(non_snake_case)] // 任务书（09 §10.2 T-B5-6）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantSyncSetPaused() {
    let caps = load_capabilities();
    let ident = "allow-sync-set-paused";
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&ident);
            continue;
        }
        assert!(
            !perms.contains(&ident),
            "辅助窗 capability {name} 不得持有 {ident}（出账闸门 main-only）"
        );
    }
    assert!(
        main_granted,
        "正对照：main 必须持有 {ident}，否则本负例是空洞"
    );
}

/// T-B5-8 负例：`sync_datasets_get` 只读白名单，但它是**同步的可见面**——六个辅助窗
/// 一个同步入口都没有，多一处声明就多一处要人记的例外（快速面板 capability 的冻结断言
/// 因此必须一次未改）。这一枚要守的不是"谁能读"，而是"读到的那份清单不许散落到别处"。
#[test]
#[allow(non_snake_case)] // 任务书（09 §10.2 T-B5-8）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantSyncDatasetsGet() {
    let caps = load_capabilities();
    let ident = "allow-sync-datasets-get";
    let mut main_granted = false;
    for (name, cap) in &caps {
        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "main" {
            main_granted = perms.contains(&ident);
            continue;
        }
        assert!(
            !perms.contains(&ident),
            "辅助窗 capability {name} 不得持有 {ident}（数据集在册表 main-only）"
        );
    }
    assert!(
        main_granted,
        "正对照：main 必须持有 {ident}，否则本负例是空洞"
    );
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-1）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantRemoteProfileCommands() {
    // B6 T-B6-1 红线负例：档案 = 站点清单，会暴露内网主机名/端口/用户形状，
    // 三命令全为 main-only；辅助窗（热键可在任意上下文呼出）永不持有。
    let caps = load_capabilities();
    for ident in [
        "allow-file-remote-profiles",
        "allow-file-remote-profile-save",
        "allow-file-remote-profile-delete",
    ] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（远程档案面是 main-only）"
            );
        }
        assert!(
            main_granted,
            "正对照：main 必须持有 {ident}，否则本负例是空洞"
        );
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-6）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantRemotePresetsAndDrivers() {
    // B6 T-B6-6 红线负例：预设表暴露"用户可能连哪些站"的意图面，drivers 暴露
    // 已连接内网形状——两者 main-only。drivers 与 T-B6-3 负例有意重复登记：
    // 命令面红线在每行的门禁里各钉一枚，删测试要先删命令。
    let caps = load_capabilities();
    for ident in ["allow-file-remote-presets", "allow-file-remote-drivers"] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（远端预设/驱动面是 main-only）"
            );
        }
        assert!(
            main_granted,
            "正对照：main 必须持有 {ident}，否则本负例是空洞"
        );
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-3）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantRemoteConnectAndBrowse() {
    // B6 T-B6-3 红线负例：connect 携带逐次口令、browse 暴露远端目录树、
    // drivers 暴露内网主机形状——三命令全为 main-only，
    // 辅助窗（热键可在任意上下文呼出）永不持有。
    let caps = load_capabilities();
    for ident in [
        "allow-file-remote-connect",
        "allow-file-remote-browse",
        "allow-file-remote-drivers",
    ] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（远端连接面是 main-only）"
            );
        }
        assert!(
            main_granted,
            "正对照：main 必须持有 {ident}，否则本负例是空洞"
        );
    }
}

#[test]
#[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-5）字面测试名优先于 rustc 命名惯例
fn auxWindows_neverGrantRemoteFingerprintAck() {
    // B6 T-B6-5 红线负例：指纹确认写的是主机键信任表——信任决定比 connect
    // 更敏感；disconnect 退役远端会话，与连接面同谱。两命令全为 main-only。
    let caps = load_capabilities();
    for ident in [
        "allow-file-remote-fingerprint-ack",
        "allow-file-remote-disconnect",
    ] {
        let mut main_granted = false;
        for (name, cap) in &caps {
            let perms: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if name == "main" {
                main_granted = perms.contains(&ident);
                continue;
            }
            assert!(
                !perms.contains(&ident),
                "辅助窗 capability {name} 不得持有 {ident}（信任决定面是 main-only）"
            );
        }
        assert!(
            main_granted,
            "正对照：main 必须持有 {ident}，否则本负例是空洞"
        );
    }
}
