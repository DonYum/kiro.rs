//! Token usage metadata events.

use serde::Deserialize;

use crate::kiro::parser::error::ParseResult;
use crate::kiro::parser::frame::Frame;

use super::base::EventPayload;

/// Kiro token usage payload carried by metadata-like events.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// Input tokens not served from prompt cache.
    #[serde(default)]
    pub uncached_input_tokens: i32,
    /// Output tokens reported by Kiro.
    #[serde(default)]
    #[allow(dead_code)]
    pub output_tokens: i32,
    /// Total tokens reported by Kiro.
    #[serde(default)]
    #[allow(dead_code)]
    pub total_tokens: i32,
    /// Input tokens served from prompt cache.
    #[serde(default)]
    pub cache_read_input_tokens: i32,
    /// Input tokens written into prompt cache.
    #[serde(default)]
    pub cache_write_input_tokens: i32,
}

/// Metadata event containing token usage.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataEvent {
    /// Optional token usage block.
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
}

impl EventPayload for MetadataEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

/// Flat metering event containing Kiro credit usage.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeteringEvent {
    /// Credits consumed by this request. Kiro uses this as the practical
    /// cache-hit signal on backends that do not emit metadataEvent tokenUsage.
    #[serde(default)]
    pub usage: Option<f64>,
    /// Input tokens reported by the metering event, when present.
    #[serde(default)]
    pub input_tokens: Option<i32>,
    /// Output tokens reported by the metering event, when present.
    #[serde(default)]
    pub output_tokens: Option<i32>,
}

impl EventPayload for MeteringEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

impl MetadataEvent {
    /// Anthropic's input token number excludes cache writes but includes cache reads.
    #[allow(dead_code)]
    pub fn anthropic_input_tokens(&self) -> Option<i32> {
        self.token_usage
            .as_ref()
            .map(|usage| usage.uncached_input_tokens + usage.cache_read_input_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_event_token_usage_deserialize() {
        let event: MetadataEvent = serde_json::from_str(
            r#"{
                "tokenUsage": {
                    "uncachedInputTokens": 50,
                    "outputTokens": 20,
                    "totalTokens": 120,
                    "cacheReadInputTokens": 40,
                    "cacheWriteInputTokens": 10
                }
            }"#,
        )
        .unwrap();

        let token_usage = event.token_usage.unwrap();
        assert_eq!(token_usage.uncached_input_tokens, 50);
        assert_eq!(token_usage.cache_read_input_tokens, 40);
        assert_eq!(token_usage.cache_write_input_tokens, 10);
        assert_eq!(token_usage.output_tokens, 20);
        assert_eq!(token_usage.total_tokens, 120);
    }

    #[test]
    fn test_metadata_event_without_token_usage_deserialize() {
        let event: MetadataEvent = serde_json::from_str(r#"{"usage": 1.5}"#).unwrap();
        assert!(event.token_usage.is_none());
    }

    #[test]
    fn test_metering_event_deserialize() {
        let event: MeteringEvent = serde_json::from_str(
            r#"{
                "usage": 1.5,
                "inputTokens": 100,
                "outputTokens": 20
            }"#,
        )
        .unwrap();

        assert_eq!(event.usage, Some(1.5));
        assert_eq!(event.input_tokens, Some(100));
        assert_eq!(event.output_tokens, Some(20));
    }
}
