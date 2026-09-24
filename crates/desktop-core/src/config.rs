//! desktop 段配置（docs/impl/09 §7.2 T-B7-16；B6 T-B6-6 merged 三件套形制：
//! Config 结构字段 ≡ config_schema 键 ≡ 真实读者；缺键不动运行态、坏值整批弹回点名）。

use serde::{Deserialize, Serialize};

use crate::tidy::TidyMapping;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DesktopConfig {
    /// 自定义分类映射（null/缺省=内置六类，旧行为逐字不变）。
    /// None 序列化为显式 null（缺键序列化会让 schema default 同源断言落空）
    #[serde(default, serialize_with = "serialize_tidy_map")]
    pub tidy_map: Option<TidyMapping>,
}

fn serialize_tidy_map<S: serde::Serializer>(
    v: &Option<TidyMapping>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match v {
        Some(m) => m.serialize(s),
        None => s.serialize_none(),
    }
}

impl DesktopConfig {
    /// 增量合并：`tidy_map` 缺键 ⇒ 保留现运行态；显式 null ⇒ 回内置；
    /// 非法值（含 validate 红线）⇒ 整批 Err 点名，调用方不半途应用
    pub fn merged(&self, values: &serde_json::Value) -> std::result::Result<Self, String> {
        let mut next = self.clone();
        if values.is_null() {
            return Ok(next);
        }
        let obj = values
            .as_object()
            .ok_or_else(|| "desktop 配置必须是 JSON 对象".to_string())?;
        if let Some(v) = obj.get("tidy_map") {
            if v.is_null() {
                next.tidy_map = None;
            } else {
                let m: TidyMapping =
                    serde_json::from_value(v.clone()).map_err(|e| format!("tidy_map 非法: {e}"))?;
                m.validate().map_err(|e| format!("tidy_map 非法: {e}"))?;
                next.tidy_map = Some(m);
            }
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §7.2 T-B7-16）字面测试名优先于 rustc 命名惯例
    fn merged_absentKeyKeepsRuntime_explicitNullReverts() {
        let base = DesktopConfig {
            tidy_map: Some(TidyMapping {
                categories: vec![("设计稿".into(), "D:\\Design".into(), vec!["psd".into()])],
            }),
        };
        // 缺键不动（如仅改他段回流）
        let kept = base.merged(&serde_json::json!({})).unwrap();
        assert_eq!(kept, base);
        // 显式 null=回内置
        let cleared = base
            .merged(&serde_json::json!({ "tidy_map": null }))
            .unwrap();
        assert_eq!(cleared.tidy_map, None);
        // 坏值整批弹回点名
        let e = base
            .merged(
                &serde_json::json!({ "tidy_map": { "categories": [["文档", "Docs", ["pdf"]]] } }),
            )
            .unwrap_err();
        assert!(
            e.contains("tidy_map 非法") && e.contains("绝对路径"),
            "点名: {e}"
        );
        assert!(base
            .merged(&serde_json::json!({ "tidy_map": { "categories": [["x", "D:\\d", ["psd"]]] } }))
            .is_ok());
    }
}
