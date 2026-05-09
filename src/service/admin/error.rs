//! Admin API 错误类型定义

use std::fmt;

use axum::http::StatusCode;

use crate::domain::error::{ConfigError, RefreshError};
use crate::interface::http::admin::dto::AdminErrorResponse;
use crate::service::credential_pool::AdminPoolError;

/// Admin 服务错误类型
///
/// 每个变体对应明确的语义，`From<AdminPoolError>` 实现 1-1 结构化映射，
/// 对外 HTTP 响应通过 `error_type` 字段传递精确的错误类型。
#[derive(Debug)]
pub enum AdminServiceError {
    /// 凭据不存在
    NotFound { id: u64 },

    /// 上游服务调用失败（网络等字符串错误）
    UpstreamError(String),

    /// Token 刷新失败（保留结构化 RefreshError）
    RefreshError(RefreshError),

    /// 上游 HTTP 非 2xx 响应
    UpstreamHttp { status: u16, body: String },

    /// 内部状态错误（通用字符串错误）
    InternalError(String),

    /// 配置持久化失败（保留结构化 ConfigError）
    ConfigError(ConfigError),

    /// 凭据因配置无效被禁用
    DisabledByInvalidConfig(u64),

    /// 凭据已存在（refreshToken 重复）
    DuplicateRefreshToken,

    /// 凭据已存在（kiroApiKey 重复）
    DuplicateApiKey,

    /// refreshToken 已被截断，可能被 Kiro IDE 故意修改
    TruncatedRefreshToken(usize),

    /// refreshToken 为空
    EmptyRefreshToken,

    /// 缺少 refreshToken
    MissingRefreshToken,

    /// kiroApiKey 为空
    EmptyApiKey,

    /// 缺少 kiroApiKey
    MissingApiKey,

    /// 只能删除已禁用的凭据
    NotDisabled(u64),

    /// API Key 凭据不支持刷新 Token
    ApiKeyNotRefreshable,

    /// 通用请求参数校验失败（非凭据本身的问题）
    InvalidRequest(String),
}

impl fmt::Display for AdminServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdminServiceError::NotFound { id } => write!(f, "凭据不存在: {id}"),
            AdminServiceError::UpstreamError(msg) => write!(f, "上游服务错误: {msg}"),
            AdminServiceError::RefreshError(e) => write!(f, "Token 刷新失败: {e}"),
            AdminServiceError::UpstreamHttp { status, body } => {
                write!(f, "上游 HTTP {status}: {body}")
            }
            AdminServiceError::InternalError(msg) => write!(f, "内部错误: {msg}"),
            AdminServiceError::ConfigError(e) => write!(f, "配置持久化失败: {e}"),
            AdminServiceError::DisabledByInvalidConfig(id) => {
                write!(
                    f,
                    "凭据 #{id} 因配置无效被禁用，请修正配置后重启服务"
                )
            }
            AdminServiceError::DuplicateRefreshToken => {
                write!(f, "凭据已存在（refreshToken 重复）")
            }
            AdminServiceError::DuplicateApiKey => {
                write!(f, "凭据已存在（kiroApiKey 重复）")
            }
            AdminServiceError::TruncatedRefreshToken(len) => {
                write!(
                    f,
                    "refreshToken 已被截断（长度: {len} 字符）。\
                     这通常是 Kiro IDE 为了防止凭证被第三方工具使用而故意截断的。"
                )
            }
            AdminServiceError::EmptyRefreshToken => write!(f, "refreshToken 为空"),
            AdminServiceError::MissingRefreshToken => write!(f, "缺少 refreshToken"),
            AdminServiceError::EmptyApiKey => write!(f, "kiroApiKey 为空"),
            AdminServiceError::MissingApiKey => write!(f, "缺少 kiroApiKey"),
            AdminServiceError::NotDisabled(id) => {
                write!(f, "只能删除已禁用的凭据（请先禁用凭据 #{id}）")
            }
            AdminServiceError::ApiKeyNotRefreshable => {
                write!(f, "API Key 凭据不支持刷新 Token")
            }
            AdminServiceError::InvalidRequest(msg) => write!(f, "请求无效: {msg}"),
        }
    }
}

impl std::error::Error for AdminServiceError {}

