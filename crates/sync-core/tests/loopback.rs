//! SYNC 回环验收（docs/impl/07 SYNC 验收）：同机双实例，双向变更收敛。
//!
//! 两台 SyncModule（独立临时 appData + 互配信任根 + 假数据集 applier）：
//! A 记录变更 → sync_with(B) → B 数据集收敛 → B 记录变更 → A 反向拉取收敛。

use parking_lot::Mutex;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

use host_core::device::{b64_encode, DeviceIdentity, PairStore, PairedPeer};
use host_core::events::EventBus;
use host_core::module::{Module, ModuleContext};
use host_core::ports::Ports;
use sync_core::engine::ChangeApplier;
use sync_core::{OpEntry, OpLog, SyncEngine, SyncModule};

/// 回环端口（与 KVM 49800/49801、K9 测试 49810–49812 错开；固定端口进程内单用）
const LOOPBACK_PORT: u16 = 49831;
const CURSOR_PORT: u16 = 49832;
const SECOND_ROUND_PORT: u16 = 49833;
const REPLAY_PORT: u16 = 49834;

fn addr(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

/// 内存数据集投影
#[derive(Default)]
struct FakeStore {
    data: Mutex<std::collections::HashMap<String, serde_json::Value>>,
}
impl FakeStore {
    fn put(&self, id: &str, content: &str) {
        self.data
            .lock()
            .insert(id.to_string(), serde_json::json!({ "content": content }));
    }
    fn get(&self, id: &str) -> Option<serde_json::Value> {
        self.data.lock().get(id).cloned()
    }
    fn len(&self) -> usize {
        self.data.lock().len()
    }
}
impl ChangeApplier for FakeStore {
    fn snapshot(&self, _entity: &str, id: &str) -> sync_core::Result<Option<serde_json::Value>> {
        Ok(self.get(id))
    }
    fn apply_upsert(
        &self,
        _entity: &str,
        id: &str,
        value: &serde_json::Value,
    ) -> sync_core::Result<()> {
        self.data.lock().insert(id.to_string(), value.clone());
        Ok(())
    }
    fn apply_delete(&self, _entity: &str, id: &str) -> sync_core::Result<()> {
        self.data.lock().remove(id);
        Ok(())
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nf_sync_loop_{tag}_{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 装配一台可同步的实例：预置身份 + 互配记录 → init + start；返回真实身份供配对
fn setup(
    tag: &str,
    peer_record: Option<PairedPeer>,
    port: u16,
) -> (Arc<SyncModule>, Arc<FakeStore>, Arc<DeviceIdentity>) {
    let dir = temp_dir(tag);
    let kvm_dir = dir.join("kvm");
    let identity = Arc::new(DeviceIdentity::load_or_create(&kvm_dir, None).unwrap());
    if let Some(p) = peer_record {
        PairStore::load_or_default(&kvm_dir)
            .unwrap()
            .upsert(p)
            .unwrap();
    }
    let module = Arc::new(SyncModule::new(&dir));
    let store = Arc::new(FakeStore::default());
    module.attach_applier(store.clone());
    let ctx = Arc::new(ModuleContext {
        app_data_dir: dir,
        ports: Arc::new(Ports::new()),
        event_bus: Arc::new(EventBus::new()),
    });
    module.init(ctx).unwrap();
    module.set_port(port);
    module.start().unwrap();
    (module, store, identity)
}

fn record_of(id: &DeviceIdentity) -> PairedPeer {
    PairedPeer {
        device_id: id.device_id.clone(),
        device_name: id.device_name.clone(),
        fingerprint: id.pubkey_fingerprint.clone(),
        pubkey_b64: b64_encode(&id.public_key()),
        paired_at: 0,
    }
}

/// 双向互信的一对实例（生产中由 KVM 配对双向落盘）
struct Cluster {
    a: Arc<SyncModule>,
    a_store: Arc<FakeStore>,
    a_id: String,
    b: Arc<SyncModule>,
    b_store: Arc<FakeStore>,
    b_id: String,
}

fn paired(tag: &str, port: u16) -> Cluster {
    let (b, b_store, id_b) = setup(&format!("{tag}_b"), None, port);
    let (a, a_store, id_a) = setup(&format!("{tag}_a"), Some(record_of(&id_b)), port);
    b.register_peer(record_of(&id_a));
    Cluster {
        a,
        a_store,
        a_id: id_a.device_id.clone(),
        b,
        b_store,
        b_id: id_b.device_id.clone(),
    }
}

/// 等待 B 的监听就绪（accept_loop 异步 bind；探测连接会被握手超时丢弃，无副作用）
async fn wait_port(port: u16) {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("SYNC 监听端口 {port} 未就绪");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_instances_converge_bidirectionally() {
    // B 先起（服务端监听），A 侧配对记录指向 B 的真实身份
    let (module_b, store_b, id_b) = setup("b", None, LOOPBACK_PORT);
    let record_b = PairedPeer {
        device_id: id_b.device_id.clone(),
        device_name: id_b.device_name.clone(),
        fingerprint: id_b.pubkey_fingerprint.clone(),
        pubkey_b64: b64_encode(&id_b.public_key()),
        paired_at: 0,
    };
    let (module_a, store_a, id_a) = setup("a", Some(record_b), LOOPBACK_PORT);
    // B 侧补登记 A（双向互信；生产中由 KVM 配对双向落盘）
    module_b.register_peer(PairedPeer {
        device_id: id_a.device_id.clone(),
        device_name: id_a.device_name.clone(),
        fingerprint: id_a.pubkey_fingerprint.clone(),
        pubkey_b64: b64_encode(&id_a.public_key()),
        paired_at: 0,
    });
    let b_id = id_b.device_id.clone();

    // 1. A 本地创建笔记 → 入 op_log
    store_a.put("n.md", "from A");
    module_a.record_change("n.md", "create").unwrap();

    // 2. A → B 同步：B 收敛
    wait_port(LOOPBACK_PORT).await;
    let s1 = match module_a.sync_with(&b_id, &addr(LOOPBACK_PORT)).await {
        Ok(s) => s,
        Err(e) => panic!("首次同步失败: {e:?}"),
    };
    assert_eq!(s1.pushed, 1, "A 应推送 1 条自产变更");
    assert_eq!(store_b.get("n.md").unwrap()["content"], "from A");

    // 3. 重复同步：出站游标已对齐 ⇒ 零推送（T-B5-1 前此项恒为"全量自产 1"，即缺陷①的表述面）
    let s2 = module_a
        .sync_with(&b_id, &addr(LOOPBACK_PORT))
        .await
        .unwrap();
    assert_eq!(s2.pushed, 0, "已推给该对端的变更不再重推");
    assert_eq!(s2.pulled_applied, 0, "无新变更时拉取为空");

    // 4. B 本地修改 → A 反向拉取收敛
    store_b.put("n.md", "from B");
    module_b.record_change("n.md", "write").unwrap();
    let s3 = module_a
        .sync_with(&b_id, &addr(LOOPBACK_PORT))
        .await
        .unwrap();
    assert_eq!(s3.pulled_applied, 1, "A 应拉取 B 的 1 条新变更");
    assert_eq!(store_a.get("n.md").unwrap()["content"], "from B");

    // 5. 删除传播：A 删除 → B 收敛删除
    module_a.record_change("n.md", "delete").unwrap();
    module_a
        .sync_with(&b_id, &addr(LOOPBACK_PORT))
        .await
        .unwrap();
    assert!(store_b.get("n.md").is_none(), "B 侧应被删除");
}

/// 任务书（09 §10.2 T-B5-1）字面测试名优先于 rustc 命名惯例
/// 缺陷①主证：本地自产变更超过单批上限时，发起方按出站游标分批续推，对端收满最新那批
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn runInitiator_moreThanBatchLimit_pushesAllNewestOps() {
    const TOTAL: i64 = 600; // BATCH_LIMIT(512) + 88
    let c = paired("batch", CURSOR_PORT);
    wait_port(CURSOR_PORT).await;

    let log_a = OpLog::open(c.a.db_path()).unwrap();
    for i in 0..TOTAL {
        SyncEngine::record_local(
            &log_a,
            "note",
            &format!("n{i}.md"),
            json!({ "content": format!("v{i}") }),
            &c.a_id,
            1_000 + i,
        )
        .unwrap();
    }

    let s = c.a.sync_with(&c.b_id, &addr(CURSOR_PORT)).await.unwrap();
    assert_eq!(s.pushed as i64, TOTAL, "600 条应分 512+88 两批全部推出");
    assert_eq!(c.b_store.len(), TOTAL as usize, "对端收满");
    assert_eq!(
        c.b_store.get("n599.md").unwrap()["content"],
        "v599",
        "最新那条必须在场（缺陷①的症状正是最新变更永不同步）"
    );
    assert_eq!(
        log_a.push_cursor(&c.b_id),
        1_000 + TOTAL - 1,
        "两批的 Ack.until_ts 逐批把出站游标推到最新一条"
    );
}

/// 任务书（09 §10.2 T-B5-1）字面测试名优先于 rustc 命名惯例
/// 游标已对齐的第二次会话必须零推送（防空转：全量重推是缺陷①的另一面）
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn runInitiator_secondRoundPushesZero() {
    let c = paired("second", SECOND_ROUND_PORT);
    wait_port(SECOND_ROUND_PORT).await;

    c.a_store.put("s1.md", "one");
    c.a.record_change("s1.md", "create").unwrap();
    let s1 =
        c.a.sync_with(&c.b_id, &addr(SECOND_ROUND_PORT))
            .await
            .unwrap();
    assert_eq!(s1.pushed, 1);

    let s2 =
        c.a.sync_with(&c.b_id, &addr(SECOND_ROUND_PORT))
            .await
            .unwrap();
    assert_eq!(s2.pushed, 0, "无新自产变更 ⇒ 出站批为空");
    assert_eq!(s2.pulled_applied, 0);

    // 游标推进后新增的变更仍可出账（不是"一次性关掉推送"）
    c.a_store.put("s2.md", "two");
    c.a.record_change("s2.md", "create").unwrap();
    let s3 =
        c.a.sync_with(&c.b_id, &addr(SECOND_ROUND_PORT))
            .await
            .unwrap();
    assert_eq!(s3.pushed, 1);
    assert_eq!(c.b_store.get("s2.md").unwrap()["content"], "two");
}

/// 任务书（09 §10.2 T-B5-1）字面测试名优先于 rustc 命名惯例
/// 旧库无 push_cursors 行 ⇒ 首轮从 0 全量重放；对端已有同 op 时条目数不得长出第二行
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn loopback_replayFromEmptyPushCursor_isIdempotent() {
    let c = paired("replay", REPLAY_PORT);
    wait_port(REPLAY_PORT).await;

    let log_a = OpLog::open(c.a.db_path()).unwrap();
    let log_b = OpLog::open(c.b.db_path()).unwrap();
    for i in 0..5i64 {
        let e = OpEntry {
            op_id: format!("replay{i}"),
            entity: "note".into(),
            entity_id: format!("r{i}.md"),
            ts: 1_000 + i,
            device: c.a_id.clone(),
            value: json!({ "content": "same" }),
        };
        log_a.append(&e).unwrap();
        log_b.append(&e).unwrap();
        c.b_store
            .apply_upsert("note", &e.entity_id, &e.value)
            .unwrap();
    }
    assert_eq!(log_a.count(), 5);
    assert_eq!(
        log_a.push_cursor(&c.b_id),
        0,
        "旧库无 push_cursors 行 ⇒ 出站游标按 0 起"
    );

    let s = c.a.sync_with(&c.b_id, &addr(REPLAY_PORT)).await.unwrap();
    assert_eq!(s.pushed, 5, "全量重放（重推无害是本行判据）");
    assert_eq!(s.pulled_applied, 0);
    assert_eq!(
        log_b.count(),
        5,
        "对端条目数与首轮一致：op_id 主键幂等 + Noop 双保险"
    );
    assert_eq!(c.b_store.len(), 5);
    assert_eq!(
        log_a.push_cursor(&c.b_id),
        1_004,
        "对端回执 until_ts 落到出站游标"
    );

    // 重放后游标已对齐 ⇒ 第二轮不再重推
    let s2 = c.a.sync_with(&c.b_id, &addr(REPLAY_PORT)).await.unwrap();
    assert_eq!(s2.pushed, 0);
}
