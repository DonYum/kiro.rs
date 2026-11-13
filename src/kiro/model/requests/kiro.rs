//! Kiro 请求类型定义
//!
//! 定义 Kiro API 的主请求结构

use serde::{Deserialize, Serialize};

use super::conversation::ConversationState;

/// Kiro API 请求
///
/// 用于构建发送给 Kiro API 的请求
///
/// # 示例
///
/// ```rust
/// use kiro_rs::kiro::model::requests::{
///     KiroRequest, ConversationState, CurrentMessage, UserInputMessage, Tool
/// };
///
/// // 创建简单请求
/// let state = ConversationState::new("conv-123")
///     .with_agent_task_type("vibe")
///     .with_current_message(CurrentMessage::new(
///         UserInputMessage::new("Hello", "claude-3-5-sonnet")
///     ));
///
/// let request = KiroRequest::new(state);
/// let json = request.to_json().unwrap();
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroRequest {
    /// 对话状态
    pub conversation_state: ConversationState,
    /// Profile ARN（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_arn: Option<String>,
    /// Additional model request fields. Real Kiro CLI traffic uses this for effort.
    ///
    /// ```json
    /// "additionalModelRequestFields": { "output_config": { "effort": "high" } }
    /// ```
    ///
    /// The outer key is camelCase via this struct's `rename_all`. The inner
    /// `output_config` key stays snake_case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields: Option<AdditionalModelRequestFields>,
}

/// Top-level container for the AWS Q `additionalModelRequestFields` object.
///
/// The inner `output_config` field is snake_case on the wire, unlike the outer
/// camelCase key, so this struct must not use `rename_all = "camelCase"`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AdditionalModelRequestFields {
    /// Claude reasoning effort (`output_config.effort`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<KiroOutputConfig>,
    /// Reasoning-wire effort (`reasoning.effort`). This fork does not emit it:
    /// the GPT-5.6 and Grok models that use it are not mapped here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<KiroReasoningConfig>,
}

/// Claude effort control accepted by the AWS Q backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KiroOutputConfig {
    pub effort: String,
}

/// Effort control used by models that reject `output_config` and expect
/// `reasoning.effort`. Kept so the serde shape matches official v0.9.9.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KiroReasoningConfig {
    pub effort: String,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_kiro_request_deserialize() {
        let json = r#"{
            "conversationState": {
                "conversationId": "conv-456",
                "currentMessage": {
                    "userInputMessage": {
                        "content": "Test message",
                        "modelId": "claude-3-5-sonnet",
                        "userInputMessageContext": {}
                    }
                }
            }
        }"#;

        let request: KiroRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.conversation_state.conversation_id, "conv-456");
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .content,
            "Test message"
        );
        assert!(request.additional_model_request_fields.is_none());
    }

    #[test]
    fn test_additional_model_request_fields_wire_format() {
        let request = KiroRequest {
            conversation_state: ConversationState::new("conv-456"),
            profile_arn: None,
            additional_model_request_fields: Some(AdditionalModelRequestFields {
                output_config: Some(KiroOutputConfig {
                    effort: "high".to_string(),
                }),
                reasoning: None,
            }),
        };
        let v = serde_json::to_value(&request).unwrap();
        assert_eq!(
            v["additionalModelRequestFields"]["output_config"]["effort"],
            "high"
        );
        assert!(v["additionalModelRequestFields"].get("reasoning").is_none());
        assert!(
            v["additionalModelRequestFields"]
                .get("outputConfig")
                .is_none(),
            "inner key must stay snake_case output_config, got {v}"
        );
        assert!(v.get("additional_model_request_fields").is_none());
    }
}