impl AdminServiceError {
    /// 获取对应的 HTTP 状态码
    pub fn status_code(&self) -> StatusCode {
        match self {
            AdminServiceError::NotFound { .. } => StatusCode::NOT_FOUND,
            AdminServiceError::UpstreamError(_)
            | AdminServiceError::RefreshError(_)
            | AdminServiceError::UpstreamHttp { .. } => {
                StatusCode::BAD_GATEWAY
            }
            AdminServiceError::InternalError(_)
            | AdminServiceError::ConfigError(_)
            | AdminServiceError::DisabledByInvalidConfig(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AdminServiceError::DuplicateRefreshToken
            | AdminServiceError::DuplicateApiKey
            | AdminServiceError::TruncatedRefreshToken(_)
            | AdminServiceError::EmptyRefreshToken
            | AdminServiceError::MissingRefreshToken
            | AdminServiceError::EmptyApiKey
            | AdminServiceError::MissingApiKey
            | AdminServiceError::NotDisabled(_)
            | AdminServiceError::ApiKeyNotRefreshable
            | AdminServiceError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
        }
    }

    /// 转换为 API 错误响应
    ///
    /// `error_type` 字段对外为稳定 API 契约，调用方可程序化区分错误类型。
    pub fn into_response(self) -> AdminErrorResponse {
        let msg = self.to_string();
        let error_type = match &self {
            AdminServiceError::NotFound { .. } => "not_found",
            AdminServiceError::UpstreamError(_) => "upstream_error",
            AdminServiceError::RefreshError(_) => "refresh_error",
            AdminServiceError::UpstreamHttp { .. } => "upstream_http_error",
            AdminServiceError::InternalError(_) => "internal_error",
            AdminServiceError::ConfigError(_) => "config_error",
            AdminServiceError::DisabledByInvalidConfig(_) => "disabled_by_invalid_config",
            AdminServiceError::DuplicateRefreshToken => "duplicate_refresh_token",
            AdminServiceError::DuplicateApiKey => "duplicate_api_key",
            AdminServiceError::TruncatedRefreshToken(_) => "truncated_refresh_token",
            AdminServiceError::EmptyRefreshToken => "empty_refresh_token",
            AdminServiceError::MissingRefreshToken => "missing_refresh_token",
            AdminServiceError::EmptyApiKey => "empty_api_key",
            AdminServiceError::MissingApiKey => "missing_api_key",
            AdminServiceError::NotDisabled(_) => "not_disabled",
            AdminServiceError::ApiKeyNotRefreshable => "api_key_not_refreshable",
            AdminServiceError::InvalidRequest(_) => "invalid_request",
        };
        AdminErrorResponse::new(error_type, msg)
    }
}

/// AdminPoolError → AdminServiceError 结构化 1-1 映射
impl From<AdminPoolError> for AdminServiceError {
    fn from(e: AdminPoolError) -> Self {
        match e {
            AdminPoolError::NotFound(id) => AdminServiceError::NotFound { id },
            AdminPoolError::DuplicateRefreshToken => AdminServiceError::DuplicateRefreshToken,
            AdminPoolError::DuplicateApiKey => AdminServiceError::DuplicateApiKey,
            AdminPoolError::TruncatedRefreshToken(len) => {
                AdminServiceError::TruncatedRefreshToken(len)
            }
            AdminPoolError::EmptyRefreshToken => AdminServiceError::EmptyRefreshToken,
            AdminPoolError::EmptyApiKey => AdminServiceError::EmptyApiKey,
            AdminPoolError::MissingRefreshToken => AdminServiceError::MissingRefreshToken,
            AdminPoolError::MissingApiKey => AdminServiceError::MissingApiKey,
            AdminPoolError::NotDisabled(id) => AdminServiceError::NotDisabled(id),
            AdminPoolError::ApiKeyNotRefreshable => AdminServiceError::ApiKeyNotRefreshable,
            AdminPoolError::Refresh(e) => AdminServiceError::RefreshError(e),
            AdminPoolError::UpstreamHttp { status, body } => {
                AdminServiceError::UpstreamHttp { status, body }
            }
            AdminPoolError::Network(e) => AdminServiceError::UpstreamError(e),
            AdminPoolError::Config(e) => AdminServiceError::ConfigError(e),
            AdminPoolError::DisabledByInvalidConfig(id) => {
                AdminServiceError::DisabledByInvalidConfig(id)
            }
        }
    }
}
