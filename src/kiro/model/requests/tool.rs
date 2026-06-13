//! 工具类型定义
//!
//! 定义 Kiro API 中工具相关的类型

use serde::{Deserialize, Serialize};

/// 工具数组条目
///
/// Kiro tools 数组可以同时包含工具定义和缓存断点。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Tool {
    /// 工具规范条目
    Specification(ToolDefinition),
    /// 缓存断点条目
    CachePoint(CachePointTool),
}

impl Tool {
    /// 创建工具定义条目
    pub fn specification(tool_specification: ToolSpecification) -> Self {
        Self::Specification(ToolDefinition { tool_specification })
    }

    /// 创建默认缓存断点条目
    pub fn cache_point_default() -> Self {
        Self::CachePoint(CachePointTool {
            cache_point: CachePoint::default_marker(),
        })
    }

    /// 返回工具名称；缓存断点没有名称
    pub fn tool_name(&self) -> Option<&str> {
        match self {
            Self::Specification(tool) => Some(tool.tool_specification.name.as_str()),
            Self::CachePoint(_) => None,
        }
    }

    /// 是否为缓存断点
    pub fn is_cache_point(&self) -> bool {
        matches!(self, Self::CachePoint(_))
    }
}

/// 工具定义
///
/// 用于在请求中定义可用的工具
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    /// 工具规范
    pub tool_specification: ToolSpecification,
}

/// Kiro prompt cache 断点
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachePointTool {
    /// 缓存断点定义
    pub cache_point: CachePoint,
}

/// Kiro prompt cache 断点配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachePoint {
    /// 断点类型
    pub r#type: String,
}

impl CachePoint {
    /// KiroProxy 参考实现使用 default 类型
    pub fn default_marker() -> Self {
        Self {
            r#type: "default".to_string(),
        }
    }
}

/// 工具规范
///
/// 定义工具的名称、描述和输入模式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpecification {
    /// 工具名称
    pub name: String,
    /// 工具描述
    pub description: String,
    /// 输入模式（JSON Schema）
    pub input_schema: InputSchema,
}

/// 输入模式
///
/// 包装 JSON Schema 定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSchema {
    /// JSON Schema 定义
    pub json: serde_json::Value,
}

impl Default for InputSchema {
    fn default() -> Self {
        Self {
            json: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        }
    }
}

impl InputSchema {
    /// 从 JSON 值创建
    pub fn from_json(json: serde_json::Value) -> Self {
        Self { json }
    }
}

/// 工具执行结果
///
/// 用于返回工具执行的结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    /// 工具使用 ID（与请求中的 tool_use_id 对应）
    pub tool_use_id: String,
    /// 结果内容（数组格式）
    pub content: Vec<serde_json::Map<String, serde_json::Value>>,
    /// 执行状态（"success" 或 "error"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// 是否为错误
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl ToolResult {
    /// 创建成功的工具结果
    pub fn success(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        let mut map = serde_json::Map::new();
        map.insert(
            "text".to_string(),
            serde_json::Value::String(content.into()),
        );

        Self {
            tool_use_id: tool_use_id.into(),
            content: vec![map],
            status: Some("success".to_string()),
            is_error: false,
        }
    }

    /// 创建错误的工具结果
    pub fn error(tool_use_id: impl Into<String>, error_message: impl Into<String>) -> Self {
        let mut map = serde_json::Map::new();
        map.insert(
            "text".to_string(),
            serde_json::Value::String(error_message.into()),
        );

        Self {
            tool_use_id: tool_use_id.into(),
            content: vec![map],
            status: Some("error".to_string()),
            is_error: true,
        }
    }
}

/// 工具使用条目
///
/// 用于历史消息中记录工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUseEntry {
    /// 工具使用 ID
    pub tool_use_id: String,
    /// 工具名称
    pub name: String,
    /// 工具输入参数
    pub input: serde_json::Value,
}

impl ToolUseEntry {
    /// 创建新的工具使用条目
    pub fn new(tool_use_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            name: name.into(),
            input: serde_json::json!({}),
        }
    }

    /// 设置输入参数
    pub fn with_input(mut self, input: serde_json::Value) -> Self {
        self.input = input;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult::success("tool-123", "Operation completed");

        assert!(!result.is_error);
        assert_eq!(result.status, Some("success".to_string()));
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error("tool-456", "File not found");

        assert!(result.is_error);
        assert_eq!(result.status, Some("error".to_string()));
    }

    #[test]
    fn test_tool_result_serialize() {
        let result = ToolResult::success("tool-789", "Done");
        let json = serde_json::to_string(&result).unwrap();

        assert!(json.contains("\"toolUseId\":\"tool-789\""));
        assert!(json.contains("\"status\":\"success\""));
        // is_error = false 应该被跳过
        assert!(!json.contains("isError"));
    }

    #[test]
    fn test_tool_use_entry() {
        let entry = ToolUseEntry::new("use-123", "read_file")
            .with_input(serde_json::json!({"path": "/test.txt"}));

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"toolUseId\":\"use-123\""));
        assert!(json.contains("\"name\":\"read_file\""));
        assert!(json.contains("\"path\":\"/test.txt\""));
    }

    #[test]
    fn test_input_schema_default() {
        let schema = InputSchema::default();
        assert_eq!(schema.json["type"], "object");
    }

    #[test]
    fn test_tool_specification_serializes_as_kiro_tool_entry() {
        let tool = Tool::specification(ToolSpecification {
            name: "Read".to_string(),
            description: "Read files".to_string(),
            input_schema: InputSchema::default(),
        });

        let value = serde_json::to_value(&tool).unwrap();
        assert!(value.get("toolSpecification").is_some());
        assert_eq!(value["toolSpecification"]["name"], "Read");
    }

    #[test]
    fn test_cache_point_serializes_as_kiro_tool_entry() {
        let tool = Tool::cache_point_default();

        let value = serde_json::to_value(&tool).unwrap();
        assert_eq!(value["cachePoint"]["type"], "default");
        assert!(tool.is_cache_point());
        assert_eq!(tool.tool_name(), None);
    }
}
