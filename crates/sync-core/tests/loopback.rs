//! SYNC 回环验收（docs/impl/07 SYNC 验收）：同机双实例，双向变更收敛。
//!
//! 两台 SyncModule（独立临时 appData + 互配信任根 + 假数据集 applier）：
//! A 记录变更 → sync_with(B) → B 数据集收敛 → B 记录变更 → A 反向拉取收敛。
//! T-B5-3 起还验收话流水：每轮同步（含失败）在两侧库里都留得下行与计数。
//! T-B5-6 起还验自动出账的到期语义：没有地址事实源就记失败不记成功，暂停位一处写两处读。

use parking_lot::Mutex;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

use host_core::device::{b64_encode, DeviceIdentity, PairStore, PairedPeer};
use host_core::events::EventBus;
use host_core::module::{Module, ModuleContext};
use host_core::ports::Ports;
use sync_core::engine::ChangeApplier;
use sync_core::oplog::{ROLE_INITIATOR, ROLE_RESPONDER};
use sync_core::{
    is_sync_entity, OpEntry, OpLog, SyncEngine, SyncError, SyncModule, SyncRun, ENTITY_NOTE,
    SYNC_ENTITIES,
};

/// 回环端口（与 KVM 49800/49801、K9 测试 49810–49812 错开；固定端口进程内单用）
const LOOPBACK_PORT: u16 = 49831;
const CURSOR_PORT: u16 = 49832;
const SECOND_ROUND_PORT: u16 = 49833;
const REPLAY_PORT: u16 = 49834;
const RUN_ROW_PORT: u16 = 49835;
const RUN_PEER_PORT: u16 = 49836;
const RUN_PENDING_PORT: u16 = 49837;
const STATUS_OK_PORT: u16 = 49838;
const STATUS_BUSY_PORT: u16 = 49839;
const STATUS_PEER_PORT: u16 = 49840;
const STATUS_SHAPE_PORT: u16 = 49841;
const VAULT_NEVER_PORT: u16 = 49842;
/// T-B5-6 自动同步两枚（与上方各段错开）
const NOADDR_PORT: u16 = 49843;
const PAUSE_PORT: u16 = 49844;
/// 无人监听的端口（connect 立刻被拒，用来造"注定失败的一轮"）
const DEAD_PORT: u16 = 49899;

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
    module
        .attach_applier(ENTITY_NOTE, store.clone())
        .expect("note 应用器装配（白名单内）");
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
    module_a
        .record_change(ENTITY_NOTE, "n.md", "create")
        .unwrap();

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
    module_b
        .record_change(ENTITY_NOTE, "n.md", "write")
        .unwrap();
    let s3 = module_a
        .sync_with(&b_id, &addr(LOOPBACK_PORT))
        .await
        .unwrap();
    assert_eq!(s3.pulled_applied, 1, "A 应拉取 B 的 1 条新变更");
    assert_eq!(store_a.get("n.md").unwrap()["content"], "from B");

    // 5. 删除传播：A 删除 → B 收敛删除
    module_a
        .record_change(ENTITY_NOTE, "n.md", "delete")
        .unwrap();
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
    c.a.record_change(ENTITY_NOTE, "s1.md", "create").unwrap();
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
    c.a.record_change(ENTITY_NOTE, "s2.md", "create").unwrap();
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

/// 当前最新流水行 id（等"比它更新的一行"用；空表记 0）
fn newest_id(log: &OpLog) -> i64 {
    log.runs(1).unwrap().first().map(|r| r.id).unwrap_or(0)
}

