//! 上游元数据事件
//!
//! Kiro 在内容过滤拦截时仍返回 HTTP 200，并且常常只有 `metadataEvent`：
//! `stopReason = CONTENT_FILTERED`，说明在 `stopDetails.refusal` 里。
//! 不解析这一帧会把拒答丢掉，下游只能看到成功的空 `end_turn`。

use serde::Deserialize;

use crate::kiro::parser::error::ParseResult;
use crate::kiro::parser::frame::Frame;

use super::base::EventPayload;

/// 上游内容过滤拦截时 `metadataEvent.stopReason` 的取值。
pub const CONTENT_FILTERED_STOP_REASON: &str = "CONTENT_FILTERED";

/// `metadataEvent.stopDetails.refusal`：上游拒答的分类与说明。
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub explanation: Option<String>,
}

/// `metadataEvent.stopDetails`。
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StopDetails {
    #[serde(default)]
    pub refusal: Option<Refusal>,
}

/// 已确认的 `CONTENT_FILTERED` 拒答。
///
/// `category` / `explanation` 都可能缺省（官方拦截有时只给 explanation）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFilterRefusal {
    pub category: Option<String>,
    pub explanation: Option<String>,
}

impl ContentFilterRefusal {
    /// 日志用的单行说明：`category: explanation`，缺哪边就只留另一边。
    pub fn summary(&self) -> String {
        match (self.category.as_deref(), self.explanation.as_deref()) {
            (Some(category), Some(explanation)) => format!("{category}: {explanation}"),
            (Some(category), None) => category.to_string(),
            (None, Some(explanation)) => explanation.to_string(),
            (None, None) => CONTENT_FILTERED_STOP_REASON.to_string(),
        }
    }

    /// Anthropic Messages `stop_details`（`stop_reason = refusal` 时附带）。
    pub fn to_anthropic_stop_details(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "refusal",
            "category": self.category,
            "explanation": self.explanation,
        })
    }
}

/// `metadataEvent` payload。
///
/// 只保留拒答所需字段。`tokenUsage` 等其它字段由 serde 忽略。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataEvent {
    /// 上游停止原因，如 `CONTENT_FILTERED`。
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_details: Option<StopDetails>,
}

impl MetadataEvent {
    /// 仅当 `stopReason == CONTENT_FILTERED` 时返回拒答；`end_turn` 等不是拒答。
    pub fn content_filter_refusal(&self) -> Option<ContentFilterRefusal> {
        if self.stop_reason.as_deref() != Some(CONTENT_FILTERED_STOP_REASON) {
            return None;
        }
        let refusal = self.stop_details.as_ref().and_then(|d| d.refusal.as_ref());
        Some(ContentFilterRefusal {
            category: refusal.and_then(|r| nonempty(r.category.as_deref())),
            explanation: refusal.and_then(|r| nonempty(r.explanation.as_deref())),
        })
    }
}

fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

impl EventPayload for MetadataEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_filtered_stop_reason_is_a_refusal_with_upstream_explanation() {
        let event: MetadataEvent = serde_json::from_str(
            r#"{"stopReason":"CONTENT_FILTERED","stopDetails":{"refusal":{"category":"CYBER","explanation":"cannot continue"}}}"#,
        )
        .unwrap();
        let refusal = event.content_filter_refusal().unwrap();
        assert_eq!(refusal.summary(), "CYBER: cannot continue");
        assert_eq!(
            refusal.to_anthropic_stop_details(),
            serde_json::json!({
                "type": "refusal",
                "category": "CYBER",
                "explanation": "cannot continue"
            })
        );
    }

    #[test]
    fn official_restriction_explanation_without_category_is_still_a_refusal() {
        let event: MetadataEvent = serde_json::from_str(
            r#"{"stopDetails":{"refusal":{"explanation":"Your access to this model has been temporarily restricted due to excessive content refusals."}},"stopReason":"CONTENT_FILTERED"}"#,
        )
        .unwrap();
        let refusal = event.content_filter_refusal().unwrap();
        assert_eq!(refusal.category, None);
        assert_eq!(
            refusal.explanation.as_deref(),
            Some(
                "Your access to this model has been temporarily restricted due to excessive content refusals."
            )
        );
    }

    #[test]
    fn end_turn_stop_reason_is_not_a_refusal() {
        let event: MetadataEvent = serde_json::from_str(r#"{"stopReason":"end_turn"}"#).unwrap();
        assert_eq!(event.content_filter_refusal(), None);
    }

    #[test]
    fn content_filtered_without_details_is_still_a_refusal() {
        let event: MetadataEvent =
            serde_json::from_str(r#"{"stopReason":"CONTENT_FILTERED"}"#).unwrap();
        let refusal = event.content_filter_refusal().unwrap();
        assert_eq!(refusal.summary(), "CONTENT_FILTERED");
        assert!(refusal.to_anthropic_stop_details()["category"].is_null());
        assert!(refusal.to_anthropic_stop_details()["explanation"].is_null());
    }
}
