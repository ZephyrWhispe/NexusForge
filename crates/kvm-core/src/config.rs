//! kvm 段配置三件套（T-B7-8，B6 承重⑭ merged 形制）：
//! [`KvmConfig`] = config_schema 声明 ≡ 字段集 ≡ 真读者，逐键同源。
//! 读者：`port` → [`KvmModule::start`] 每次取 `discovery_port` 现值建发现服务
//! （[`KvmConfig::merged`] 校验后即刻 store，下次 start 生效）；
//! `edge_map` → [`edge::EdgeSwitch`] 内存态（apply_config 派发即生效，不重启）。
//! 旧盘无 `edge_map` 键 = serde default 零迁移；`enabled` 死键已除（复活不得
//! 就删除——全局 enabled_modules 才是真开关，见 09 §7.2 T-B7-8 收口登记）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::edge::Edge;

/// 持久化形制：`{left,right,up,down -> deviceId|""}`（任务书 09 §7.2 T-B7-8）。
/// `""` = 该缘未映射；四键恒全写（set 侧整表覆写，删设备即归位 `""`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EdgeMapConfig {
    pub left: String,
    pub right: String,
    pub up: String,
    pub down: String,
}

impl EdgeMapConfig {
    /// 边标识 → 槽位可变引用；非四缘键名弹回并点名
    fn slot_mut(&mut self, edge: &str) -> Result<&mut String, String> {
        match edge {
            "left" => Ok(&mut self.left),
            "right" => Ok(&mut self.right),
            "up" => Ok(&mut self.up),
            "down" => Ok(&mut self.down),
            other => Err(format!("非法边标识: {other}")),
        }
    }

    /// 运行态（设备→边）反演为持久形制（边→设备）。写侧 set_edge_map 已拒
    /// 同缘双设备，反演全函数；按设备 id 升序落槽保证逐位确定性。
    pub fn from_device_map(map: &HashMap<String, String>) -> Self {
        let mut out = Self::default();
        let mut devs: Vec<(&String, &String)> = map.iter().collect();
        devs.sort();
        for (device, edge) in devs {
            if let Ok(slot) = out.slot_mut(edge) {
                *slot = device.clone();
            }
        }
        out
    }

    /// 持久形制 → EdgeSwitch 内存态（`""` 槽位跳过；四槽恒合法边，无弹回路径）
    pub fn to_edges(&self) -> HashMap<String, Edge> {
        [
            (self.left.as_str(), Edge::Left),
            (self.right.as_str(), Edge::Right),
            (self.up.as_str(), Edge::Up),
            (self.down.as_str(), Edge::Down),
        ]
        .into_iter()
        .filter(|(dev, _)| !dev.is_empty())
        .map(|(dev, e)| (dev.to_string(), e))
        .collect()
    }

    /// 持久形制 → 运行态（设备→边字符串表，IPC 展示形制）
    pub fn to_device_map(&self) -> HashMap<String, String> {
        self.to_edges()
            .into_iter()
            .map(|(dev, e)| (dev, e.as_str().to_string()))
            .collect()
    }

    /// 从 JSON 值解析（apply_config 派发口）：未知键名整批弹回并点名（全有或
    /// 全无，B6 形制——半套边缘映射在跑比不生效更难查）
    pub fn from_value(values: &serde_json::Value) -> Result<Self, String> {
        let obj = values
            .as_object()
            .ok_or_else(|| format!("edge_map 须为 JSON 对象，收到 {values}"))?;
        let mut out = Self::default();
        for (k, v) in obj {
            let dev = v
                .as_str()
                .ok_or_else(|| format!("边 {k} 的设备值须为字符串: {v}"))?;
            let slot = out.slot_mut(k)?;
            *slot = dev.to_string();
        }
        Ok(out)
    }
}

/// kvm 段配置（schema 键集 ≡ 字段集，死键守卫测钉死）
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KvmConfig {
    /// UDP 组播发现端口（1024..=65535；变更下次 start 生效——发现服务在
    /// init/start 取一次快照，运行期改口不断存量会话）
    pub port: u16,
    /// 边缘映射持久值（运行态真源仍在 EdgeSwitch，本字段是盘上镜像）
    pub edge_map: EdgeMapConfig,
}

impl Default for KvmConfig {
    fn default() -> Self {
        Self {
            port: crate::discovery::DEFAULT_PORT,
            edge_map: EdgeMapConfig::default(),
        }
    }
}

impl KvmConfig {
    /// 现运行态 ⊕ 补丁（缺键 = 不动运行态；一个坏值整批弹回并点名键）
    pub fn merged(&self, values: &serde_json::Value) -> Result<KvmConfig, String> {
        let mut next = self.clone();
        if let Some(v) = values.get("port") {
            let p = v
                .as_u64()
                .and_then(|p| u16::try_from(p).ok())
                .ok_or_else(|| format!("port 须为 1024–65535 整数，收到 {v}"))?;
            if !(1024..=65535).contains(&p) {
                return Err(format!("port 须为 1024–65535 整数，收到 {v}"));
            }
            next.port = p;
        }
        if let Some(v) = values.get("edge_map") {
            next.edge_map = EdgeMapConfig::from_value(v)?;
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_map_config_roundtrip_deterministic() {
        let mut dev = HashMap::new();
        dev.insert("dev-a".to_string(), "left".to_string());
        dev.insert("dev-b".to_string(), "up".to_string());
        let cfg = EdgeMapConfig::from_device_map(&dev);
        assert_eq!((cfg.left.as_str(), cfg.up.as_str()), ("dev-a", "dev-b"));
        assert_eq!((cfg.right.as_str(), cfg.down.as_str()), ("", ""));
        assert_eq!(cfg.to_device_map(), dev);
    }

    #[test]
    fn kvm_config_merged_absent_key_keeps() {
        let base = KvmConfig {
            port: 50000,
            edge_map: EdgeMapConfig {
                left: "dev-a".into(),
                ..Default::default()
            },
        };
        assert_eq!(base.merged(&serde_json::json!({})).unwrap(), base);
        let patched = base
            .merged(&serde_json::json!({"edge_map": {"right": "dev-b"}}))
            .unwrap();
        assert_eq!(patched.port, 50000);
        assert_eq!(
            (
                patched.edge_map.left.as_str(),
                patched.edge_map.right.as_str()
            ),
            ("", "dev-b")
        );
        // 坏值整批点名
        assert!(base
            .merged(&serde_json::json!({"port": 80}))
            .unwrap_err()
            .contains("port"));
        assert!(base
            .merged(&serde_json::json!({"edge_map": {"diagonal": "x"}}))
            .unwrap_err()
            .contains("diagonal"));
    }
}