/// 被动一侧的会话在发起方返回后可能还差最后一笔落盘，按 (role, peer) 等它出现。
/// 只等"这一轮的 responder 行"，不把 sleep 当断言（超时即红，不假绿）。
async fn wait_run_row(log: &OpLog, peer: &str, after_id: i64) -> SyncRun {
    for _ in 0..100 {
        if let Some(r) = log
            .runs(50)
            .unwrap()
            .into_iter()
            .find(|r| r.role == ROLE_RESPONDER && r.peer == peer && r.id > after_id)
        {
            return r;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("被动侧流水没落盘（peer={peer}）");
}

/// 任务书（09 §10.2 T-B5-3）字面测试名优先于 rustc 命名惯例
/// 红线"失败不静默"：连不上/没配对的尝试也要在流水里留得下行，且 error 存展示原文
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn syncRun_errorPath_stillRecorded() {
    let c = paired("errpath", RUN_ROW_PORT);
    wait_port(RUN_ROW_PORT).await;
    let log_a = OpLog::open(c.a.db_path()).unwrap();
    assert!(
        log_a.runs(10).unwrap().is_empty(),
        "还没同步过 ⇒ 不该有凭空出现的流水行"
    );

    // ① 还没开会话就失败（未配对）：过去这种失败只有一行 warn 就上抛了
    let e1 = c.a.sync_with("ghost", &addr(DEAD_PORT)).await.unwrap_err();
    // ② 连不上对端（无监听）
    let e2 = c.a.sync_with(&c.b_id, &addr(DEAD_PORT)).await.unwrap_err();

    let runs = log_a.runs(10).unwrap();
    assert_eq!(runs.len(), 2, "两笔失败 = 两行流水（失败不静默）");
    assert_eq!(runs[0].peer, c.b_id, "peer 记用户点的那台设备 id");
    assert_eq!(runs[0].role, ROLE_INITIATOR);
    assert_eq!(
        runs[0].error.as_deref(),
        Some(e2.to_string().as_str()),
        "error 列存的就是上抛给调用方的那句原文"
    );
    assert_eq!(runs[1].peer, "ghost");
    assert!(runs[1]
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("未配对"));
    for r in &runs {
        assert_eq!(r.pushed, 0, "没推出去就是 0，不猜");
        assert!(r.duration_ms >= 0, "耗时不可能是负数");
    }
    // 记账不是吞错的替代品：两处失败照样如实上抛
    assert!(e1.to_string().contains("未配对"));
}

/// 任务书（09 §10.2 T-B5-3）字面测试名优先于 rustc 命名惯例
/// 承重④的对称面：被动一侧的结果本机可查；且被动供数也推进出站游标（每 peer 进度事实源）
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn syncRun_responderSideRecorded() {
    let c = paired("resprow", RUN_PEER_PORT);
    wait_port(RUN_PEER_PORT).await;
    let log_a = OpLog::open(c.a.db_path()).unwrap();
    let log_b = OpLog::open(c.b.db_path()).unwrap();

    // 第一轮：A 推 2 条给 B（B 侧 responder 行 pulled_applied=2，自己没东西可推）
    c.a_store.put("a1.md", "one");
    c.a.record_change(ENTITY_NOTE, "a1.md", "create").unwrap();
    c.a_store.put("a2.md", "two");
    c.a.record_change(ENTITY_NOTE, "a2.md", "create").unwrap();
    let before_b = newest_id(&log_b);
    let s1 = c.a.sync_with(&c.b_id, &addr(RUN_PEER_PORT)).await.unwrap();
    assert_eq!(s1.pushed, 2);
    let r1 = wait_run_row(&log_b, &c.a_id, before_b).await;
    assert_eq!(
        r1.error, None,
        "被动侧本轮是成功的（T-B5-1 落地补记④：修前发起方从不回 Ack，这一行恒为 EOF 失败）"
    );
    assert_eq!(r1.pulled_applied, 2, "被动侧知道自己应用了几条");
    assert_eq!(r1.pushed, 0, "B 这一轮没有自产变更可回");
    assert_eq!(r1.conflicts, 0);

    // 第二轮：B 改一篇 → A 拉走 ⇒ B 的 pushed 与出站游标都要动起来
    c.b_store.put("b1.md", "from B");
    c.b.record_change(ENTITY_NOTE, "b1.md", "create").unwrap();
    let b_ts = log_b
        .ops_of_device(&c.b_id, 0, 10)
        .unwrap()
        .last()
        .expect("B 的自产 op 在册")
        .ts;
    let before_b = newest_id(&log_b);
    let s2 = c.a.sync_with(&c.b_id, &addr(RUN_PEER_PORT)).await.unwrap();
    assert_eq!(s2.pushed, 0);
    assert_eq!(s2.pulled_applied, 1);
    let r2 = wait_run_row(&log_b, &c.a_id, before_b).await;
    assert_eq!(r2.error, None);
    assert_eq!(
        r2.pushed, 1,
        "Pull 应答方向回的那一批也是推送（否则被动侧流水在说谎）"
    );
    assert_eq!(
        log_b.push_cursor(&c.a_id),
        b_ts,
        "对端回执 after 被动供数也要落进出站游标：否则 B 视角的 pending 永久虚高"
    );
    assert_eq!(
        log_b
            .count_ops_after(&c.b_id, log_b.push_cursor(&c.a_id))
            .unwrap(),
        0,
        "游标一推进，pending 归零"
    );
    // 发起侧同样各留一行（A 主动两轮 ⇒ 两行 initiator）
    let a_rows: Vec<_> = log_a
        .runs(20)
        .unwrap()
        .into_iter()
        .filter(|r| r.role == ROLE_INITIATOR)
        .collect();
    assert_eq!(a_rows.len(), 2);
}

/// 任务书（09 §10.2 T-B5-3）字面测试名优先于 rustc 命名惯例
/// pending 算式与真实 pushed 一致（承重③：面板上"还欠对端几条"必须是真的欠）
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn countOpsAfter_matchesPushedSemantics() {
    let c = paired("pending", RUN_PENDING_PORT);
    wait_port(RUN_PENDING_PORT).await;
    let log_a = OpLog::open(c.a.db_path()).unwrap();
    let pending = |log: &OpLog| {
        log.count_ops_after(&c.a_id, log.push_cursor(&c.b_id))
            .unwrap()
    };

    for (i, name) in ["p1.md", "p2.md", "p3.md"].iter().enumerate() {
        c.a_store.put(name, &format!("v{i}"));
        c.a.record_change(ENTITY_NOTE, name, "create").unwrap();
    }
    assert_eq!(pending(&log_a), 3, "三条自产变更还没推给 B");

    let s =
        c.a.sync_with(&c.b_id, &addr(RUN_PENDING_PORT))
            .await
            .unwrap();
    assert_eq!(
        s.pushed as u64, 3,
        "pending 算式与真实 pushed 必须数到同一批东西"
    );
    assert_eq!(pending(&log_a), 0, "推完即不欠");

    // 新增一条 ⇒ 又欠一条（不是"首轮清零后永远清零"）
    c.a_store.put("p4.md", "v4");
    c.a.record_change(ENTITY_NOTE, "p4.md", "create").unwrap();
    assert_eq!(pending(&log_a), 1);
    let s2 =
        c.a.sync_with(&c.b_id, &addr(RUN_PENDING_PORT))
            .await
            .unwrap();
    assert_eq!(s2.pushed, 1);
    assert_eq!(pending(&log_a), 0);

    // 算式按产出设备分维：对端产出的条目会经拉取进了本机库（入站），但不计进"我欠对端"
    c.b_store.put("q1.md", "b owns it");
    c.b.record_change(ENTITY_NOTE, "q1.md", "create").unwrap();
    let s3 =
        c.a.sync_with(&c.b_id, &addr(RUN_PENDING_PORT))
            .await
            .unwrap();
    assert_eq!(s3.pulled_applied, 1, "B 的这条经拉取进了本机库");
    assert_eq!(s3.pushed, 0, "拉对端的东西不改变本机欠对端多少");
    assert_eq!(
        log_a.count_ops_after(&c.b_id, 0).unwrap(),
        1,
        "入站条目按产出设备（B）计数，就在本机库里"
    );
    assert_eq!(pending(&log_a), 0, "对端自己产的东西不算本机未出账");
}

// ======================== T-B5-4：状态读面类型化 + 监听真态 ========================

/// 有界轮询取一次"监听事实已落地"的状态快照。
///
/// bind 发生在 `start()` spawn 的任务里，与测试线程天然并发；不轮询就等于把竞态写进
/// 断言（要么偶发红，要么靠 sleep 时长许愿）。失败时把最后一次快照如实打进 panic 消息。
async fn wait_bind_fact(module: &SyncModule) -> sync_core::SyncStatus {
    let mut last = None;
    for _ in 0..100 {
        let st = module.status().unwrap();
        if st.listening || st.last_bind_error.is_some() {
            return st;
        }
        last = Some(st);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("监听事实从未落地（最后一次：{last:?}）");
}

/// 任务书（09 §10.2 T-B5-4）字面测试名优先于 rustc 命名惯例
/// 真占端口造红：bind 失败后 `listening` 必须 false 且错误点名端口号
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn status_listeningFalse_whenPortAlreadyBound() {
    // 先把端口真占住（accept_loop 的 bind 必然失败，不是模拟返回值）
    let blocker = std::net::TcpListener::bind(("0.0.0.0", STATUS_BUSY_PORT)).unwrap();
    let (module, _store, _id) = setup("status_busy", None, STATUS_BUSY_PORT);

    let st = wait_bind_fact(&module).await;
    assert!(
        !st.listening,
        "端口被占时不得报\"在听\"（修前：start 无条件置 Running + 面板直读 port）"
    );
    let err = st
        .last_bind_error
        .clone()
        .expect("bind 失败原因要如实可读，不是只剩一个 false");
    assert!(
        err.contains(&STATUS_BUSY_PORT.to_string()),
        "错误消息须点名端口号，面板才能说清是谁占了：{err}"
    );
    assert_eq!(
        st.port, STATUS_BUSY_PORT,
        "未监听也要如实报出口端口（主动同步这条腿仍然可用）"
    );

    drop(blocker);
}

/// 正对照防空洞：若只测失败臂，`listening` 写成常量 false 也能绿
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn status_listeningTrue_afterSuccessfulBind() {
    let (module, _store, id) = setup("status_ok", None, STATUS_OK_PORT);

    let st = wait_bind_fact(&module).await;
    assert!(st.listening, "bind 成功即真在听");
    assert!(
        st.last_bind_error.is_none(),
        "成功臂不得留下上一轮的失败残文：{:?}",
        st.last_bind_error
    );
    assert_eq!(st.port, STATUS_OK_PORT);
    assert_eq!(st.op_count, 0, "全新实例零变更");
    assert_eq!(st.self_device_id, id.device_id);
    assert!(!st.self_name.is_empty());
    assert!(
        !st.paused,
        "暂停位自 T-B5-6 起是真开关，但新实例的初值必须是\"没暂停\"（不谎报）"
    );
    assert!(
        !st.auto_sync,
        "自动同步默认关（本批上线行为零变化的负对照）"
    );
    assert!(st.peers.is_empty(), "未配对 ⇒ 无进度行");
}

/// 任务书字面测试名：两维游标（入站/出站）+ pending 一致，且三个数各自对得上表里的真值
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn status_peersCarryCursorsAndPending() {
    let c = paired("status_peer", STATUS_PEER_PORT);
    wait_port(STATUS_PEER_PORT).await;
    let log_a = OpLog::open(c.a.db_path()).unwrap();
    let peer_of = |st: &sync_core::SyncStatus| {
        st.peers
            .iter()
            .find(|p| p.device_id == c.b_id)
            .expect("配对设备在状态读面上")
            .clone()
    };

    // 还没同步过：三条自产变更全欠着，两维游标都是 0，没有流水行
    for name in ["s1.md", "s2.md", "s3.md"] {
        c.a_store.put(name, "v");
        c.a.record_change(ENTITY_NOTE, name, "create").unwrap();
    }
    let p0 = peer_of(&c.a.status().unwrap());
    assert!(!p0.device_name.is_empty(), "面板显示的是设备名，不是裸 id");
    assert!(!p0.fingerprint.is_empty());
    assert_eq!(p0.pending_ops, 3, "面板上的\"还欠几条\"必须是真的欠");
    assert_eq!(p0.push_cursor, 0, "一次没推 ⇒ 出站游标为零");
    assert_eq!(p0.inbound_cursor, 0, "一次没收 ⇒ 入站游标为零");
    assert_eq!(
        p0.last_sync_ms, 0,
        "从没同步过就是从没同步过，不拿 now_ms 凑数"
    );
    assert!(p0.last_error.is_none());

    // 推完：pending 归零，出站游标 == 表里的出站游标 == 本机最后一条自产 op 的 ts
    let s1 =
        c.a.sync_with(&c.b_id, &addr(STATUS_PEER_PORT))
            .await
            .unwrap();
    assert_eq!(s1.pushed, 3);
    let p1 = peer_of(&c.a.status().unwrap());
    assert_eq!(p1.pending_ops, 0);
    assert_eq!(
        p1.push_cursor,
        log_a.push_cursor(&c.b_id),
        "出站游标读自表，不是另算一份"
    );
    assert_eq!(
        p1.push_cursor,
        log_a
            .ops_of_device(&c.a_id, 0, 10)
            .unwrap()
            .last()
            .expect("本机自产 op 在册")
            .ts,
        "出站游标推进到已出账的那一条"
    );
    assert!(p1.last_sync_ms > 0, "有流水行 ⇒ 时刻非零");
    assert!(p1.last_error.is_none());

    // 对端产出一条 → 本机拉走：入站游标动起来，出站游标**不该**跟着动（两维分离）
    c.b_store.put("t1.md", "from B");
    c.b.record_change(ENTITY_NOTE, "t1.md", "create").unwrap();
    let s2 =
        c.a.sync_with(&c.b_id, &addr(STATUS_PEER_PORT))
            .await
            .unwrap();
    assert_eq!(s2.pulled_applied, 1);
    let p2 = peer_of(&c.a.status().unwrap());
    assert_eq!(p2.inbound_cursor, log_a.cursor(&c.b_id));
    assert_eq!(
        p2.push_cursor, p1.push_cursor,
        "拉对端的东西不改本机出账进度"
    );
    assert_eq!(p2.pending_ops, 0);
    assert!(
        p2.inbound_cursor > p2.push_cursor,
        "两维游标各读各的表：混用即谎报（入站 {} vs 出站 {}）",
        p2.inbound_cursor,
        p2.push_cursor
    );
}

/// 任务书字面测试名：`to_value` 后键集合恰等于结构体字段集——偷偷加键（或退回裸 json）都判红
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn status_shapeIsTyped_notRawJson() {
    let peer = PairedPeer {
        device_id: "dev-shape".into(),
        device_name: "形状机".into(),
        fingerprint: "ab:cd".into(),
        pubkey_b64: "AAAA".into(),
        paired_at: 1,
    };
    let (module, _store, _id) = setup("status_shape", Some(peer), STATUS_SHAPE_PORT);
    let st = module.status().unwrap();

    let value = serde_json::to_value(&st).unwrap();
    let mut keys: Vec<String> = value
        .as_object()
        .expect("状态序列化为对象")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    let expected = [
        "auto_sync",
        "last_bind_error",
        "listening",
        "op_count",
        "paused",
        "peers",
        "port",
        "self_device_id",
        "self_name",
    ];
    let mut expected: Vec<String> = expected.iter().map(|k| (*k).to_string()).collect();
    expected.sort();
    assert_eq!(keys, expected, "状态键集合恰等于 SyncStatus 字段集");

    let mut peer_keys: Vec<String> = value["peers"][0]
        .as_object()
        .expect("peer 序列化为对象")
        .keys()
        .cloned()
        .collect();
    peer_keys.sort();
    let mut expected_peer: Vec<String> = [
        "device_id",
        "device_name",
        "fingerprint",
        "inbound_cursor",
        "push_cursor",
        "pending_ops",
        "last_sync_ms",
        "last_error",
        "sync_addr",
        "online",
    ]
    .iter()
    .map(|k| (*k).to_string())
    .collect();
    expected_peer.sort();
    assert_eq!(
        peer_keys, expected_peer,
        "peer 键集合恰等于 PeerStatus 字段集"
    );
    assert_eq!(value["peers"][0]["sync_addr"], serde_json::Value::Null);
    assert_eq!(value["peers"][0]["online"], json!(false));
    assert_eq!(
        value["auto_sync"],
        json!(false),
        "新装实例的自动同步位在状态读面上也必须是关（默认关是本行的红线）"
    );
}

/// 任务书（09 §10.2 T-B5-5）字面测试名 · 红线："密码库永不自动同步"三层皆可否证
///
/// ① 编译期常量集不含 vault；② 运行期装配口拒收；③ 线格式与存储层不设防
/// （`op_log.entity` 只是文本列），因此手工伪造一条 vault 行进对端变更流，
/// 收侧仍须整会话拒收、数据集零落地、游标零前进。
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn syncEntity_vaultNeverAdmitted() {
    // ① 编译期：白名单里不存在 vault（"没装"是运行期偶然，"名单上没有"才是结构事实）
    assert!(!SYNC_ENTITIES.iter().any(|s| s.id == "vault"));
    assert!(!is_sync_entity("vault"));
    assert_eq!(
        SYNC_ENTITIES.len(),
        1,
        "v1 白名单恰一员 note；新增数据集须同时带来测试与文档（§10.2 判据）"
    );
    assert_eq!(SYNC_ENTITIES[0].id, ENTITY_NOTE);
    assert_eq!(SYNC_ENTITIES[0].label, "笔记库");

    let c = paired("vault_never", VAULT_NEVER_PORT);
    wait_port(VAULT_NEVER_PORT).await;

    // ② 装配口：想接 vault，一行代码写不进去（FakeStore 本身对 entity 毫无防备，
    //    所以这道拒必须来自注册表而不是"数据集自己不肯"——下面第③层同理）
    let err =
        c.b.attach_applier("vault", Arc::new(FakeStore::default()))
            .unwrap_err();
    assert!(matches!(err, SyncError::Entity(_)), "实际 {err:?}");
    assert!(err.to_string().contains("vault"), "错误要点名被拒的数据集");
    assert!(
        c.b.record_change("vault", "任何路径", "write").is_err(),
        "拒收后注册表里确实没有 vault：本地记录口同样进不去"
    );

    // ③ 伪造：vault 行直接写进 A 的 op_log（绕过全部代码路径），A → B 同步
    let log_a = OpLog::open(c.a.db_path()).unwrap();
    log_a
        .append(&OpEntry {
            op_id: "vault-forged-1".into(),
            entity: "vault".into(),
            entity_id: "secret-entry".into(),
            ts: 100,
            device: c.a_id.clone(),
            value: json!({ "secret": "绝不该离开本机的东西" }),
        })
        .unwrap();

    let before_b = c.b_store.len();
    let r = c.a.sync_with(&c.b_id, &addr(VAULT_NEVER_PORT)).await;
    assert!(
        r.is_err(),
        "对端没装 vault 应用器 ⇒ 会话必须失败，不能\"推成功\"了事：{r:?}"
    );
    assert_eq!(
        c.b_store.len(),
        before_b,
        "B 侧数据集零动：零个应用器被问过"
    );
    assert!(
        c.b_store.get("secret-entry").is_none(),
        "密码库条目内容不得出现在对端任何数据集里"
    );

    // 游标不前进：静默"跳过"会让 B 下轮把这批当成已经收过
    let log_b = OpLog::open(c.b.db_path()).unwrap();
    assert_eq!(
        log_b.cursor(&c.a_id),
        0,
        "拒收不许推进入站游标（否则谎报已同步）"
    );
    let runs = c.b.runs(5).unwrap();
    let errs: Vec<String> = runs.iter().filter_map(|r| r.error.clone()).collect();
    assert!(
        errs.iter().any(|e| e.contains("vault")),
        "失败要在流水里点名是哪个数据集，实际：{errs:?}"
    );
}

/// 任务书（09 §10.2 T-B5-6）字面测试名 · 与 T-B5-7 交界的诚实面：
/// 从没成功同步过的设备**没有地址事实源**，到期一轮必须记失败行而不是记成功
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn syncAuto_noAddressPeer_recordsErrorNotSuccess() {
    let c = paired("auto_noaddr", NOADDR_PORT);
    c.a_store.put("a1.md", "from A");
    c.a.record_change(ENTITY_NOTE, "a1.md", "write").unwrap();

    // 到期一轮（静默窗跑的就是这个入口，这里直接调它把 5 秒下限留给 T-B5-7 之后）
    c.a.sync_due_peers().await.unwrap();

    let log_a = OpLog::open(c.a.db_path()).unwrap();
    let rows = log_a.runs(10).unwrap();
    assert_eq!(rows.len(), 1, "跳过也要留一行（承重③：失败不静默）");
    assert_eq!(rows[0].peer, c.b_id, "流水行要点名是哪台设备");
    assert_eq!(rows[0].pushed, 0, "没推就是没推");
    assert_eq!(rows[0].pulled_applied, 0);
    let err = rows[0]
        .error
        .as_deref()
        .expect("无地址的一轮必须记错误，绝不记成功");
    assert!(err.contains("无可用地址"), "要自证拒的是什么，实际：{err}");
    assert!(err.contains("手输"), "要指路下一步怎么办，实际：{err}");
    assert!(
        !err.contains("127.0.0.1"),
        "绝不拿回环地址凑数（发给邻居比失败更坏），实际：{err}"
    );
    assert_eq!(log_a.push_cursor(&c.b_id), 0, "跳过不许推进出站游标");
    assert_eq!(c.b_store.len(), 0, "B 侧什么都没收到：跳过不是暗推");
    assert!(c.a.peer_addr(&c.b_id).is_none(), "没成功过就没有地址事实源");

    // 面板同一读面：进度与失败原因一起看得见
    let st = c.a.status().unwrap();
    let p = st
        .peers
        .iter()
        .find(|x| x.device_id == c.b_id)
        .expect("配对设备在状态读面上");
    assert_eq!(p.pending_ops, 1, "欠着的那条不许被\"跳过\"抹掉");
    assert!(p.last_error.is_some(), "上次为什么没成得说得出");
    assert_eq!(p.last_sync_ms, rows[0].ts_ms, "时刻与错误读自同一行流水");
}

/// 任务书（09 §10.2 T-B5-6）字面测试名：暂停位是**唯一**那一位，
/// 暂停即收手、恢复不补跑、面板徽章与内核行为读同一内存
#[tokio::test(flavor = "multi_thread")]
#[allow(non_snake_case)]
async fn syncAuto_pausedSkipsAndReports() {
    let c = paired("auto_pause", PAUSE_PORT);
    wait_port(PAUSE_PORT).await;
    let log_a = OpLog::open(c.a.db_path()).unwrap();
    let peer_row = || {
        c.a.status()
            .unwrap()
            .peers
            .into_iter()
            .find(|x| x.device_id == c.b_id)
            .expect("配对设备在状态读面上")
    };

    // 先成功同步一次：这才留下"该往哪台发"的地址事实源
    c.a_store.put("p.md", "v1");
    c.a.record_change(ENTITY_NOTE, "p.md", "write").unwrap();
    c.a.sync_with(&c.b_id, &addr(PAUSE_PORT)).await.unwrap();
    assert_eq!(c.b_store.get("p.md").unwrap()["content"], "v1");
    assert!(
        c.a.peer_addr(&c.b_id).is_some(),
        "成功过 ⇒ 有地址（下一臂的\"跳过\"才是真跳过，而不是无从下手）"
    );
    let rows0 = log_a.runs(50).unwrap().len();
    assert!(rows0 >= 1);

    // 暂停臂：新的到期轮一个字节都不许出
    c.a.set_paused(true);
    assert!(
        c.a.status().unwrap().paused,
        "面板徽章读的就是内核那一位（reports 的半边）"
    );
    assert!(
        peer_row().last_error.is_none(),
        "暂停不是故障：状态读面不该凭空多出一条错误"
    );
    c.a_store.put("p.md", "v2");
    c.a.record_change(ENTITY_NOTE, "p.md", "write").unwrap();
    c.a.sync_due_peers().await.unwrap();
    assert_eq!(
        log_a.runs(50).unwrap().len(),
        rows0,
        "暂停态下到期也不出账，而且**不补记**一行（skips 的半边）"
    );
    assert_eq!(
        c.b_store.get("p.md").unwrap()["content"],
        "v1",
        "B 侧停在暂停前的那一份"
    );
    assert_eq!(
        peer_row().pending_ops,
        1,
        "暂停不抹掉还欠着的账（恢复后要说得清欠了什么）"
    );

    // 恢复：按"以后照常"的语义，不立刻补跑一轮
    c.a.set_paused(false);
    assert!(!c.a.status().unwrap().paused);
    assert_eq!(
        log_a.runs(50).unwrap().len(),
        rows0,
        "恢复本身不触发一轮（要立刻出账那里有「立即同步」）"
    );
    // 但下一次到期真跑得动（暂停位放行的是同一个入口）
    c.a.sync_due_peers().await.unwrap();
    assert_eq!(
        c.b_store.get("p.md").unwrap()["content"],
        "v2",
        "恢复后到期一轮真出账"
    );
    assert_eq!(peer_row().pending_ops, 0, "出完账面板的欠账数跟着归零");
    assert!(
        log_a.runs(50).unwrap().len() > rows0,
        "这一轮必须在流水里留痕（不是悄悄同步完了）"
    );
}
