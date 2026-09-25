# 02 · 安全问题详解

> 本文档覆盖 `SEC-01`…`SEC-22`，每条含：问题描述（含证据）→ 解决方案（实现步骤 + 代码示例 + 配置修改）→ 修复前后对比 → 预防措施。
> 证据分级见 [README §2](./README.md)。所有行号对应审查时工作区代码。
> 相关：`COR-02`（WASM 越界分配）、`COR-05`（信任根 fail-open）、`SEC-20/21/22` 属规范类安全项，见本文末。

---

# SEC-01 · 会话 AEAD 双向复用同一密钥与 nonce 计数器　`P0` `[实测]`

### 位置
- [crates/kvm-core/src/session.rs:274](../../crates/kvm-core/src/session.rs#L274)（`server_handshake`）
- [crates/kvm-core/src/session.rs:304](../../crates/kvm-core/src/session.rs#L304)（`client_handshake`）
- [crates/sync-core/src/transport.rs:175-176,205-206](../../crates/sync-core/src/transport.rs#L175-L176)（`SyncSession{ rx, tx }`）
- [crates/host-core/src/wire.rs:180-223](../../crates/host-core/src/wire.rs#L180-L223)（`FrameCipher`）

### 问题描述

握手完成后，两个方向各自构造一个 `FrameCipher`，但**传入的是同一把 key**：

```rust
// kvm-core/src/session.rs:272-274（服务端；客户端 :302-304 同形）
let key = session_key_from(identity, &peer_static, dh2, &peer.fingerprint)?;
let (rd, wr) = stream.into_split();
Ok((rd, wr, FrameCipher::new(key), FrameCipher::new(key), peer))
//            ^^^^^^^^^^^^^^^^^^  ^^^^^^^^^^^^^^^^^^  收发两个实例共用 key
```

而 `FrameCipher` 的 nonce 是**从 0 开始的单调计数器**（8B 小端 + 4B 零前缀）：

```rust
// host-core/src/wire.rs:186-191
pub fn new(key: [u8; 32]) -> Self { Self { cipher: ChaCha20Poly1305::new(Key::from_slice(&key)), counter: 0 } }

// host-core/src/wire.rs:217-222
fn next_nonce(&mut self) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&self.counter.to_le_bytes());
    self.counter = self.counter.wrapping_add(1);
    nonce
}
```

于是 **A→B 的第 1 帧与 B→A 的第 1 帧使用完全相同的 (key, nonce)**，第 2 帧同理。

`open()`（[:208-215](../../crates/host-core/src/wire.rs#L208-L215)）直接用帧里携带的 nonce 调 `decrypt`，**不校验序号是否等于期望的接收计数**，因此旧帧可被原样重放。

同一缺陷存在于同步通道：[transport.rs:175-176](../../crates/sync-core/src/transport.rs#L175-L176) 与 [:205-206](../../crates/sync-core/src/transport.rs#L205-L206) 也是 `rx: FrameCipher::new(key)` / `tx: FrameCipher::new(key)`。

> 注意：既有测试 [wire.rs:283-285](../../crates/host-core/src/wire.rs#L283-L285) 与 [session.rs:634-635](../../crates/kvm-core/src/session.rs#L634-L635) 恰好也"用同一 key 造两个 cipher"，但它只验证单向往返，**结构上无法发现**该缺陷——这也是"测试全绿 ≠ 安全"的典型例子。

### 影响

ChaCha20-Poly1305 的 nonce 复用属于**密码学致命误用**（RFC 8439 §4）：

1. **机密性击穿（被动窃听）**：同 key 同 nonce 下两侧 keystream 相同 → 攻击者抓 `C1`（A→B）与 `C2`（B→A），`C1 ⊕ C2 = P1 ⊕ P2`。已知/可猜一侧明文（如协议头、心跳 `ping`、固定格式的输入事件帧）即可还原另一侧。
2. **完整性击穿（可伪造）**：同 (key, nonce) 意味着 Poly1305 的一次性密钥重复，攻击者可构造通过校验的伪造帧——足以注入**键鼠输入事件**、剪贴板内容、文件块。
3. **可重放**：`open()` 不校验序号，历史帧可无限重放（重放一次"复制密码"或一条删除指令）。
4. **波及范围**：KVM 会话承载遥控输入、剪贴板、文件传输；同步会话承载 oplog 与业务数据。二者均非"低价值信道"。

### 解决方案

**步骤 1：在 `host-core::wire` 增加方向化密钥派生（保留旧函数仅供文档兼容期使用）**

```rust
// crates/host-core/src/wire.rs
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

/// 会话密钥对：按方向分离，杜绝 (key, nonce) 复用。
/// info 后缀区分方向；salt 仍为双方指纹（绑定设备对），shared 为双 DH 拼接。
pub struct DirectionalKeys {
    pub initiator_to_responder: [u8; 32],
    pub responder_to_initiator: [u8; 32],
}

pub fn derive_directional_keys(shared: &[u8], salt_material: &[u8]) -> DirectionalKeys {
    let salt: [u8; 32] = Sha256::digest(salt_material).into();
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut i2r = [0u8; 32];
    let mut r2i = [0u8; 32];
    // 协议标识升版：v2（v1 已发布缺陷，见 SEC-01）
    hk.expand(b"nexusforge-kvm-v2/initiator", &mut i2r).expect("HKDF 扩展长度合法");
    hk.expand(b"nexusforge-kvm-v2/responder", &mut r2i).expect("HKDF 扩展长度合法");
    DirectionalKeys { initiator_to_responder: i2r, responder_to_initiator: r2i }
}
```

**步骤 2：在 `FrameCipher` 增加接收序号校验（防重放）**

```rust
// crates/host-core/src/wire.rs
pub struct FrameCipher { cipher: ChaCha20Poly1305, counter: u64 }

impl FrameCipher {
    /// 解密并校验序号：帧内 nonce 的计数必须等于期望接收序号，否则拒绝。
    pub fn open(&mut self, nonce: &[u8; 12], ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
        use chacha20poly1305::aead::Aead;
        let got = u64::from_le_bytes(nonce[4..].try_into().expect("8B"));
        if got != self.counter {
            return Err(AppError::module(
                "KVM_SESSION_006",
                format!("帧序号失序（期望 {} 收到 {got}）：疑似重放或丢帧", self.counter),
                Some("断开并重连该会话以重建密钥".into()),
            ));
        }
        let pt = self.cipher
            .decrypt(chacha20poly1305::Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| AppError::module("KVM_SESSION_005", "帧解密失败（密钥或序号不匹配）", None))?;
        self.counter = self.counter.wrapping_add(1);
        Ok(pt)
    }
}
```

> 若担心"顺序传输被中间丢弃导致误杀"，可放宽为"必须 `got >= counter`（拒绝重复/回退），并允许跳号"——但**禁止接受 `got < counter`**；跳号时把 `counter` 提升到 `got + 1`。

**步骤 3：握手处按角色分配方向（KVM）**

```rust
// crates/kvm-core/src/session.rs·server_handshake（服务端 = responder）
let keys = session_key_from_directional(identity, &peer_static, dh2, &peer.fingerprint)?;
let (rd, wr) = stream.into_split();
Ok((
    rd, wr,
    FrameCipher::new(keys.initiator_to_responder), // rx：收对方（initiator）方向
    FrameCipher::new(keys.responder_to_initiator), // tx：发本端（responder）方向
    peer,
))

// client_handshake（客户端 = initiator）
Ok((
    rd, wr,
    FrameCipher::new(keys.responder_to_initiator), // rx
    FrameCipher::new(keys.initiator_to_responder), // tx
    peer,
))
```

**步骤 4：同步通道同改**

```rust
// crates/sync-core/src/transport.rs（两处 Session 构造）
let keys = derive_directional_keys(&shared, &salt.concat());
let (rx_key, tx_key) = if is_initiator {
    (keys.responder_to_initiator, keys.initiator_to_responder)
} else {
    (keys.initiator_to_responder, keys.responder_to_initiator)
};
Ok(SyncSession { rd, wr, rx: FrameCipher::new(rx_key), tx: FrameCipher::new(tx_key), peer })
```

**步骤 5：泄漏旧密钥材料（可选但推荐）** —— 派生后将 `shared`/中间 secret 用 `zeroize` 清零；`key` 数组移入 `FrameCipher` 后不再保留副本。

**步骤 6：协议版本协商** —— `Hello` 帧携带 `proto_ver`；`v1` 对端一律拒连并给出可操作 hint（"对端版本过旧，请同步升级"），避免"升级一半仍用旧密钥方案"。

**步骤 7：回归测试（必须包含"先失败"证明）**

```rust
#[test]
fn directional_keys_differ() {
    let k = derive_directional_keys(&[9u8; 64], b"fpAfpB");
    assert_ne!(k.initiator_to_responder, k.responder_to_initiator, "方向密钥必须不同");
}

#[test]
fn same_counter_different_direction_yields_distinct_keystream() {
    let k = derive_directional_keys(&[9u8; 64], b"fpAfpB");
    let mut a = FrameCipher::new(k.initiator_to_responder);
    let mut b = FrameCipher::new(k.responder_to_initiator);
    let (na, ca) = a.seal(b"AAAAAAAA").unwrap();  // 两侧第 1 帧，counter 均为 0
    let (nb, cb) = b.seal(b"AAAAAAAA").unwrap();
    assert_eq!(na, nb, "序号相同是允许的——安全性必须来自密钥不同");
    assert_ne!(ca, cb, "同明文同序号必须产生不同密文");
}

#[test]
fn replayed_frame_is_rejected() {
    let mut tx = FrameCipher::new([3u8; 32]);
    let mut rx = FrameCipher::new([3u8; 32]);
    let (n, c) = tx.seal(b"op").unwrap();
    assert!(rx.open(&n.into(), &c).is_ok());
    assert!(rx.open(&n.into(), &c).is_err(), "重放必须被拒");
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 方向密钥 | 收发同一 `[u8;32]` | HKDF info 分离为 `initiator`/`responder` 两把 |
| nonce 空间 | 两方向 0,1,2… 完全重叠 | 两方向 key 不同 → (key, nonce) 对全局唯一 |
| 被动窃听 | `C1⊕C2` 恢复明文，机密性失效 | 无同 (key,nonce) 流，机密性恢复 |
| 帧伪造 | Poly1305 一次性密钥重复 → 可伪造 | 每 (key,nonce) 唯一 → 伪造需破 AEAD |
| 重放 | `open()` 不校验序号，任意重放 | `got != counter` 即拒（或拒回退） |
| 兼容性 | — | 需升 `proto_ver=2`；v1 对端拒连并提示升级 |
| 测试 | 用同 key 造两 cipher 的用例无法暴露缺陷 | 新增 3 个方向性/重放负例 |

### 预防措施

1. **把"AEAD 使用规则"写成不可绕过的封装**：`FrameCipher::new` 改为 `FrameCipher::pair(keys, Role)`，从类型上禁止"两方向同 key"；或干脆把 two-way 会话封装成 `SecureChannel::establish(..., Role)`，调用方拿不到裸 key。
2. **协议安全评审清单**（进 `CONTRIBUTING.md` 或 `docs/`）：任何 AEAD 必须回答"nonce 唯一性由谁保证、是否校验序号、密钥是否方向分离、是否绑定协议版本"。
3. **CI 增加密码学负例门禁**：在新会话实现处强制存在"同明文同序号密文必异"与"重放必拒"两个测试（可用 `grep` 断言测试名存在，防止重构时被删）。
4. **升级一次就必须全链路**：`derive_session_key` 旧函数若保留，标注 `#[deprecated]` 并在 `DECISIONS.md` 登记"v1 会话密钥方案作废（安全）"，避免新代码误用。
5. **把该缺陷登记到 `DECISIONS.md`** 并注明影响面（KVM + sync），供后续 threat model 复用。

---

# SEC-02 · 提权 Helper 的 `exec` 参数透传 与 用户可写的外部 catalog　`P1` `[实测]+[框架]`

### 位置
- [src-tauri/src/commands/winops.rs:35,47](../../src-tauri/src/commands/winops.rs#L35)（`load_catalog(Some(&external))`）
- [crates/sys-core/src/winops.rs:241-259](../../crates/sys-core/src/winops.rs#L241-L259)（`merge_tweaks` / `load_catalog`）
- [crates/win-integration/src/maintenance.rs:26,60-74](../../crates/win-integration/src/maintenance.rs#L60-L74)（`EXEC_ALLOWLIST` 与 `args.to_vec()`）
- [crates/nexusforge-helper/src/dispatch.rs:172-189](../../crates/nexusforge-helper/src/dispatch.rs#L172-L189)（`exec` 分发）
- [src-tauri/src/winops_helper.rs:289-295](../../src-tauri/src/winops_helper.rs#L289-L295)（`helper_call("exec", json!({"program","args",...}))`）

### 问题描述

三条事实叠加，构成"用户级进程 → 提权任意操作"的通路：

**(1) catalog 来自用户可写目录，且同 id 覆盖内置项**

```rust
// src-tauri/src/commands/winops.rs:35
let external = state.app_data_dir.join("winops").join("catalog");   // %APPDATA%\com.nexusforge.app\winops\catalog
let tweaks = sys_core::winops::load_catalog(Some(&external))?;

// crates/sys-core/src/winops.rs:241-246 —— 外置同 id 直接替换内置
fn merge_tweaks(mut base: Vec<Tweak>, extra: Vec<Tweak>) -> Vec<Tweak> {
    for t in extra {
        match base.iter_mut().find(|b| b.id == t.id) {
            Some(slot) => *slot = t,      // ← 覆盖
            None => base.push(t),          // ← 或新增
        }
    }
    base
}
```
（该行为甚至被测试固定：`catalog_external_overrides_by_id`，[winops.rs:1258](../../crates/sys-core/src/winops.rs#L1258)。）

**(2) tweak 的动作参数被逐字送进提权进程**

```rust
// crates/nexusforge-helper/src/dispatch.rs:172-189
let program = req_str(&p, "program")?;
let args: Vec<String> = p["args"].as_array()...collect();   // 任意字符串原样
ops.maintenance.exec(&program, &args, timeout)
```

**(3) `win-integration` 侧只锁程序名，不锁参数** —— 而模块文档自己写明"禁止透传"

```rust
// crates/win-integration/src/maintenance.rs:6-7（文档承诺）
//! - exec：白名单程序执行（powercfg|dism|sfc|netsh|onedrive_uninstall）——spec §4.3
//!   安全约束：args 参数模板拼接禁止透传任意字符串；超时强杀；输出截断 256KB

// :26 只白名单程序名
const EXEC_ALLOWLIST: &[&str] = &["powercfg", "dism", "sfc", "netsh", "onedrive_uninstall"];

// :72-74 参数逐字透传
} else {
    (program.to_string(), args.to_vec())   // ← 与上面的文档承诺相矛盾
};
```

同类面还有 `dispatch.rs` 的 `registry.write`（[:85-92](../../crates/nexusforge-helper/src/dispatch.rs#L85-L92)）、`service.set_start/stop/start`（[:106-125](../../crates/nexusforge-helper/src/dispatch.rs#L106-L125)）：只要求 `key` 以 `HKCU\`/`HKLM\` 开头，**无键名/服务名白名单**。

### 影响

攻击场景（本地提权，无需内存漏洞）：

1. 攻击者（或诱导用户执行的"配置包"）在 `%APPDATA%\com.nexusforge.app\winops\catalog\evil.json` 写入一个 `id` 与内置 tweak 相同的条目，把动作改为 `program:"netsh", args:["advfirewall","set","allprofiles","state","off"]`。
2. 用户在 UI 中点击该 tweak 的"应用"，通过一次 UAC。
3. Helper 以提权身份执行：关闭防火墙、`dism /Online /Add-Package /PackagePath:\\attacker\share\x.cab`、`netsh` 改路由/DNS、`powercfg` 改电源策略、任意 HKLM 写入、任意服务启停、任意计划任务启停。

即：**提权进程的输入面从"服务端内置常量"变成了"用户可写文件"**，一次 UAC 换取提权任意代码执行（可进一步用 `dism` 安装驱动/包落地持久化）。

### 解决方案

分三层收口，**任一层单独不足**，建议全部落地。

**步骤 1（必须）：catalog 源不可被用户篡改 —— 外置目录禁止覆盖内置 id**

```rust
// crates/sys-core/src/winops.rs
fn merge_tweaks(base: Vec<Tweak>, extra: Vec<Tweak>) -> Vec<Tweak> {
    let mut merged = base;
    for t in extra {
        if merged.iter().any(|b| b.id == t.id) {
            tracing::warn!(id = %t.id, "外置 catalog 试图覆盖内置 tweak，已拒绝");
            continue;                      // ← 覆盖改为拒绝
        }
        merged.push(t);
    }
    merged
}
```
若产品确实需要"用户自定义 tweak"，则改为**只允许新增且 id 必须带 `user/` 前缀**，并在 `apply` 前用服务端签名的 catalog 校验（minisign，复用 updater 的密钥设施）：

```rust
// 外置目录：只接受签名文件
const CATALOG_SIG_SUFFIX: &str = ".json.minisig";
fn load_external_catalog(dir: &Path) -> SysResult<Vec<Tweak>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
        let sig = p.with_extension("json.minisig");
        let (Ok(bytes), Ok(sig_bytes)) = (std::fs::read(&p), std::fs::read(&sig)) else {
            tracing::warn!(path = %p.display(), "外置 catalog 缺签名，跳过");
            continue;
        };
        verify_minisign(&bytes, &sig_bytes, CATALOG_PUBKEY)
            .map_err(|e| SysError::WinOps(format!("catalog 签名校验失败: {e}")))?;
        out.extend(parse_catalog(&bytes)?);
    }
    Ok(out)
}
```

**步骤 2（必须）：Helper 端只接受"预置参数模板"，不再接受调用方 args**

方案 A（推荐，改动最干净）：把 IPC 从 `exec(program, args)` 改为 `exec_template(template_id)`，参数表由**编译进 helper 的常量**提供。

```rust
// crates/nexusforge-helper/src/dispatch.rs
/// 预置参数模板表：调用方只能选择模板，不能提供字符串。
const EXEC_TEMPLATES: &[(&str, &str, &[&str])] = &[
    // (template_id, program, args)
    ("defender.enable",   "powershell", &["-NoProfile","-NonInteractive","-Command",
        "Set-MpPreference -DisableRealtimeMonitoring $false"]),
    ("defender.disable",  "powershell", &["-NoProfile","-NonInteractive","-Command",
        "Set-MpPreference -DisableRealtimeMonitoring $true"]),
    ("dism.scan_health",  "dism", &["/Online","/Cleanup-Image","/ScanHealth"]),
    ("sfc.scannow",       "sfc",  &["/scannow"]),
    ("powercfg.high_perf","powercfg", &["-setactive","8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c"]),
    ("netsh.winsock_reset","netsh", &["winsock","reset"]),
    // …逐条穷举，新增即改代码 + 评审 + 测试
];

"exec_template" => {
    let id = req_str(&p, "template_id")?;
    let (_, program, args) = EXEC_TEMPLATES.iter().find(|(i,_,_)| *i == id)
        .ok_or_else(|| format!("未知模板 {id}（不在白名单）"))?;
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    ops.maintenance.exec(program, &args, 600_000).map_err(|e| e.to_string())
        .map(|output| json!({ "output": output }))
}
```

方案 B（若短期必须保留 `exec` 接口）：在 `win-integration` 侧做**严格模板匹配**（白名单程序 + 参数个数/取值集合），不匹配即拒：

```rust
// crates/win-integration/src/maintenance.rs
/// 每个白名单程序允许的参数模板（精确或前缀+枚举值）
fn args_are_templated(program: &str, args: &[String]) -> bool {
    let eq = |i: usize, v: &str| args.get(i).map(String::as_str) == Some(v);
    match program {
        "sfc" => args == ["/scannow"] || args == ["/verifyonly"],
        "dism" => eq(0, "/Online") && matches!(args.get(1).map(String::as_str),
                        Some("/Cleanup-Image")) && matches!(args.get(2).map(String::as_str),
                        Some("/ScanHealth") | Some("/RestoreHealth")),
        "netsh" => args == ["winsock", "reset"] || args == ["int", "ip", "reset"],
        "powercfg" => eq(0, "-setactive") && args.get(1).is_some_and(|g| GUID_ALLOWLIST.contains(&g.as_str())),
        _ => false,
    }
}
// run_exec 首部：
if !EXEC_ALLOWLIST.contains(&program) || !args_are_templated(program, args) {
    return Err(AppError::module("SYS_MAINT_010", format!("参数不在模板白名单: {program} {args:?}"), Some("仅支持预置维护动作".into())));
}
```
同时**删除** `winops_helper.rs` 的 `args` 透传入口（改为 `template_id`），并在 `win-integration` 的 `EXEC_ALLOWLIST` 中移除未被模板覆盖的程序。

**步骤 3（必须）：`registry.write` / `service.*` / `task.*` 增加键名与对象白名单**

```rust
// crates/nexusforge-helper/src/dispatch.rs
/// HKLM 可写键前缀白名单（与内置 catalog 使用的键一一对应；用户自定义项不得触及）
const REG_ALLOWED_PREFIXES: &[&str] = &[
    r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management",
    r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
    // …由 catalog 生成/人工评审维护
];
fn reg_key_allowed(key: &str) -> bool {
    let k = key.replace('/', "\\").to_ascii_lowercase();
    REG_ALLOWED_PREFIXES.iter().any(|p| k.starts_with(&p.to_ascii_lowercase()))
}
// "registry.write"/"registry.delete" 分支：if !reg_key_allowed(key) { return Err("键不在白名单") }

/// 服务名白名单（只允许本产品自述的维护服务，禁止任意服务启停）
const SVC_ALLOWLIST: &[&str] = &["w32time", "wuauserv", "bits", "DiagTrack"];
// "service.set_start" 等分支：if !SVC_ALLOWLIST.contains(&name.to_ascii_lowercase().as_str()) { return Err(...) }
```

**步骤 4：收紧外置目录权限** —— 安装时（`installer.nsh`）对 `winops\catalog` 目录设置 ACL，仅允许 Administrators 写入（普通用户不可写），使"用户可写"这一前提不成立。

**步骤 5：测试（负例必须）**

```rust
// crates/sys-core/src/winops.rs
#[test]
fn external_cannot_override_builtin_tweak() {
    // 写一个 id 与内置相同的 evil.json，断言 load_catalog 返回的该 id 仍是内置定义
}

// crates/nexusforge-helper/src/dispatch.rs
#[test]
fn exec_rejects_arbitrary_args() {
    assert!(handle_request(&ops, "exec", json!({"program":"netsh","args":["advfirewall","set","allprofiles","state","off"],"timeout_ms":1000})).is_err());
}
#[test]
fn registry_write_rejects_out_of_whitelist_key() {
    assert!(handle_request(&ops, "registry.write", json!({"key": r"HKLM\SOFTWARE\Run","value_name":"x","value":{}})).is_err());
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| catalog 来源 | `%APPDATA%\...\winops\catalog`（用户可写），同 id 覆盖内置 | 禁止覆盖内置；或仅接受签名外置项；目录 ACL 收紧 |
| `exec` 参数 | 调用方任意字符串 → 提权执行 | 只接受 `template_id`，参数为编译期常量（或严格模板匹配） |
| 注册表写 | 仅要求 `HKCU\`/`HKLM\` 前缀 | 键前缀白名单（逐条评审） |
| 服务/计划任务 | 任意名 | 服务名白名单 |
| 威胁模型 | "一次 UAC ⇒ 提权任意操作" | "一次 UAC ⇒ 仅预置维护动作" |
| 测试 | 仅固定"覆盖生效"行为 | 覆盖拒绝 + 任意参数拒绝 + 键白名单拒绝 |

### 预防措施

1. **确立"提权进程输入不可信"原则**并写入 `DESIGN.md §8`：跨提权边界的 IPC **只允许枚举/ID**，禁止字符串参数、禁止路径参数（路径仅允许 helper 侧常量表）。
2. **`docs/impl/08-winops.md` 增加"提权输入面清单"**：每个 helper 方法列出"允许的输入类型 + 白名单位置 + 负例测试名"，评审时逐条勾选。
3. **CI 加静态断言**：`grep -n "args.to_vec()" crates/win-integration/src/maintenance.rs` 应无命中；`grep -rn "helper_call(\"exec\"" src-tauri/src` 只应出现 `template_id` 形式（可写成 `security_config.rs` 里的源码扫描断言，项目已有同类手法）。
4. **外置可写目录统一清单**：任何"从 `app_data` 读取并改变**提权行为**"的路径都要进入该清单并做完整性校验——同类风险还包括自动化插件与代理内核（见 `SEC-10`）。
5. **威胁建模复训**：把本例作为"边界信任假设"教材，纳入 `07-prevention.md §3` 的评审问题清单。

---

# SEC-03 · 笔记画布读写目录穿越　`P1` `[实测]`

### 位置
- [src-tauri/src/commands/notes.rs:405-418](../../src-tauri/src/commands/notes.rs#L405-L418)（`notes_canvas_get`）
- [src-tauri/src/commands/notes.rs:420-439](../../src-tauri/src/commands/notes.rs#L420-L439)（`notes_canvas_save`）
- [crates/notes-core/src/canvas.rs:14-43](../../crates/notes-core/src/canvas.rs#L14-L43)（`canvas_path` / `load` / `save`）

### 问题描述

`dir` 参数来自前端，**未经任何校验**就进入路径拼接：

```rust
// crates/notes-core/src/canvas.rs:14-21
pub fn canvas_path(root: &Path, dir_rel: &str) -> PathBuf {
    let dir = if dir_rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(dir_rel.replace('/', "\\"))     // ← 无 norm_rel、无 .. / 绝对路径判定
    };
    dir.join(CANVAS_FILE)
}

// :33-37 save() 还会先建目录
pub fn save(root: &Path, dir_rel: &str, doc: &CanvasDoc) -> Result<()> {
    let p = canvas_path(root, dir_rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(NoteError::Io)?;   // ← 任意目录创建
    }
    ...
}
```
命令层同样直接透传：

```rust
// src-tauri/src/commands/notes.rs:414
tauri::async_runtime::spawn_blocking(move || m.canvas_get(&dir))
// :432
tauri::async_runtime::spawn_blocking(move || m.canvas_save(&dir, &doc))
```

### 影响

- `notes_canvas_save(dir = "C:\\Users\\x\\Desktop")` → 在桌面临近位置创建/覆写 `.nforge-canvas.json`。
- `notes_canvas_save(dir = "..\\..\\..\\Windows")` → 逃出笔记库根（`replace('/','\\')` 让 `../..` 变成 `..\..`，路径解析时逐级上跳）。
- 结合 `create_dir_all` 可**创建任意目录**；再配合 `SEC-04`（`norm_rel` 不拒盘符）可组合出"库外任意路径写 JSON"的稳定原语。
- 画布读取侧同样可读库外 JSON（信息泄露面较小，但可用于探测路径存在性）。

### 解决方案

**步骤 1：把 `norm_rel` 提升为唯一入口，`canvas_path` 返回 `Result`**

```rust
// crates/notes-core/src/canvas.rs
use crate::library::NoteLibrary;

pub fn canvas_path(root: &Path, dir_rel: &str) -> Result<PathBuf, NoteError> {
    // 空串 = 库根（允许）；其余一律走与笔记相同的相对路径校验
    let dir = if dir_rel.is_empty() {
        root.to_path_buf()
    } else {
        let norm = NoteLibrary::norm_rel(dir_rel)?;         // 拒 .. / 绝对 / 盘符（见 SEC-04 加固后）
        root.join(norm.replace('/', std::path::MAIN_SEPARATOR_STR))
    };
    Ok(dir.join(CANVAS_FILE))
}

pub fn load(root: &Path, dir_rel: &str) -> CanvasDoc {
    let Ok(p) = canvas_path(root, dir_rel) else { return CanvasDoc::default() };
    ...
}

pub fn save(root: &Path, dir_rel: &str, doc: &CanvasDoc) -> Result<()> {
    let p = canvas_path(root, dir_rel)?;
    // 纵深防御：落盘前复核最终路径仍在库根内
    let real_root = std::fs::canonicalize(root).map_err(NoteError::Io)?;
    let parent = p.parent().ok_or_else(|| NoteError::BadPath("画布路径无父目录".into()))?;
    std::fs::create_dir_all(parent).map_err(NoteError::Io)?;
    let real_parent = std::fs::canonicalize(parent).map_err(NoteError::Io)?;
    if !real_parent.starts_with(&real_root) {
        return Err(NoteError::BadPath(format!("画布目录越出笔记库: {dir_rel}")));
    }
    ...
}
```

**步骤 2：命令层保持薄，但保证错误可传播**（`canvas_get` 改为返回 `Result` 后由 `notes_err` 映射为带 code 的 `AppError`）。

**步骤 3：`DECISIONS.md` 登记** —— "笔记路径类 IPC（笔记/画布/附件）统一走 `norm_rel` + 落盘前 canonicalize 复核"。

**步骤 4：负例测试**

```rust
#[test]
fn canvas_path_rejects_traversal_and_absolute() {
    let root = tmpdir("canvas_guard");
    for bad in ["..", "..\\..\\Windows", "C:\\Windows", "\\\\server\\share", "a\\..\\..\\b"] {
        assert!(canvas_path(&root, bad).is_err(), "{bad} 必须被拒");
    }
    assert!(canvas_path(&root, "").is_ok(), "库根必须允许");
}

#[test]
fn canvas_save_never_writes_outside_root() {
    let root = tmpdir("canvas_write_guard");
    let outside = root.parent().unwrap().join("nf_should_not_exist.json");
    assert!(save(&root, r"..\nf_should_not_exist", &CanvasDoc::default()).is_err());
    assert!(!outside.exists());
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| `dir` 校验 | 无 | `norm_rel`（拒 `..`/绝对/盘符/UNC） |
| 落盘前复核 | 无 | `canonicalize` 断言仍在库根内 |
| 目录创建 | `create_dir_all` 于任意父目录 | 同上一并受控 |
| 返回值 | `PathBuf`（无法表达失败） | `Result<PathBuf, NoteError>` |
| 测试 | 仅 roundtrip + 损坏回退 | 新增 5 个恶意输入的负例 |

### 预防措施

1. **路径即安全边界**：在 `notes-core` 建立 `fn resolve_in_root(root, rel) -> Result<PathBuf>` 单一函数，所有涉及"用户给相对路径"的入口（笔记、画布、附件、导出）只能经由它——**禁止**任何地方出现 `root.join(user_input)` 的裸写法。
2. **CI 静态检查**：`grep -rn "root.join(" crates/notes-core/src | grep -v "resolve_in_root"` 应无命中（可写成 `security_config.rs` 中的源码断言）。
3. **IPC 契约文档标注**：`DESIGN.md §6` 对每个接受路径的命令标注"是否相对库根 / 校验函数"，避免新增命令时漏校验。
4. **同类入口审计清单**：画布之外还有 `screenshot filename_template`（`SEC-17`）、`rename template`（`COR-16` 相关）、`ocr export` 等，统一纳入该清单。

---

# SEC-04 · `norm_rel` 不拒盘符前缀 → 笔记库外任意 `.md` 读写删　`P1` `[实测]+[框架]`

### 位置
- [crates/notes-core/src/library.rs:52-75](../../crates/notes-core/src/library.rs#L52-L75)（`norm_rel` / `disk`）
- 调用点：[src-tauri/src/commands/notes.rs:86,122,143,160](../../src-tauri/src/commands/notes.rs#L86)（`notes_create`/`notes_write`/`notes_delete`/`notes_read`）

### 问题描述

```rust
// crates/notes-core/src/library.rs:52-63
pub fn norm_rel(rel: &str) -> Result<String> {
    let t = rel.trim().trim_start_matches(['/', '\\']);      // 只剥前导分隔符
    if t.is_empty() { return Err(NoteError::BadPath("路径为空".into())); }
    let segs: Vec<&str> = t.split(['/', '\\']).collect();
    if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return Err(NoteError::BadPath(format!("路径含非法段: {rel}")));
    }
    Ok(segs.join("/"))                                       // ← 不检查 Component::Prefix
}

fn disk(&self, rel: &str) -> PathBuf {
    self.root.join(rel.replace('/', "\\"))                    // ← PathBuf::join 遇 prefix 会替换根
}
```

Windows 语义（`[框架]`）：`Path::new("C:\\Users\\x").is_absolute() == true`，而 `PathBuf::push` 对含 `Prefix` 的入参**清空已有路径**。因此 `"C:\\Users\\x\\Desktop\\pwn.md"` 经 `norm_rel` 后是 `"C:/Users/x/Desktop/pwn.md"`，`disk()` 得到 `C:\Users\x\Desktop\pwn.md`——**完全脱离笔记库根**。

`require_md`（[:66-71](../../crates/notes-core/src/library.rs#L66-L71)）只校验 `.md` 后缀，不构成限制。

### 影响

- `notes_create("C:\\Users\\x\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\evil.md")` → 在启动目录投放 `.md`（若被其他工具/编辑器联想执行则有进一步影响）。
- `notes_write` 可**覆写**用户任意 `.md` 文件（例如其他项目的文档、配置化的 markdown 清单）。
- `notes_delete` 可**删除**用户任意 `.md` 文件（不可逆）。
- 因为 `.md` 是同步数据集成员，越权写入的文件还会进入同步链路，扩散到其他设备。

### 解决方案

**步骤 1：`norm_rel` 增加前缀/绝对路径拒绝（同时保留既有段校验）**

```rust
// crates/notes-core/src/library.rs
use std::path::{Component, Path};

pub fn norm_rel(rel: &str) -> Result<String> {
    let raw = rel.trim();
    let p = Path::new(raw);

    // ① 绝对路径（含 UNC）直接拒
    if p.is_absolute() {
        return Err(NoteError::BadPath(format!("不接受绝对路径: {rel}")));
    }
    // ② 盘符/设备前缀（C:、\\?\、\\.\）直接拒
    if matches!(p.components().next(), Some(Component::Prefix(_))) {
        return Err(NoteError::BadPath(format!("不接受盘符前缀: {rel}")));
    }
    // ③ 逐个组件校验，只允许 Normal（拒 RootDir/ParentDir/CurDir）
    let mut segs = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str().ok_or_else(|| NoteError::BadPath("路径含非法字符".into()))?;
                if s.is_empty() { return Err(NoteError::BadPath(format!("空段: {rel}"))); }
                segs.push(s.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir => return Err(NoteError::BadPath(format!("含 .. 段: {rel}"))),
            Component::RootDir | Component::Prefix(_) => {
                return Err(NoteError::BadPath(format!("含根/前缀组件: {rel}")))
            }
        }
    }
    if segs.is_empty() { return Err(NoteError::BadPath("路径为空".into())); }
    Ok(segs.join("/"))
}
```

**步骤 2（纵深防御）：`disk()` 改为可失败并在建/写/删前复核**

```rust
fn disk(&self, rel: &str) -> Result<PathBuf> {
    let norm = Self::norm_rel(rel)?;
    let p = self.root.join(norm.replace('/', std::path::MAIN_SEPARATOR_STR));
    // 词法复核（无需 canonicalize，覆盖新建场景）
    if !p.starts_with(&self.root) {
        return Err(NoteError::BadPath(format!("越出笔记库: {rel}")));
    }
    Ok(p)
}
```
（`disk` 由 `PathBuf` 改为 `Result<PathBuf>` 会波及多处调用点，属必要改动；建议一次性改完并借编译器定位全部调用者。）

**步骤 3：负例测试（覆盖三类绕过）**

```rust
#[test]
fn norm_rel_rejects_prefix_and_absolute() {
    for bad in [r"C:\Windows\x.md", r"C:/Windows/x.md", r"\\server\share\x.md", r"\\.\C:\x.md",
                r"..\..\x.md", r"a/../../x.md", r"\Windows\x.md"] {
        assert!(NoteLibrary::norm_rel(bad).is_err(), "{bad} 必须被拒");
    }
    assert_eq!(NoteLibrary::norm_rel("a/b/c.md").unwrap(), "a/b/c.md");
}

#[test]
fn create_write_delete_cannot_escape_root() {
    let lib = tmp_library("escape_guard");
    let outside = std::env::temp_dir().join("nf_outside_probe.md");
    let _ = std::fs::remove_file(&outside);
    assert!(lib.create(r"C:\Users\Public\nf_outside_probe.md", "x").is_err());
    assert!(!outside.exists());
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 前导分隔符 | `trim_start_matches` 剥掉（`/x.md` → `x.md`） | 由 `RootDir` 分支显式拒绝（更严格，语义清晰） |
| 盘符前缀 | **未检查** → `join` 替换根 → 逃逸 | `Component::Prefix` / `is_absolute` 双检 → 拒绝 |
| `disk()` | 无条件 `root.join(...)` | 返回 `Result`，并断言 `starts_with(root)` |
| 影响面 | 库外任意 `.md` 读/写/删 | 严格限制在库根内 |
| 测试 | 无（仅路径越界的间接覆盖） | 11 个恶意输入负例 + 磁盘副作用断言 |

### 预防措施

1. **"相对路径"必须用 `Component` 判定，不能用字符串判定**：把该规则写进 `CONTRIBUTING.md` 的 Rust 安全编码约定；字符串 `contains("..")` 与 `is_absolute()` 在 Windows 上都不充分（前者漏 `\Windows`，后者漏 `C:` 前缀组合场景）。
2. **单点收敛**：`norm_rel` 是笔记域唯一入口；建议同样为 `file-core`（远端与本地）与 `screenshot-core`（文件名模板）建立各自的 `resolve_in_root`，并在 `DECISIONS.md` 登记"路径校验入口函数一览"。
3. **模糊测试**：为 `norm_rel`、`canvas_path`、解压条目名等纯函数加 `cargo-fuzz`/`proptest` 属性测试（性质：返回值必须使 `root.join(result)` 仍在 `root` 下）。
4. **CI 断言**：`crates/notes-core/src/library.rs` 中不得再出现 `is_absolute` 之外的裸 `join(rel` 调用。

---

# SEC-05 · 解压 zip-slip 变体（根相对条目名逃逸）　`P1` `[实测]+`[框架]`

### 位置
- [crates/file-core/src/ops.rs:2192-2199](../../crates/file-core/src/ops.rs#L2192-L2199)（`run_extract`）

### 问题描述

```rust
// crates/file-core/src/ops.rs:2192-2199
// zip-slip 防护：拒绝逃逸根目录的条目
let name = entry.name().to_owned();
if name.contains("..") || Path::new(&name).is_absolute() {
    tracing::warn!(name = %name, "跳过可疑 zip 条目");
    rep.cur.files_done += 1;
    continue;
}
let out_path = root.join(&name);
```

`[框架]` 依据：
- `Path::new("\\Windows\\evil.dll").is_absolute()` 在 Windows 上返回 **false**（`is_absolute` 要求同时有 prefix **和** root）。
- `Path::new(r"C:\out").join(r"\Windows\evil.dll")` → `C:\Windows\evil.dll`（有根无前缀的入参**替换除前缀外的全部**）。
- 条目名 `/\Windows/...` 与 `\Windows\...` 均可被 zip 头写入。

即：**`..` 被拦了，但"根相对路径"没被拦**。

### 影响

构造一个条目名为 `\Windows\System32\evil.dll`（或 `\Users\Public\Startup\a.bat`）的压缩包：
- 在用户解压到 `C:\downloads\pkg` 时，实际写出到 `C:\Windows\System32\evil.dll`（同盘任意位置）。
- 若用户以管理员身份运行本程序（产品存在提权 helper 与系统维护功能，用户态可能提权），可直接覆盖系统文件。
- 与 `SEC-06` 组合（远端返回的压缩包）可形成"远端 → 本机任意路径写"的完整链路。

### 解决方案

**步骤 1：改用 `enclosed_name()`（zip crate 内建，拒绝 RootDir/Prefix/ParentDir/`\0`）**

```rust
// crates/file-core/src/ops.rs:2192-
let Some(rel) = entry.enclosed_name() else {
    tracing::warn!(name = %entry.name(), "跳过可疑 zip 条目（enclose 校验失败）");
    rep.cur.files_done += 1;
    continue;
};
let out_path = root.join(&rel);
// 纵深防御：词法复核
if !out_path.starts_with(root) {
    tracing::warn!(name = %entry.name(), "跳过越出根的条目");
    rep.cur.files_done += 1;
    continue;
}
```

**步骤 2（可选，字节级兜底）：自实现段级校验，避免依赖第三方语义**

```rust
/// 仅接受 Normal 段组成的相对路径；拒绝 Prefix/RootDir/ParentDir/空段
fn safe_entry_rel(name: &str) -> Option<PathBuf> {
    if name.contains('\0') { return None; }
    let mut out = PathBuf::new();
    for seg in name.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => return None,
            s if s.contains(':') => return None,   // 防 ADS 与盘符
            s => out.push(s),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}
```
（若同时提供 ADS 防护，`a.txt:evil` 也会被拒——但注意正常文件名不含 `:`，合法。）

**步骤 3：落盘时使用长路径前缀（既有 `to_long_path` 保留），并在解压前对根做 `canonicalize` 一次**，把 `real_root` 作为复核基准（避免 `root` 本身含符号链接/`..` 带来的歧义）。

**步骤 4：负例测试（当前仅有良性 roundtrip，必须补）**

```rust
#[test]
fn extract_rejects_root_relative_and_parent_entries() {
    // 用 zip::ZipWriter 造 3 个恶意条目：r"\Windows\evil.txt"、r"..\..\evil.txt"、r"C:\evil.txt"
    // 断言：全部跳过，且磁盘上不存在越界文件
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 判定方式 | 字符串 `contains("..")` + `is_absolute()` | `enclosed_name()` + 段级校验 + `starts_with(root)` |
| `\Windows\x` | **放行 → 逃逸** | 拒绝（RootDir 组件） |
| `C:\x` | 放行（`is_absolute` 为 true 时能拦，但混写 `C:x` 形态需段校验） | 拒绝（Prefix 组件 / `:` 段） |
| 符号链接根 | 未处理 | `canonicalize(root)` 作复核基准 |
| 测试 | 1 个良性 roundtrip | 新增 3 类恶意条目负例 |

### 预防措施

1. **归档解压专项 checklist**（写进 `docs/impl/` 对应模块文档）：条目名校验、条目数上限、单文件/总解压上限、符号链接条目处理、ADS、长路径、覆盖策略、目标根复核——8 条逐项给出实现位置与测试名。
2. **同一 `safe_rel_path` 工具函数全仓复用**：解压、远端下载落点（`SEC-06`）、批量重命名目标（`rename.rs`）、截图文件名模板（`SEC-17`）都应调用同一实现，避免"每处各写一遍校验"。
3. **CI 增加"恶意压缩包"夹具测试**：仓库内放一个固定的小体积 `tests/fixtures/traversal.zip`，作为回归门禁的一部分（也便于验收）。
4. **引入 `cargo-fuzz` 对 `safe_entry_rel` 做属性测试**：性质 = 返回值必须使 `root.join(ret) ⊆ root`。

---

# SEC-06 · 远端条目名穿越本地下载落点　`P1` `[实测]`

### 位置
- [crates/file-core/src/ops.rs:1589-1596](../../crates/file-core/src/ops.rs#L1589-L1596)（`build_remote_items` 的 Download 臂）
- 名称来源：[crates/file-core/src/ops.rs:1389-1417](../../crates/file-core/src/ops.rs#L1389-L1417)（`walk_remote`）
- WebDAV 名称解码：[crates/file-core/src/remote/webdav.rs:33-52,74-91,177-180](../../crates/file-core/src/remote/webdav.rs#L33-L52)

### 问题描述

```rust
// crates/file-core/src/ops.rs:1589-1596
let full_rel = if prefix.is_empty() { rel } else { format!("{prefix}/{rel}") };
let target = dst_local.join(full_rel.replace('/', std::path::MAIN_SEPARATOR_STR));
```

`rel` 来源于远端服务端返回的条目名（WebDAV 的 `href` 末段、SFTP/FTP 的目录项 `name`）：
- WebDAV 侧 `entry_name_from_href` 会做百分号解码（`%2e%2e` → `..`、`%5C` → `\`）；
- SFTP/FTP 侧的 `name` 完全由服务端给定。

落点仅做 `/ → \` 替换，**未过滤 `..`、未过滤反斜杠、未判绝对路径**。

### 影响

已配对/被攻陷的 WebDAV 或 SFTP 服务器（或局域网 ARP/DNS 劫持者，在信任建立阶段之后）可返回名为 `..\..\..\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup\p.bat` 的条目 → 用户执行"下载到 D:\download"时，文件被写到**启动目录**（持久化），或在任意位置写任意文件（覆盖配置文件、注入 DLL 到可被加载的目录）。

### 解决方案

**步骤 1：复用 `safe_rel_path`（与 `SEC-05` 同一实现），在落点处强制校验**

```rust
// crates/file-core/src/ops.rs（Download 臂）
let Some(rel_safe) = safe_rel_path(&full_rel) else {
    tracing::warn!(rel = %full_rel, "远端条目名非法，跳过下载");
    continue;                    // 或计入 failed 并把原因回报
};
let target = dst_local.join(&rel_safe);
if !target.starts_with(&dst_local) {
    tracing::warn!(rel = %full_rel, "下载落点越出目标目录，跳过");
    continue;
}
```

**步骤 2：在传输层再加一道"每文件落点清单预检"** —— 入队阶段（`xfer` 队列）就校验全部目标路径，避免下载到一半才发现；这样 UI 能在开始前给出"检测到 X 条非法条目"。

**步骤 3：WebDAV 名称解码后立即规范化** —— `entry_name_from_href` 返回前拒绝含 `..` 段或 `\` 的名字（解码在裁决之前，顺序不能反）。

**步骤 4：负例测试**

```rust
#[test]
fn remote_download_rejects_traversal_names() {
    // 用假 provider 返回条目名 "..\\..\\evil.txt" 与 "a/../../evil.txt"
    // 断言：不产生越界文件；返回结果包含被拒计数
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 名称处理 | 仅 `/`→`\` | 段级校验 + `starts_with(dst_local)` |
| 校验时机 | 无（下载时直接 join） | 入队预检 + 落点复核（两道） |
| 恶意名行为 | 写出目标目录之外 | 跳过并回报条目名/原因 |
| 测试 | 无 | 2 类穿越名负例 |

### 预防措施

1. **"远端数据一律不可信"写入设计约束**：`DESIGN.md §8` 增补"所有来自对端/远端服务器的字符串字段（文件名、路径、键名、规则表达式）必须经服务端与客户端双侧校验"，并列出各字段的校验函数。
2. **统一的远端结果解析层**：WebDAV/SFTP/FTP 三个 provider 返回的条目集合应先过 `sanitize_entries()`，再进入业务逻辑，避免"每个消费点各校验一次"。
3. **把该场景加入安全测试套件**：假 provider 注入恶意名字已是低成本手法（项目已有假 provider 测试基础）。
4. **与 `SEC-05` 合并为一个"归档/远端路径安全"任务批次**，一次把 `safe_rel_path` 铺满所有落点。

---

# SEC-07 · Markdown 预览未净化 → 跨设备 XSS → IPC 面　`P1` `[实测]`

### 位置
- [src/modules/notes/NotesPanel.tsx:985-990](../../src/modules/notes/NotesPanel.tsx#L985-L990)
- [src/modules/editor/EditorPanel.tsx:675-676](../../src/modules/editor/EditorPanel.tsx#L675-L676)
- 数据来源：[crates/sync-core/src/module.rs:1912-1947](../../crates/sync-core/src/module.rs#L1912)（`notes` 属同步数据集）
- CSP 现状：[src-tauri/tauri.conf.json:31](../../src-tauri/tauri.conf.json#L31)

### 问题描述

```tsx
// src/modules/notes/NotesPanel.tsx:985-990
{preview && (
  <div className={styles.preview}
       dangerouslySetInnerHTML={{ __html: marked.parse(content) as string }} />
)}
```

- `marked@^15`（`package.json:21`）自 v5 起**移除了 `sanitize` 选项**，`marked.parse` 对原始 HTML 是**透传**的。
- 全库检索 `DOMPurify` / `sanitize-html`：**零命中**。
- EditorPanel 的注释自认未净化："v1 内容来自用户本地文件，与编辑器同信任域"——该假设对 **notes** 不成立：`sync-core` 明确把 `"notes"` 列入同步数据集，笔记内容可来自**对端设备**。

```
// crates/sync-core/src/module.rs:1912 起（数据集定义，含 "notes"）
```

CSP 为 `default-src 'self'; script-src 'self'`：内联事件处理器（`onerror=`）**不受 `script-src` 约束**（需 `unsafe-hashes`/nonce 才能拦 `on*` 属性——实际上 `on*` 属于 inline script，浏览器策略是"若无 `unsafe-inline` 则拦 inline script"，但**属主 HTML 内的 `on*` 属性在多数情况下仍会被拦**……关键点在于：本项目 WebView 环境与 CSP 组合下，注入 `<img src=x onerror=...>` 与 `<a href="javascript:...">`、`<iframe srcdoc>` 等仍可能达成执行，且**即使执行被拦，注入本身已可做 UI 欺骗/外链窃取**）。因此必须以"净化输入"为唯一可靠防线，不能只靠 CSP。

### 影响

1. **代码执行**：对端设备推送含 `<img src=x onerror="fetch('/x')">` 的 `.md` → 在用户打开预览时于 WebView 内执行任意 JS。
2. **IPC 提权面**：该 JS 可访问 `window.__TAURI_INTERNALS__.invoke`，按当前窗口 capability 调用 IPC（main 窗口持有大量模块权限，包括剪贴板读取、文件读写、vault 命令）——**跨设备的 XSS 直接升级为本地 IPC 越权**。
3. **数据外泄**：可把剪贴板内容、笔记全文、配置（含订阅 URL）编码后经 `connect-src`（`ipc:`/`self`）或弹窗/表单外发（`connect-src` 已限制为 self/ipc，但 `img-src`/`font-src` 仍可用于外带）。
4. **影响还需注意 `EditorPanel`**：虽然当前假设"本地文件"，但用户打开他人给的 `.md`（邮件附件、下载文件）即等同不可信输入。

### 解决方案

**步骤 1：引入 DOMPurify 并统一到一个 `MarkdownView` 组件**

```bash
npm i dompurify
npm i -D @types/dompurify   # 若版本已内置类型可省略
```

```tsx
// src/components/MarkdownView.tsx（新建共享组件）
import { useMemo } from "react";
import DOMPurify from "dompurify";
import { marked } from "marked";

const ALLOWED_TAGS = [
  "h1","h2","h3","h4","h5","h6","p","br","hr","ul","ol","li","blockquote",
  "pre","code","strong","em","del","ins","table","thead","tbody","tr","th","td",
  "a","img","input", // input 仅用于 task list
];
const ALLOWED_ATTR = ["href","title","alt","src","class","type","checked","disabled","align"];

export function renderMarkdown(src: string): string {
  const raw = marked.parse(src, { async: false }) as string;
  return DOMPurify.sanitize(raw, {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    // 关键：只允许安全协议的链接与图片
    ALLOWED_URI_REGEXP: /^(?:https?|mailto|data:image\/(?:png|jpe?g|gif|webp);base64,)/i,
    FORBID_TAGS: ["style", "script", "iframe", "object", "embed", "form", "base", "link"],
    FORBID_ATTR: ["onerror","onload","onclick","onmouseover","style","formaction","xlink:href"],
  });
}

export default function MarkdownView({ source, className }: { source: string; className?: string }) {
  const html = useMemo(() => renderMarkdown(source), [source]);
  return <div className={className} dangerouslySetInnerHTML={{ __html: html }} />;
}
```

**步骤 2：两处调用点改为 `MarkdownView`**

```tsx
// NotesPanel.tsx:985-990 →
{preview && <MarkdownView source={content} className={styles.preview} />}

// EditorPanel.tsx:675-676 →
{mdPreview && <MarkdownView source={mdSource} className={styles.preview} />}
```
（EditorPanel 同时删除"与编辑器同信任域"的错误注释，改为"远端/外部文件均不可信"。）

**步骤 3：外链统一走"不安全外链"处理** —— 净化后再对 `<a>` 注入 `rel="noopener noreferrer"` 与 `target="_blank"`，或改为"点击经 `host_open_url` IPC 用系统浏览器打开"，避免 WebView 内导航。

**步骤 4：CSP 补强（与 `SEC-14` 联动）**

```json
"csp": "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: http://asset.localhost; font-src 'self' data:; worker-src 'self' blob:; connect-src 'self' ipc: http://ipc.localhost; object-src 'none'; base-uri 'self'; form-action 'self'; frame-src 'none'"
```

**步骤 5：负例测试（必须）**

```tsx
// src/modules/notes/__tests__/markdownSanitize.test.tsx
it("markdown 预览必须净化脚本与事件处理器", () => {
  const html = renderMarkdown('<img src=x onerror="alert(1)"><script>alert(2)</script><a href="javascript:alert(3)">x</a>');
  expect(html).not.toMatch(/onerror/i);
  expect(html).not.toMatch(/<script/i);
  expect(html).not.toMatch(/javascript:/i);
});

it("允许安全链接与图片", () => {
  expect(renderMarkdown("[a](https://example.com)")).toContain('href="https://example.com"');
});
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 渲染 | `marked.parse()` 直插 DOM | `DOMPurify.sanitize(marked.parse())`（标签/属性/协议白名单） |
| 实现位置 | 2 处各自 inline | 1 个 `MarkdownView` 共享组件 |
| 对端内容 | 可执行任意 JS | 仅渲染白名单标签，脚本/事件属性/危险协议被剥离 |
| IPC 风险 | 跨设备 XSS → IPC 越权 | XSS 不可达 → 无越权路径 |
| CSP | 7 条指令 | 补 `object-src`/`base-uri`/`form-action`/`frame-src` |
| 测试 | `monacoWiki.test.tsx` 反而断言 `innerHTML` 原样输出 | 新增"必须净化"与"允许安全链接"两条 |

### 预防措施

1. **建立"任何 `dangerouslySetInnerHTML` 必须经净化"的门禁**：ESLint 自定义规则或 `no-restricted-syntax` 禁止直接写 `dangerouslySetInnerHTML`，只允许 `MarkdownView`（项目已用 ESLint，增量成本低）。
2. **把"信任域"写成显式清单**：`DESIGN.md §8` 列出"哪些数据可能来自对端"（notes 内容、同步字段、远端文件名、订阅内容、插件元数据），这些数据在前端**一律按不可信处理**。
3. **安全测试纳入 CI**：`markdownSanitize.test.tsx` 加入 vitest 常规套件；并在 `security_config.rs` 侧断言"仓库内 `dangerouslySetInnerHTML` 只出现在 `MarkdownView.tsx`"（源码扫描断言，项目已有同类先例）。
4. **依赖治理**：`DOMPurify` 进入 `THIRD_PARTY_LICENSES.md`（Apache-2.0/MPL-2.0 双许可，注意合规声明），由 `cargo-deny`/`npm audit` 门禁覆盖（见 `GOV-02`）。
5. **同类排查**：所有"把远端字符串渲染成富文本/HTML"的位置都要过同一净化函数（如未来 OCR 结果预览、代理日志高亮、自动化规则描述）。

---

# SEC-08 · `assetProtocol` 授权面过宽（`$APPDATA/**`）　`P1` `[实测]`

### 位置
- [src-tauri/tauri.conf.json:30-36](../../src-tauri/tauri.conf.json#L30-L36)
- 缺失断言：[src-tauri/tests/security_config.rs](../../src-tauri/tests/security_config.rs)（全文件无 assetProtocol 断言）

### 问题描述

```json
"security": {
  "csp": "... img-src 'self' data: http://asset.localhost; ...",
  "assetProtocol": { "enable": true, "scope": ["$APPDATA/**"] }
}
```

`$APPDATA` 指应用数据根（`%APPDATA%\com.nexusforge.app`），其下有 `db/`（含各模块 SQLite + WAL）、`blobs/`、`config.json`、`log/`、`export/`、`automation/rules.json`、`kvm/identity.json`（DPAPI 包裹的私钥）、`vault/`（加密库与信封）、`winops/catalog/` 等。

`asset` 协议 scope 在 Tauri 中是**全局**的（不区分窗口），且 CSP 已放行 `http://asset.localhost`。

### 影响

- 任一窗口（含 `overlay`/`pin`/`quickpanel`/`notebar`/`launcher` 这些"低权限"窗口）内的脚本可请求 `http://asset.localhost/...` 读取数据目录下**任意文件**。
- 与 `SEC-07`（XSS 面）组合时，攻击者可把 `identity.json`、`config.json`（含订阅 URL、路径、账号名等）、`log/` 读出来并经图片/字体信道外带。
- 超出产品实际需要：前端目前只用 asset 协议展示**笔记图片与截图缩略图**（`NotesPanel` 走 `convertFileSrc`、`canvasEdit.ts` 正则门控 `^(data:|https?:|asset:|ipc:)`）。

### 解决方案

**步骤 1：scope 收窄到"只读媒体子目录"**

```json
"assetProtocol": {
  "enable": true,
  "scope": [
    "$APPDATA/blobs/**",
    "$APPDATA/screenshot/images/**",
    "$APPDATA/notes/**"
  ]
}
```
（按实际前端使用面逐条列出；先在前端搜索 `convertFileSrc` 的调用点确定最小集合。）

**步骤 2：非媒体文件改走命令返回字节** —— 若确需展示库文件/日志，新增专用只读命令（如 `editor_read_export_preview`），由 Rust 侧做路径校验与内容脱敏，**不走 asset 协议**。

**步骤 3：加断言（把"过宽 scope 回归"变成红灯）**

```rust
// src-tauri/tests/security_config.rs
#[test]
fn asset_protocol_scope_is_narrow() {
    let cfg: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    let scope = cfg["app"]["security"]["assetProtocol"]["scope"].as_array().unwrap();
    let pats: Vec<&str> = scope.iter().filter_map(|v| v.as_str()).collect();
    assert!(!pats.iter().any(|p| *p == "$APPDATA/**"),
        "assetProtocol 禁止整根授权（$APPDATA/**）");
    for p in &pats {
        assert!(p.starts_with("$APPDATA/") && p.ends_with("/**"),
            "scope 必须是具体子目录：{p}");
    }
}
```

**步骤 4：`DESIGN.md §8.5` 补一条**"asset 协议 scope 必须为具体子目录，禁止 `$APPDATA/**`；新增子目录需在本测试中显式加入"。

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| scope | `$APPDATA/**`（整根） | 3 个媒体子目录（按前端真实需要） |
| 低权限窗口可达面 | 全部数据文件（含私钥信封、配置、日志） | 仅图片/blob |
| 库文件/日志展示 | 经 asset 直读 | 经专用只读命令（校验 + 脱敏） |
| 测试 | 无断言 | 新增"禁止整根授权"断言 |

### 预防措施

1. **"最小授权"落到配置测试**：capabilities（已有 17 权限文件 + 6 按窗能力）与 assetProtocol 都应有一对一断言；把 `security_config.rs` 扩展为"安全配置清单"的单一事实来源。
2. **新窗口/新目录的加目录（checklist）**：新增 capability 或 asset scope 时，必须同步在 `security_config.rs` 写断言并列明用途。
3. **数据目录分级**：把"密级数据"（vault/kvm/identity）与"可展示媒体"（blobs/screenshots）物理分目录（如 `%APPDATA%\com.nexusforge.app\secure\` vs `\media\`），从结构上让"只读媒体 scope"无法触及敏感区。
4. **配合 `SEC-07` 一次性整改前端渲染面**，避免"净化后仍可从 asset 读敏感文件外带"的残余路径。

---

# SEC-09 · 系统代理端口变更后崩溃还原失效　`P1` `[实测]`

### 位置
- [crates/proxy-core/src/sysproxy.rs:104-120](../../crates/proxy-core/src/sysproxy.rs#L104-L120)（`restore_if_ours`）
- [crates/proxy-core/src/service.rs:303](../../crates/proxy-core/src/service.rs#L303)（启动扫描调用）
- [crates/proxy-core/src/service.rs:705-712](../../crates/proxy-core/src/service.rs#L705-L712)（`set_mixed_port`）

### 问题描述

```rust
// crates/proxy-core/src/sysproxy.rs:104-120
pub fn restore_if_ours(proxy_dir: &Path, sp: &dyn SysProxyPort, mixed_port: u16) -> Result<bool> {
    let current = sp.get()?;
    if current.enable && current.server == our_server(mixed_port) {   // ← 用"当前配置端口"算期望值
        restore(proxy_dir, sp)?;
        ...
    } else {
        // 当前值不是我们的（用户改过/已关闭）：备份已失效，清理
        let _ = std::fs::remove_file(backup_path(proxy_dir));          // ← 删备份且不还原
    }
}

// crates/proxy-core/src/service.rs:705-712 —— 只改内存 + 持久化，不重写注册表
pub fn set_mixed_port(&self, port: u16) -> Result<()> {
    let mut inner = self.inner.lock();
    inner.mixed_port = port;
    self.persist_state(&inner)
}

// :303 启动时用"当前内存里的端口"比对
let restored = sysproxy::restore_if_ours(&svc.proxy_dir, svc.sp.as_ref(), state.mixed_port)
```

时序：开启系统代理（注册表写入 `127.0.0.1:<旧端口>`）→ 用户改 `mixed_port`（注册表仍是旧端口）→ 进程被强杀（来不及还原）→ 下次启动用**新端口**比对 → 不相等 → 判定"用户改过" → **删除备份且不还原**。

### 影响

系统代理被永久留在`127.0.0.1:<旧端口>`（已无进程监听）→ **全机断网**（浏览器、更新、其他应用全部走死代理）。这是"能一键恢复"承诺最可能失败的路径，且用户很难自行定位（注册表 `HKCU\...\Internet Settings\ProxyServer`）。

### 解决方案

**步骤 1：备份文件记录"实际写入的 server 字符串"，比对不再依赖当前端口**

```rust
// crates/proxy-core/src/sysproxy.rs
#[derive(serde::Serialize, serde::Deserialize)]
struct ProxyBackup {
    enable: bool,
    server: String,      // ← 开启时真实写入的 "127.0.0.1:7890"
    bypass: String,
    applied_ms: i64,
}

pub fn enable(proxy_dir: &Path, sp: &dyn SysProxyPort, mixed_port: u16) -> Result<()> {
    let cur = sp.get()?;
    let backup = ProxyBackup {
        enable: cur.enable, server: cur.server.clone(), bypass: cur.bypass.clone(), applied_ms: now_ms(),
    };
    write_json_atomic(&backup_path(proxy_dir), &backup)?;      // tmp + rename
    sp.set(&SysProxy{ enable: true, server: our_server(mixed_port), ..default })?;
    Ok(())
}

pub fn restore_if_ours(proxy_dir: &Path, sp: &dyn SysProxyPort) -> Result<bool> {
    let Some(backup) = read_backup(proxy_dir) else { return Ok(false) };
    let current = sp.get()?;
    // 只认"当前值是否仍等于我们写入的那个值"
    if current.enable && current.server == backup.server_we_wrote() {
        restore_from_backup(sp, &backup)?;
    }
    let _ = std::fs::remove_file(backup_path(proxy_dir));
    Ok(true)
}
```
（`backup.server_we_wrote()` = 备份里记录的"我们写入的 server"，即开启时算出的 `our_server(port)`；**签名改为把该值一并存进备份**。）

**步骤 2：`set_mixed_port` 在"代理开启中"时同步重写注册表（保持不变量：注册表 == 当前端口）**

```rust
pub fn set_mixed_port(&self, port: u16) -> Result<()> {
    let mut inner = self.inner.lock();
    let was_on = inner.mode == ProxyMode::System && inner.sys_proxy_applied;
    inner.mixed_port = port;
    self.persist_state(&inner)?;
    drop(inner);
    if was_on {
        self.sysproxy_enable_quiet(port)?;   // 幂等重写；失败即回滚内存端口
    }
    Ok(())
}
```

**步骤 3：进程级兜底 —— 注册"退出即还原"**：`tray`/`lib.rs` 的退出路径（含 `ExitRequested`、panic hook）调用 `restore_if_ours`；同时把"上次运行遗留"检测放到**早于 UI 的启动阶段**。

**步骤 4：测试（覆盖"改端口后崩溃"）**

```rust
#[test]
fn restore_works_after_port_change_then_crash() {
    // FakeSysProxy 记录 set 调用；enable(7890) → set_mixed_port(7891) → 模拟"未还原即退出"
    // 断言 restore_if_ours 仍能识别为我们所设并还原为原值
}

#[test]
fn restore_does_not_touch_user_modified_settings() {
    // 用户把 server 改成别的值 → restore_if_ours 不得覆盖用户值
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 残留识别依据 | 用**当前** `mixed_port` 反算期望值 | 用备份中记录的**实际写入值** |
| 改端口后的注册表 | 保持旧端口（与内存不一致） | `set_mixed_port` 同步重写（不变式成立） |
| 崩溃恢复 | 判为"用户改过" → 删备份不还原 → 永久断网 | 正确识别并还原 |
| 用户改过的情况 | 删备份（语义正确） | 仍删备份、不覆盖用户值（保持） |
| 测试 | 无端口变更场景 | 新增 2 条（崩溃恢复 / 用户改动不覆盖） |

### 预防措施

1. **"系统级副作用必须可幂等还原"原则**：凡修改注册表/防火墙/服务/计划任务的功能，都要有"备份（含实际写入值）+ 幂等还原 + 启动扫描 + 退出钩子 + 崩溃恢复测试"五件套；`DESIGN.md §8` 增补该要求。
2. **不变量显式化**：代码注释或类型中写清"代理开启 ⇒ 注册表 server == `our_server(mixed_port)`"，并在 `set_mixed_port` 处维护它。
3. **回归测试纳入常规套件**（不要放 `#[ignore]`）：这是用户可感知的"断网级"故障。
4. **同类排查清单**：TUN 模式切换（`SEC-12`）、WinOps 注册表 tweak（`SEC-02`）、Defender/服务变更都应有对应的"备份-还原-崩溃恢复"设计与测试。

---

# SEC-10 · 订阅/内核下载无大小上限、无重定向策略、不校验状态码　`P2` `[实测]`

### 位置
[proxy-core/src/service.rs:455-490](../../crates/proxy-core/src/service.rs#L455-L490)

### 问题描述
```rust
let client = reqwest::Client::builder().user_agent(APP_UA).timeout(Duration::from_secs(30)).build()...
let mut req = client.get(url);
...
match resp.bytes().await { Ok(b) => return Ok(FetchOutcome { bytes: b.to_vec(), ... }) }
```
① 无响应体大小上限（`bytes()` 全量入内存；reqwest 默认自动 gzip 解压 → gzip bomb 可放大）；② 未设 `redirect` 策略（默认跟随最多 10 跳）且不校验最终 scheme/host → 可被重定向到内网（SSRF 探测）或降级到明文；③ 注释明示**不检查状态码**（`service.rs:453`）。

### 影响
恶意/被劫持的订阅 URL 或面板配置可致内存耗尽（进程崩溃）、内网探测、订阅内容（含节点凭据）经明文传输。

### 解决方案

```rust
// proxy-core/src/service.rs
const MAX_FETCH_BYTES: usize = 8 * 1024 * 1024;      // 订阅/geo 上限
const MAX_KERNEL_BYTES: usize = 128 * 1024 * 1024;   // 内核上限（独立常量）

fn build_client(allow_http: bool) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(APP_UA)
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            // 只允许 https 终态，最多 3 跳
            if attempt.previous().len() >= 3 { attempt.stop() }
            else if attempt.url().scheme() != "https" && !allow_http { attempt.stop() }
            else { attempt.follow() }
        }))
        .build().map_err(ProxyError::Http)
}

async fn fetch_capped(url: &str, max: usize) -> Result<Vec<u8>> {
    let resp = build_client(false)?.get(url).send().await.map_err(ProxyError::Http)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(ProxyError::Subscription(format!("下载失败：HTTP {status}")));
    }
    if let Some(len) = resp.content_length() {
        if len as usize > max {
            return Err(ProxyError::Subscription(format!("响应过大：{len} > {max}")));
        }
    }
    let mut out = Vec::new();
    let mut stream = resp;
    while let Some(chunk) = stream.chunk().await.map_err(ProxyError::Http)? {
        if out.len() + chunk.len() > max {
            return Err(ProxyError::Subscription(format!("下载超过上限 {max} 字节")));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}
```
内核/geo 下载另加 SHA256 校验（当前仅在解压后校验，见 `PERF` 与 sidecar 相关项）与"已知值比对"开关。

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 响应大小 | 无上限（含 gzip 放大） | `content_length` 预检 + 流式累计硬上限 |
| 重定向 | 默认 10 跳、允许降级明文 | ≤3 跳、仅 https 终态 |
| 状态码 | 不检查 | 非 2xx 明确报错 |
| 失败语义 | 可能"成功返回错误页内容" | code + hint 清晰 |

### 预防措施
1. "所有出网请求必须向量化"：把 `build_client` 收敛为唯一工厂，禁止业务处直接 `Client::builder()`；CI 断言 `grep -c "Client::builder" == 1`。
2. 网络访问清单化：`DESIGN.md §8` 列出"允许出网的功能 + 目标 + 上限 + 是否校验内容签名"。
3. 新增"下载夹具"测试：本地 HTTP 服务器返回超大体/302 到内网/500，断言被拒。

---

# SEC-11 · KVM 配对：一次性码明文传输、可被抢占　`P2` `[走查]`

### 位置
[kvm-core/src/pairing.rs:328-441](../../crates/kvm-core/src/pairing.rs#L328-L441)（`write_frame(&mut stream, &Frame{ MsgType::PairRequest, payload: serde_json::to_vec(&req)... })`）

### 问题描述
6 位配对码与整个 `PairRequest/PairResponse` 握手走**明文 TCP**（会话密钥在配对完成之后才派生），无 PAKE、无密钥确认。应答端收到带正确码的请求即把对端公钥落盘为受信设备（`:328-335`）。

### 影响
局域网嗅探者在 120s 有效窗口内捕获 `code`，可以**自身公钥**抢先提交 `PairRequest` → 被记为受信设备 → 此后可正常建 K3 会话、注入输入、取剪贴板/文件。

### 解决方案
1. **升级为 PAKE**：用 SPAKE2（`spake2` crate）以 6 位码为低熵口令，握手即在"码"上做认证密钥交换，杜绝"明文码先泄露"。双方在 PAKE 完成后做 key confirmation（互发 HMAC(key, transcript)）。
2. **短期缓解（不改协议）**：
   - 配对仅在用户**同时在两端**点击并显示 6 位码、且**双方指纹短码需人工核对**（UI 上显示 4 组 hex，用户确认后落盘）；
   - 缩短窗口至 60s、限制同 IP 尝试次数（3 次失败即锁）；
   - 配对成功事件强提醒（托盘 + 通知），并允许"立即撤销最近配对"。
3. **落盘绑定**：`paired.json` 记录配对时的双方指纹与时间，配对确认（key confirmation）通过后才写入。

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 码的传输 | 明文帧 | PAKE（码作为低熵秘密） |
| 中间人 | 可抢占配对 | 需破 PAKE |
| 用户可见确认 | 仅显示码 | 指纹人工核对 + 成功强提醒 |
| 失败尝试 | 无限制 | 3 次锁定/限速 |

### 预防措施
1. **配对协议纳入 threat model 文档**（`docs/impl` 下新增 KVM 安全章节：攻击者能力、假设、缓解、测试）。
2. **"低熵秘密不得裸传"作为编解码评审项**：任何以口令/PIN 为核心的安全交换，必须走 PAKE 或先建立加密信道。
3. 增加"抢占"负例测试：模拟平行注入的第二个 `PairRequest`，断言先到者获胜且后到者被拒/告警。

---

# SEC-12 · TUN 模式无自身/宿主排除规则　`P2` `[走查]`

### 位置
[proxy-core/src/service.rs:762-792](../../crates/proxy-core/src/service.rs#L762-L792)（`enter_tun`）

### 问题描述
`enter_tun` 只做能力/权限/wintun 检查与"系统代理必须还原"，全仓在 proxy 域**无任何防火墙规则创建**（grep `firewall|advfirewall|netsh` 无命中）。System 模式有 `<local>` 等 bypass 例外（`sysproxy.rs:63`），TUN 模式没有。

### 影响
依赖内核（sing-box/mihomo）隐式 `strict_route` 规避；若内核未排除宿主，可能出现流量自旋/环路或内核自身请求被自己接管（影响面取决于内核实现，故列 P2）。

### 解决方案
1. `enter_tun` 成功后按内核类型下发最小排除：TUN 网卡（wintun 适配器）直连路由、内核进程排除，或在渲染 IR 时显式加"内核自身出站避让"。
2. 退出/崩溃路径对称撤销（与 `SEC-09` 同一"五件套"要求）。
3. 文档记录：在 `docs/impl` 的代理章节写明"TUN 模式对宿主的排除由内核 X 与本地规则 Y 共同保证"。

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 自身排除 | 依赖内核隐式行为 | 显式规则 + 内核配置双保险 |
| 退出清理 | 未建模 | 对称撤销 |

### 预防措施
把"TUN/虚拟网卡类功能的宿主排除"写进 `DESIGN.md §8` 副作用清单；真机验收项（`§5.4` 类）增加"TUN 开启后本机网络与速度不劣化/无环路"。

---

# SEC-13 · 剪贴板"自写标记"可被任意进程置位（绕过捕获）　`P2` `[实测]`

### 位置
[win-integration/src/clipboard.rs:79,632-663](../../crates/win-integration/src/clipboard.rs#L79)（`has_self_write_marker` 用注册的全局剪贴板格式 `NexusForgeSelf`）

### 问题描述
防循环标记是**全局注册的剪贴板格式**（`RegisterClipboardFormat` 后任何进程都能设置该格式）。`wndproc` 在其中存在时直接 `return`。

### 影响
本机恶意程序在用户复制内容时附上同名格式 → 捕获逻辑静默跳过 → 剪贴板历史里看不到该条目（掩盖窃取痕迹），也可被用于"投毒"（让被窃取的内容不出现在历史中）。

### 解决方案
1. 标记改为**进程内可验证**：写入时同时置一个含进程随机 nonce 的隐藏格式（`NexusForgeSelf:<uuid-v7>`，nonce 存进程内存 `OnceLock<String>`），读取时校验 nonce 匹配。
2. 或放弃格式标记，改用**进程内时间窗 + 内容哈希**比对：`last_self_write = (hash, Instant)`；`WM_CLIPBOARDUPDATE` 到达时若 `hash` 相同且 `Instant` 在 500ms 内 → 视为自写。外部进程无法伪造时间窗与哈希的组合。
3. 保留格式标记作为**辅助信号**（用于日志），但不作为唯一跳过依据。

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 判定依据 | 全局格式存在性（可伪造） | 进程内 nonce / 时间窗+哈希（不可伪造） |
| 绕过难度 | 一次 `SetClipboardData` | 需注入本进程 |
| 误跳过风险 | 外部程序撞名即误跳 | 仅本进程自写才跳过 |

### 预防措施
"防循环/去重标记必须落在自己的信任域内"——写入 IPC、剪贴板、文件监听的"自产标记"都不能放在其他进程可写的空间（全局格式、共享文件、环境变量、临时文件名约定）；同类项：代理/内核 pid 文件、sync 的 origin 标记（应校验为本地枚举值而非对端可设字符串）。

---

# SEC-14 · CSP 缺关键指令（含不回落 default-src 的 `base-uri`）　`P2` `[实测]`

### 位置
[tauri.conf.json:31](../../src-tauri/tauri.conf.json#L31)；测试覆盖缺口 [security_config.rs:149-201](../../src-tauri/tests/security_config.rs#L149-L201)

### 问题描述
现 CSP：`default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src …; font-src …; worker-src …; connect-src …`。
缺 `object-src`、`base-uri`、`form-action`、`frame-src`。其中 **`base-uri` 不回落 `default-src`**（未声明即不受限）。

### 影响
`<base href="http://evil/">` 注入可改写全部相对 URL 解析基准（配合 `SEC-07` 的注入面可劫持资源/表单目标）；`object`/`embed`/`frame` 未禁；表单可外发到任意域。现有测试只断言 7 条指令存在且无 `unsafe-eval`，给出"完整"假象。

### 解决方案

```json
"csp": "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: http://asset.localhost; font-src 'self' data:; worker-src 'self' blob:; connect-src 'self' ipc: http://ipc.localhost; object-src 'none'; base-uri 'self'; form-action 'self'; frame-src 'none'; frame-ancestors 'none'"
```

```rust
// security_config.rs
const REQUIRED_CSP_DIRECTIVES: &[&str] = &[
    "default-src","script-src","style-src","img-src","font-src","worker-src","connect-src",
    "object-src","base-uri","form-action","frame-src",      // ← 新增
];
#[test]
fn csp_has_all_required_directives_and_no_unsafe_eval() { /* 遍历断言 + 断言 object-src 'none'、base-uri 'self' */ }
```

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 指令数 | 7 | 12 |
| `base-uri` | 未声明（无限） | `'self'` |
| `object/embed` | 允许 | `object-src 'none'` |
| 表单外发 | 允许 | `form-action 'self'` |
| iframe | 允许 | `frame-src 'none'` + `frame-ancestors 'none'` |
| 测试 | 7 条存在性 | 12 条 + 关键值断言 |

### 预防措施
把 CSP 指令清单写成常量数组（测试与配置同源），新增/删除指令必须同步；并在 `DESIGN.md §8.5` 记录"style-src 的 `unsafe-inline` 是已知例外（toast 动画运行时 `<style>`，见 `STD-07`），若改 nonce 制需同步去掉该例外"。

---

# SEC-15 · `scoop` 经 `cmd /C` 拼接：元字符注入　`P2` `[走查]`

### 位置
[sys-core/src/pkg.rs:222](../../crates/sys-core/src/pkg.rs#L222)、[:298-301](../../crates/sys-core/src/pkg.rs#L298-L301)、预览 [:325-333](../../crates/sys-core/src/pkg.rs#L325-L333)

### 问题描述
```rust
"scoop" => Ok(("cmd", argv(&["/C", "scoop", "search", q]))),
"scoop" => match action { "install" => Ok(("cmd", argv(&["/C", "scoop", "install", pkg]))), ... }
```
注释与测试（`:706-732`）断言"query 作独立 argv 元素原样传递，零 shell 拼接"——该断言对 `Command` 的**常规**程序成立，但 `cmd.exe` **不使用** `CommandLineToArgvW` 规则，而是自行解析命令行；Rust 只对含空格/制表/引号的参数加引号，故 `git&calc`（无空格）落到 cmd 时 `&` 被当作命令分隔符。

### 影响
`package_id` 来源包括前端 IPC 与 scoop bucket 远端列出的包名（供应链面）→ 可执行任意附加命令（当前用户权限）。

### 解决方案

```rust
// sys-core/src/pkg.rs
/// cmd.exe 元字符黑名单（cmd 不做标准引号解析，必须显式拒绝）
const CMD_METACHARS: &[char] = &['&','|','^','>','<','(',')','%','!','"','\n','\r',';'];

fn reject_cmd_metachars(s: &str, what: &str) -> Result<()> {
    if s.chars().any(|c| CMD_METACHARS.contains(&c)) {
        return Err(SysError::Pkg(format!("{what} 含非法字符: {s}")));
    }
    Ok(())
}

// build_search_args / build_action_args 共用入口处调用
reject_cmd_metachars(q, "查询串")?;      // search
reject_cmd_metachars(pkg, "包名")?;      // install/upgrade/uninstall
```
更彻底的做法：不走 `cmd /C`，改为直接调用 `scoop.cmd` 的绝对路径（`$env:SCOOP\shims\scoop.cmd`）或 `powershell -NoProfile -Command` 并以参数数组传值；若必须经 cmd，则对每个参数额外做"数字/字母/连字符/点/加号/下划线/斜杠"白名单（比黑名单更强）。

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 参数校验 | 无（信任 argv 语义） | cmd 元字符显式拒绝 / 字符白名单 |
| `git&calc` | 执行 `calc` | 拒绝并报错 |
| 包名来源 | 前端 + 远端 bucket | 同左，但注入不可达 |
| 测试 | 断言"零拼接"（不覆盖 cmd 语义） | 新增 `git&calc`、`a|b`、`%TEMP%` 等负例 |

### 预防措施
1. **"shell 语义 ≠ argv 语义"规则**：凡经 `cmd /C`、`powershell -Command`、`sh -c` 的参数，必须走显式白名单；在 `CONTRIBUTING.md` 记录。
2. **CI 断言**：`grep -rn '"cmd"' crates/*/src` 的每一处都必须相邻出现 `reject_cmd_metachars` 或白名单函数（可用源码扫描断言）。
3. **包名来源校验**：把"远端 bucket 返回的包名"纳入"远端数据不可信"清单（同 `SEC-06`）。

---

# SEC-16 · `nf.open_url` 无 scheme 限制 → 任意程序/协议启动　`P2` `[走查]`

### 位置
[automation-core/src/wasm.rs:89-105](../../crates/automation-core/src/wasm.rs#L89-L105)；[win-integration/src/shell.rs:158-174](../../crates/win-integration/src/shell.rs#L158-L174)

### 问题描述
`nf.open_url` → `HostActionHandler::open_url` → `ShellPort::shell_execute` → `ShellExecuteW(None, "open", path, ...)`，**无 scheme/路径校验**。段落注释称"宿主函数白名单"，但白名单成员自身的授权范围无约束。

### 影响
取得 manifest `permissions:["open"]` 的插件（或规则里的 `OpenUrl` 动作）可传任意本地可执行文件、`.lnk`、`file://`、自定义协议（`ms-settings:`、`search-ms:`、`ms-msdt:` 等）→ 程序启动 / 协议处理器滥用（远超"打开 URL"语义）。

### 解决方案

```rust
// automation-core/src/wasm.rs（或更靠下的 ShellPort 契约处）
/// 仅允许浏览器可安全打开的 Web URL
fn is_openable_url(s: &str) -> bool {
    let Ok(u) = url::Url::parse(s) else { return false };
    matches!(u.scheme(), "http" | "https") && u.host_str().is_some()
}
// open_url 实现
let Some(url) = read_guest_str(caller, ptr, len)? else { ... };
if !is_openable_url(&url) {
    return Err(wasmtime::Error::msg("open_url 仅接受 http/https URL"));
}
host.open_url(&url)
```
```rust
// win-integration/src/shell.rs
/// 明确区分两种语义：URL（限 http/https）与可执行文件（需显式高权限 + 确认）
pub fn shell_execute_url(&self, url: &str) -> Result<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(AppError::module("WIN_SHELL_001", "仅允许打开 http/https URL", Some("如需打开本地程序请使用受确认的动作".into())));
    }
    // ShellExecuteW
}
```

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 输入约束 | 任意字符串 | 仅 http/https（含 host 校验） |
| 能力语义 | "open" 覆盖程序与协议 | URL 打开与程序启动分离，后者需显式授权+确认 |
| 插件逃逸面 | 可达宿主启动器 | 仅浏览器打开网页 |

### 预防措施
1. **宿主函数逐个写"授权契约"**：`DESIGN.md`/插件开发文档中为每个 host 函数写明允许的输入集合与失败语义（当前仅"白名单成员"一层）。
2. **插件权限模型细化**：`open` 拆为 `open_url` 与 `launch_app`（后者需要用户确认 + 记录到自动化日志）。
3. 负例测试：`nf.open_url("C:\\Windows\\System32\\calc.exe")`、`nf.open_url("ms-settings:")` 必须被拒。

---

# SEC-17 · 截图文件名模板可含分隔符 → 写出保存目录　`P2` `[走查]`

### 位置
[screenshot-core/src/util.rs:190-213](../../crates/screenshot-core/src/util.rs#L190-L213)（`resolve_filename`）；[screenshot-core/src/module.rs:1275-1286](../../crates/screenshot-core/src/module.rs#L1275-L1286)（`action_save`）

### 问题描述
`filename_template` 由用户配置，`resolve_filename` 只替换 `{ts}`/`{fmt}`，不剥离 `/\`，`action_save` 直接 `dir.join(format!("{stem}{dot_ext}"))`。

### 影响
模板含 `..\..\` 时截图写到 `save_dir` 之外（虽属用户自身配置，但违反"写盘白名单"纪律，且配置文件可能被同步/导入）。

### 解决方案
```rust
// screenshot-core/src/util.rs
pub fn resolve_filename(template: &str, ts: i64, fmt: &str) -> Result<String> {
    let stem = template.replace("{ts}", &fmt_ts(ts)).replace("{fmt}", fmt);
    // 强制 basename：剥离任何路径成分，并拒绝 .. / 分隔符 / 保留名
    if stem.contains(['/', '\\']) || stem.contains("..") || stem.is_empty() {
        return Err(ScreenshotError::Config(format!("文件名模板含非法字符: {stem}")));
    }
    let stem = stem.trim_matches([' ', '.']);   // Windows 尾部点/空格会被静默剥离
    if stem.is_empty() { return Err(ScreenshotError::Config("文件名模板为空".into())); }
    Ok(stem)
}
```

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 模板校验 | 无 | 拒分隔符/`..`/空；尾部空格与点归一 |
| 副作用范围 | 可越出 `save_dir` | 严格限于 `save_dir` |
| 配置导入 | 可携带穿越模板 | 导入即被拒并提示 |

### 预防措施
统一使用 `SEC-05` 的 `safe_rel_path`/basename 工具；把所有"用户可配置的模板/命名规则"列入路径安全清单（含 `file-core` 重命名模板——见 `COR-16` 同一族问题的 `rename.rs`）。

---

# SEC-18 · OCR 临时明文 PNG 残留（无启动清理）　`P2` `[走查]`

### 位置
[ocr-core/src/tesseract.rs:296-312](../../crates/ocr-core/src/tesseract.rs#L296-L312)

### 问题描述
每次识别把整帧 PNG **明文**写到 `{app_data}/ocr-tmp/{uuid}.png`，成败都 `remove_file`（unlink，不覆写）；**OCR 模块 init/start 无 `ocr-tmp` 清理**（截图模块对 `frames/` 有 `sweep_leftover_frames`，init:317）。

### 影响
崩溃/强杀/断电后用户屏幕截图明文永久驻留磁盘（unlink 后仍可被文件恢复工具还原）。

### 解决方案
1. `OcrModule::init` 增加启动清空（对齐截图 sweep）：
```rust
fn sweep_leftover_tmp(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let _ = std::fs::remove_file(&p);      // 或走覆写删除（见 §下）
            tracing::debug!(path = %p.display(), "清理 OCR 临时文件");
        }
    }
}
```
2. 删除语义对齐剪贴板 `remove_blob` 的**覆写后 unlink**（若判定截图为敏感数据）。
3. 更优：**不落盘** —— tesseract 支持 stdin/stdout（`-` 作为输入输出），改为管道传图，从根上消除临时文件。
4. 目录权限收紧（仅当前用户），并纳入"敏感临时目录清单"。

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 崩溃残留 | 永久保留明文 PNG | 启动即清理（并可覆写） |
| 删除方式 | unlink（可恢复） | 覆写后 unlink（不可恢复） |
| 落盘 | 必经磁盘 | 方案 3 后完全不落盘 |

### 预防措施
把所有"为调用外部工具而落盘的中间产物"列入清单（OCR、PDF 转换、压缩、水印、WASM 插件临时目录），统一要求：`init` 清理 + 覆写删除 + 目录权限 + 优选管道/内存实现。

---

# SEC-19 · Helper 看门狗可能在耗时操作中退出　`P2` `[走查]`

### 位置
[nexusforge-helper/src/main.rs:51-63](../../crates/nexusforge-helper/src/main.rs#L51-L63)；`BUSY` 置位点 [dispatch.rs:183,192,209](../../crates/nexusforge-helper/src/dispatch.rs#L183)

### 问题描述
```rust
if dispatch::BUSY.load(Ordering::Relaxed) { continue; }
if idle > idle_secs as u64 * 1000 { std::process::exit(0); }
```
`BUSY` 仅在 `exec`/`dism`/`sfc`/`defender`/`restore_point` 置位；`file.clean_dir` 等耗时方法不在保护内。

### 影响
清理/删除过程中 helper 可能 `exit(0)` → 半清理状态；若正在 `pipe_*` 交互则连接中断，上层看到"进程消失"而非明确错误。

### 解决方案
1. 所有可能超过空闲窗口的方法统一经 `with_busy` 包裹（RAII）：
```rust
struct BusyGuard;
impl BusyGuard { fn new() -> Self { BUSY.store(true, Ordering::SeqCst); Self } }
impl Drop for BusyGuard { fn drop(&mut self) { BUSY.store(false, Ordering::SeqCst); } }
// handle_request 入口：
let _busy = BusyGuard::new();       // 覆盖全部方法，无需逐方法记得置位
```
2. 看门狗改为"仅在无活动连接且无在途请求时退出"（维护 `active_conns: AtomicUsize`）。
3. 退出前落一条审计日志（谁在何时用了 helper、做了什么）。

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| BUSY 覆盖 | 5 个方法手工置位 | RAII 全量覆盖 |
| 退出条件 | 仅看 BUSY + 空闲 | 无连接且无在途请求 |
| 中断后果 | 半清理 | 不中断 |

### 预防措施
"生命周期与忙碌标记用 RAII 而非手工配对"（项目已有同类教训：SessionHandle Drop、DispatcherGuard 计数）；新增 helper 方法时必须复用该守卫。

---

# SEC-20 · Helper 自建 `Result<_, String>` 错误面　`P3` `[实测]`

**位置**：[dispatch.rs:75](../../crates/nexusforge-helper/src/dispatch.rs#L75)（`handle_request(...) -> Result<Value, String>`）、[main.rs:85,116](../../crates/nexusforge-helper/src/main.rs#L85)。

**问题**：跨进程 JSON-RPC 层用裸 `String` 错误，偏离"统一 AppError 错误码体系"硬约束（同族问题此前已在 `kvm-core/identity.rs` 被点名）。

**解决方案**：
1. helper 内部仍用 `AppError`，序列化为 `{code, message, hint}` 放进 `error` 字段；上层 `helper_call` 反序列化回 `AppError` 并保留 `code`。
2. 或（更轻）在 `DECISIONS.md` 明确登记"helper 进程为错误体系豁免项"，并在 `src-tauri` 侧统一包装为 `AppError::module("HELPER_...", ...)`，保证前端拿到的永远是 `AppError`。

**前后对比**：修复前前端收到 `String`（无法按 code 分支/判断可重试）；修复后统一 `AppError`，可复用 `parseAppError`。

**预防**：`security_config.rs` 式源码扫描断言"`pub fn ... -> Result<_, String>` 仅允许出现在 `#[cfg(test)]`"（或维护一份显式豁免清单）。

---

# SEC-21 · 剪贴板自写标记机制（规范面）　`P3` `[实测]`

与 `SEC-13` 同源。定位：[clipboard.rs:632-663](../../crates/win-integration/src/clipboard.rs#L632-L663)。解决方案与预防措施见 `SEC-13`；此条仅为"机制性缺陷"的规范登记（任何"存在性即信任"的全局标记都应替换为"可验证凭证"）。

---

# SEC-22 · 非回环 FTP 明文口令（设计取舍登记）　`P3` `[走查]`

**位置**：[ftp.rs:443-453](../../crates/file-core/src/remote/ftp.rs#L443-L453)（`USER`/`PASS` 明文；`sanitize_arg` 只防 CRLF 注入）。

**现状与评估**：项目已设三闸（总闸 006 / 逐次确认 008），属"用户同意"而非"传输保护"。属已知设计取舍。

**解决方案**：
1. 对非回环 FTP 的 `PromptEachTime` 档加强告警文案（明确"口令将以明文发送"）；
2. 或产品层策略：仅允许匿名/回环 FTP，非回环要求显式开关（默认关）并在设置中标注风险；
3. 长线建议：FTPS/SFTP 优先，FTP 标记为"不推荐"。

**前后对比**：修复前文案未指明明文风险；修复后风险显式呈现，且有默认安全策略。

**预防**：所有"降级协议"（HTTP/FTP/Telnet 类）都要有"默认关闭 + 显式风险告知 + 文档标注"，并在 `DESIGN.md §8` 建立降级协议清单。