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
