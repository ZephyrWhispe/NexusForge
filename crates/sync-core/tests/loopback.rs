//! SYNC 回环验收（docs/impl/07 SYNC 验收）：同机双实例，双向变更收敛。
//!
//! 两台 SyncModule（独立临时 appData + 互配信任根 + 假数据集 applier）：
//! A 记录变更 → sync_with(B) → B 数据集收敛 → B 记录变更 → A 反向拉取收敛。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use host_core::events::EventBus;
use host_core::module::{Module, ModuleContext};
use host_core::ports::Ports;
use kvm_core::pairing::{b64_encode, PairedPeer, PairStore};
use kvm_core::DeviceIdentity;
use sync_core::engine::ChangeApplier;
use sync_core::SyncModule;

/// 回环端口（与 KVM 49800/49801、K9 测试 49810–49812 错开；固定端口进程内单用）
const LOOPBACK_PORT: u16 = 49831;

/// 内存数据集投影
#[derive(Default)]
struct FakeStore {
    data: Mutex<std::collections::HashMap<String, serde_json::Value>>,
}
impl FakeStore {
    fn put(&self, id: &str, content: &str) {
        self.data.lock().unwrap().insert(id.to_string(), serde_json::json!({ "content": content }));
    }
    fn get(&self, id: &str) -> Option<serde_json::Value> {
        self.data.lock().unwrap().get(id).cloned()
    }
}
impl ChangeApplier for FakeStore {
    fn snapshot(&self, _entity: &str, id: &str) -> sync_core::Result<Option<serde_json::Value>> {
        Ok(self.get(id))
    }
    fn apply_upsert(&self, _entity: &str, id: &str, value: &serde_json::Value) -> sync_core::Result<()> {
        self.data.lock().unwrap().insert(id.to_string(), value.clone());
        Ok(())
    }
    fn apply_delete(&self, _entity: &str, id: &str) -> sync_core::Result<()> {
        self.data.lock().unwrap().remove(id);
        Ok(())
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nf_sync_loop_{tag}_{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 装配一台可同步的实例：预置身份 + 互配记录 → init + start；返回真实身份供配对
fn setup(tag: &str, peer_record: Option<PairedPeer>) -> (Arc<SyncModule>, Arc<FakeStore>, Arc<DeviceIdentity>) {
    let dir = temp_dir(tag);
    let kvm_dir = dir.join("kvm");
    let identity = Arc::new(DeviceIdentity::load_or_create(&kvm_dir, None).unwrap());
    if let Some(p) = peer_record {
        PairStore::load_or_default(&kvm_dir).unwrap().upsert(p).unwrap();
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
    module.set_port(LOOPBACK_PORT);
    module.start().unwrap();
    (module, store, identity)
}

/// 等待 B 的监听就绪（accept_loop 异步 bind；探测连接会被握手超时丢弃，无副作用）
async fn wait_port(port: u16) {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("SYNC 监听端口 {port} 未就绪");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_instances_converge_bidirectionally() {
    // B 先起（服务端监听），A 侧配对记录指向 B 的真实身份
    let (module_b, store_b, id_b) = setup("b", None);
    let record_b = PairedPeer {
        device_id: id_b.device_id.clone(),
        device_name: id_b.device_name.clone(),
        fingerprint: id_b.pubkey_fingerprint.clone(),
        pubkey_b64: b64_encode(&id_b.public_key()),
        paired_at: 0,
    };
    let (module_a, store_a, id_a) = setup("a", Some(record_b));
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
    let s1 = match module_a.sync_with(&b_id, "127.0.0.1:49831").await {
        Ok(s) => s,
        Err(e) => panic!("首次同步失败: {e:?}"),
    };
    assert_eq!(s1.pushed, 1, "A 应推送 1 条自产变更");
    assert_eq!(store_b.get("n.md").unwrap()["content"], "from A");

    // 3. 重复同步幂等：Push 恒为全量自产（对端 LWW/Noop 吸收重复）；Pull 靠游标零拉取
    let s2 = module_a.sync_with(&b_id, "127.0.0.1:49831").await.unwrap();
    assert_eq!(s2.pushed, 1, "Push 恒全量自产（v1 语义）");
    assert_eq!(s2.pulled_applied, 0, "无新变更时拉取为空");

    // 4. B 本地修改 → A 反向拉取收敛
    store_b.put("n.md", "from B");
    module_b.record_change("n.md", "write").unwrap();
    let s3 = module_a.sync_with(&b_id, "127.0.0.1:49831").await.unwrap();
    assert_eq!(s3.pulled_applied, 1, "A 应拉取 B 的 1 条新变更");
    assert_eq!(store_a.get("n.md").unwrap()["content"], "from B");

    // 5. 删除传播：A 删除 → B 收敛删除
    module_a.record_change("n.md", "delete").unwrap();
    module_a.sync_with(&b_id, "127.0.0.1:49831").await.unwrap();
    assert!(store_b.get("n.md").is_none(), "B 侧应被删除");
}
