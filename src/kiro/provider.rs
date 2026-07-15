//! Kiro API Provider
//!
//! 核心组件，负责与 Kiro API 通信
//! 支持流式和非流式请求
//! 支持多凭据故障转移和重试
//! 支持按凭据级 endpoint 切换不同 Kiro API 端点

use reqwest::Client;
use reqwest::header::HeaderMap;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::endpoint::{KiroEndpoint, RequestContext};
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::token_manager::{AllCredentialsCoolingDownError, MultiTokenManager};
use crate::model::config::TlsBackend;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// 每个凭据的最大重试次数
const MAX_RETRIES_PER_CREDENTIAL: usize = 3;

/// 常规重试预算上限；可用凭据更多时仍保证至少完整遍历一轮。
const MAX_TOTAL_RETRIES: usize = 64;

const DEFAULT_RATE_LIMIT_COOLDOWN_SECS: u64 = 60;
const MAX_RATE_LIMIT_COOLDOWN_SECS: u64 = 300;

/// API 调用结果：响应 + 实际使用的凭据 ID
///
/// 凭据 ID 用于缓存指纹追踪——模拟的 prompt cache 按凭据隔离，
/// 调用方需要知道这次请求最终落在哪个凭据上。
pub struct ApiCallResult {
    pub response: reqwest::Response,
    pub credential_id: u64,
}

/// Collects the final metering value and persists it once per upstream request.
#[derive(Clone)]
pub struct MeteringRecorder {
    token_manager: Arc<MultiTokenManager>,
    credential_id: u64,
    latest_credits: Arc<Mutex<Option<f64>>>,
    committed: Arc<AtomicBool>,
}

impl MeteringRecorder {
    pub fn observe(&self, credits: Option<f64>) {
        let Some(credits) = credits else { return };
        if credits.is_finite() && credits >= 0.0 {
            *self.latest_credits.lock() = Some(credits);
        }
    }

    pub fn commit(&self) {
        if self.committed.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            if let Some(credits) = *self.latest_credits.lock() {
                self.token_manager.report_metering_usage(self.credential_id, credits);
            }
        }
    }
}

/// Kiro API Provider
///
/// 核心组件，负责与 Kiro API 通信
/// 支持多凭据故障转移和重试机制
/// 按凭据 `endpoint` 字段选择 [`KiroEndpoint`] 实现
pub struct KiroProvider {
    token_manager: Arc<MultiTokenManager>,
    /// 全局代理配置（用于凭据无自定义代理时的回退）
    global_proxy: Option<ProxyConfig>,
    /// Client 缓存：key = effective proxy config, value = reqwest::Client
    /// 不同代理配置的凭据使用不同的 Client，共享相同代理的凭据复用 Client
    client_cache: Mutex<HashMap<Option<ProxyConfig>, Client>>,
    /// TLS 后端配置
    tls_backend: TlsBackend,
    /// 端点实现注册表（key: endpoint 名称）
    endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
    /// 默认端点名称（凭据未指定 endpoint 时使用）
    default_endpoint: String,
}

impl KiroProvider {
    pub fn metering_recorder(&self, credential_id: u64) -> MeteringRecorder {
        MeteringRecorder {
            token_manager: self.token_manager.clone(),
            credential_id,
            latest_credits: Arc::new(Mutex::new(None)),
            committed: Arc::new(AtomicBool::new(false)),
        }
    }
    /// 创建带代理配置和端点注册表的 KiroProvider 实例
    ///
    /// # Arguments
    /// * `token_manager` - 多凭据 Token 管理器
    /// * `proxy` - 全局代理配置
    /// * `endpoints` - 端点名 → 实现的注册表（至少包含 `default_endpoint` 对应条目）
    /// * `default_endpoint` - 凭据未显式指定 endpoint 时使用的名称
    pub fn with_proxy(
        token_manager: Arc<MultiTokenManager>,
        proxy: Option<ProxyConfig>,
        endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
        default_endpoint: String,
    ) -> Self {
        assert!(
            endpoints.contains_key(&default_endpoint),
            "默认端点 {} 未在 endpoints 注册表中",
            default_endpoint
        );
        let tls_backend = token_manager.config().tls_backend;
        // 预热：构建全局代理对应的 Client
        let initial_client = build_client(proxy.as_ref(), 720, tls_backend)
            .expect("创建 HTTP 客户端失败");
        let mut cache = HashMap::new();
        cache.insert(proxy.clone(), initial_client);

        Self {
            token_manager,
            global_proxy: proxy,
            client_cache: Mutex::new(cache),
            tls_backend,
            endpoints,
            default_endpoint,
        }
    }

    /// 根据凭据的代理配置获取（或创建并缓存）对应的 reqwest::Client
    fn client_for(&self, credentials: &KiroCredentials) -> anyhow::Result<Client> {
        let effective = credentials.effective_proxy(self.global_proxy.as_ref());
        let mut cache = self.client_cache.lock();
        if let Some(client) = cache.get(&effective) {
            return Ok(client.clone());
        }
        let client = build_client(effective.as_ref(), 720, self.tls_backend)?;
        cache.insert(effective, client.clone());
        Ok(client)
    }

    /// 根据凭据选择 endpoint 实现
    fn endpoint_for(
        &self,
        credentials: &KiroCredentials,
    ) -> anyhow::Result<Arc<dyn KiroEndpoint>> {
        let name = credentials
            .endpoint
            .as_deref()
            .unwrap_or(&self.default_endpoint);
        self.endpoints
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("未知端点: {}", name))
    }

    /// 发送非流式 API 请求
    ///
    /// 支持多凭据故障转移（见 [`Self::call_api_with_retry`]）
    /// 返回响应及实际使用的凭据 ID（用于按凭据维护缓存指纹表）
    pub async fn call_api(&self, request_body: &str) -> anyhow::Result<ApiCallResult> {
        self.call_api_with_retry(request_body, false).await
    }

    /// 发送流式 API 请求
    pub async fn call_api_stream(&self, request_body: &str) -> anyhow::Result<ApiCallResult> {
        self.call_api_with_retry(request_body, true).await
    }

    /// 发送 MCP API 请求（WebSearch 等工具调用）
    pub async fn call_mcp(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        self.call_mcp_with_retry(request_body).await
    }

    /// 内部方法：带重试逻辑的 MCP API 调用
    async fn call_mcp_with_retry(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        let total_credentials = self.token_manager.total_count();
        let max_retries = Self::compute_max_retries(
            total_credentials,
            self.token_manager.available_count_for_model(None),
        );
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();
        let mut failed_ids: Vec<u64> = Vec::new();

        for attempt in 0..max_retries {
            if self
                .token_manager
                .all_available_credentials_excluded(None, &failed_ids)
            {
                failed_ids.clear();
            }
            // MCP 调用（WebSearch 等工具）不涉及模型选择，无需按模型过滤凭据
            let ctx = match self
                .token_manager
                .acquire_context_for_session_excluding(None, None, &failed_ids)
                .await
            {
                Ok(c) => c,
                Err(e) if e.downcast_ref::<AllCredentialsCoolingDownError>().is_some() => {
                    return Err(e);
                }
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            let config = self.token_manager.config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(e);
                    // endpoint 解析失败：记为失败，换下一张凭据
                    self.token_manager.report_failure(ctx.id);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config,
            };

            let url = endpoint.mcp_url(&rctx);
            let body = endpoint.transform_mcp_body(request_body, &rctx);

            let base = self
                .client_for(&ctx.credentials)?
                .post(&url)
                .body(body)
                .header("content-type", "application/json")
                .header("Connection", "close");
            let request = endpoint.decorate_mcp(base, &rctx);

            let response = match request.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::warn!(
                        "MCP 请求发送失败（尝试 {}/{}）: {}",
                        attempt + 1,
                        max_retries,
                        e
                    );
                    last_error = Some(e.into());
                    if attempt + 1 < max_retries {
                        sleep(Self::retry_delay(attempt)).await;
                    }
                    continue;
                }
            };

            let status = response.status();
            let retry_after = Self::parse_retry_after(response.headers());

            // 成功响应
            if status.is_success() {
                self.token_manager.report_success(ctx.id);
                return Ok(response);
            }

            // 失败响应
            let body = response.text().await.unwrap_or_default();

            // 402 额度用尽
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if !has_available {
                    anyhow::bail!("MCP 请求失败（所有凭据已用尽）: {} {}", status, body);
                }
                last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
                continue;
            }

            // 400 Bad Request
            if status.as_u16() == 400 {
                anyhow::bail!("MCP 请求失败: {} {}", status, body);
            }

            // 401/403 凭据问题
            if matches!(status.as_u16(), 401 | 403) {
                // token 被上游失效：先尝试 force-refresh，每凭据仅一次机会
                if endpoint.is_bearer_token_invalid(&body) && !force_refreshed.contains(&ctx.id) {
                    force_refreshed.insert(ctx.id);
                    tracing::info!("凭据 #{} token 疑似被上游失效，尝试强制刷新", ctx.id);
                    if self.token_manager.force_refresh_token_for(ctx.id).await.is_ok() {
                        tracing::info!("凭据 #{} token 强制刷新成功，重试请求", ctx.id);
                        continue;
                    }
                    tracing::warn!("凭据 #{} token 强制刷新失败，计入失败", ctx.id);
                }

                let has_available = self.token_manager.report_failure(ctx.id);
                if !has_available {
                    anyhow::bail!("MCP 请求失败（所有凭据已用尽）: {} {}", status, body);
                }
                last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
                continue;
            }

            if status.as_u16() == 429 {
                if let Some(cooldown) = Self::classify_429_cooldown(&body, retry_after) {
                    let applied = self
                        .token_manager
                        .set_credential_rate_limit_cooldown(ctx.id, cooldown);
                    tracing::warn!(
                        credential_id = ctx.id,
                        cooldown_secs = applied.as_secs(),
                        "MCP 请求触发 429，已设置短冷却并切换凭据"
                    );
                } else {
                    tracing::warn!(
                        credential_id = ctx.id,
                        "MCP 请求触发容量不足类 429，不冷却凭据，仅切换"
                    );
                }
                if !failed_ids.contains(&ctx.id) {
                    failed_ids.push(ctx.id);
                }
                last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
                continue;
            }

            // 瞬态错误
            if status.as_u16() == 408 || status.is_server_error() {
                tracing::warn!(
                    "MCP 请求失败（上游瞬态错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx
            if status.is_client_error() {
                anyhow::bail!("MCP 请求失败: {} {}", status, body);
            }

            // 兜底
            last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!("MCP 请求失败：已达到最大重试次数（{}次）", max_retries)
        }))
    }

    /// 内部方法：带重试逻辑的 API 调用
    ///
    /// 重试策略：
    /// - 每个凭据最多重试 MAX_RETRIES_PER_CREDENTIAL 次
    /// - 常规预算为 `凭据数量 × 每凭据重试次数`
    /// - 至少完整遍历当前可用凭据一轮
    async fn call_api_with_retry(
        &self,
        request_body: &str,
        is_stream: bool,
    ) -> anyhow::Result<ApiCallResult> {
        let total_credentials = self.token_manager.total_count();
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();
        let mut failed_ids: Vec<u64> = Vec::new();
        let api_type = if is_stream { "流式" } else { "非流式" };

        // 尝试从请求体中提取模型信息
        let model = Self::extract_model_from_request(request_body);
        let session_id = Self::extract_session_id_from_request(request_body);
        let max_retries = Self::compute_max_retries(
            total_credentials,
            self.token_manager.available_count_for_model(model.as_deref()),
        );

        for attempt in 0..max_retries {
            if self
                .token_manager
                .all_available_credentials_excluded(model.as_deref(), &failed_ids)
            {
                failed_ids.clear();
            }
            // 获取调用上下文（绑定 index、credentials、token）
            let ctx = match self
                .token_manager
                .acquire_context_for_session_excluding(
                    model.as_deref(),
                    session_id.as_deref(),
                    &failed_ids,
                )
                .await
            {
                Ok(c) => c,
                Err(e) if e.downcast_ref::<AllCredentialsCoolingDownError>().is_some() => {
                    return Err(e);
                }
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            let config = self.token_manager.config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(e);
                    self.token_manager.report_failure(ctx.id);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config,
            };

            let url = endpoint.api_url(&rctx);
            let body = endpoint.transform_api_body(request_body, &rctx);

            let base = self
                .client_for(&ctx.credentials)?
                .post(&url)
                .body(body)
                .header("content-type", "application/json")
                .header("Connection", "close");
            let request = endpoint.decorate_api(base, &rctx);

            let response = match request.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::warn!(
                        "API 请求发送失败（尝试 {}/{}）: {}",
                        attempt + 1,
                        max_retries,
                        e
                    );
                    // 网络错误通常是上游/链路瞬态问题，不应导致"禁用凭据"或"切换凭据"
                    // （否则一段时间网络抖动会把所有凭据都误禁用，需要重启才能恢复）
                    last_error = Some(e.into());
                    if attempt + 1 < max_retries {
                        sleep(Self::retry_delay(attempt)).await;
                    }
                    continue;
                }
            };

            let status = response.status();
            let retry_after = Self::parse_retry_after(response.headers());

            // 成功响应
            if status.is_success() {
                self.token_manager.report_success(ctx.id);
                return Ok(ApiCallResult {
                    response,
                    credential_id: ctx.id,
                });
            }

            // 失败响应：读取 body 用于日志/错误信息
            let body = response.text().await.unwrap_or_default();

            // 402 Payment Required 且额度用尽：禁用凭据并故障转移
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                tracing::warn!(
                    "API 请求失败（额度已用尽，禁用凭据并切换，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if !failed_ids.contains(&ctx.id) {
                    failed_ids.push(ctx.id);
                }
                if !has_available {
                    anyhow::bail!(
                        "{} API 请求失败（所有凭据已用尽）: {} {}",
                        api_type,
                        status,
                        body
                    );
                }

                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                ));
                continue;
            }

            // 400 Bad Request - 大部分是请求问题；INVALID_MODEL_ID 是凭据级模型权限问题
            if status.as_u16() == 400 {
                if Self::is_invalid_model_id(&body) {
                    tracing::warn!(
                        "API 请求失败（当前凭据无此模型权限，尝试切换凭据，尝试 {}/{}）: {} {}",
                        attempt + 1,
                        max_retries,
                        status,
                        body
                    );

                    let has_available = self.token_manager.report_failure(ctx.id);
                    if !has_available {
                        anyhow::bail!(
                            "{} API 请求失败（所有凭据均不支持该模型或已不可用）: {} {}",
                            api_type,
                            status,
                            body
                        );
                    }

                    last_error = Some(anyhow::anyhow!(
                        "{} API 请求失败: {} {}",
                        api_type,
                        status,
                        body
                    ));
                    continue;
                }
                anyhow::bail!("{} API 请求失败: {} {}", api_type, status, body);
            }

            // 401/403 - 更可能是凭据/权限问题：计入失败并允许故障转移
            if matches!(status.as_u16(), 401 | 403) {
                tracing::warn!(
                    "API 请求失败（可能为凭据错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                // token 被上游失效：先尝试 force-refresh，每凭据仅一次机会
                if endpoint.is_bearer_token_invalid(&body) && !force_refreshed.contains(&ctx.id) {
                    force_refreshed.insert(ctx.id);
                    tracing::info!("凭据 #{} token 疑似被上游失效，尝试强制刷新", ctx.id);
                    if self.token_manager.force_refresh_token_for(ctx.id).await.is_ok() {
                        tracing::info!("凭据 #{} token 强制刷新成功，重试请求", ctx.id);
                        continue;
                    }
                    tracing::warn!("凭据 #{} token 强制刷新失败，计入失败", ctx.id);
                }

                let has_available = self.token_manager.report_failure(ctx.id);
                if !failed_ids.contains(&ctx.id) {
                    failed_ids.push(ctx.id);
                }
                if !has_available {
                    anyhow::bail!(
                        "{} API 请求失败（所有凭据已用尽）: {} {}",
                        api_type,
                        status,
                        body
                    );
                }

                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                ));
                continue;
            }

            if status.as_u16() == 429 {
                if let Some(cooldown) = Self::classify_429_cooldown(&body, retry_after) {
                    let applied = self
                        .token_manager
                        .set_credential_rate_limit_cooldown(ctx.id, cooldown);
                    tracing::warn!(
                        credential_id = ctx.id,
                        cooldown_secs = applied.as_secs(),
                        "{} API 请求触发 429，已设置短冷却并切换凭据",
                        api_type
                    );
                } else {
                    tracing::warn!(
                        credential_id = ctx.id,
                        "{} API 请求触发容量不足类 429，不冷却凭据，仅切换",
                        api_type
                    );
                }
                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                ));
                if !failed_ids.contains(&ctx.id) {
                    failed_ids.push(ctx.id);
                }
                continue;
            }

            // 408/5xx - 瞬态上游错误：本轮切换凭据，但不禁用
            if status.as_u16() == 408 || status.is_server_error() {
                tracing::warn!(
                    "API 请求失败（上游瞬态错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                ));
                if !failed_ids.contains(&ctx.id) {
                    failed_ids.push(ctx.id);
                }
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx - 通常为请求/配置问题：直接返回，不计入凭据失败
            if status.is_client_error() {
                anyhow::bail!("{} API 请求失败: {} {}", api_type, status, body);
            }

            // 兜底：当作可重试的瞬态错误处理（不切换凭据）
            tracing::warn!(
                "API 请求失败（未知错误，尝试 {}/{}）: {} {}",
                attempt + 1,
                max_retries,
                status,
                body
            );
            last_error = Some(anyhow::anyhow!(
                "{} API 请求失败: {} {}",
                api_type,
                status,
                body
            ));
            if !failed_ids.contains(&ctx.id) {
                failed_ids.push(ctx.id);
            }
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        // 所有重试都失败
        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!(
                "{} API 请求失败：已达到最大重试次数（{}次）",
                api_type,
                max_retries
            )
        }))
    }

    /// 从请求体中提取模型信息
    ///
    /// 尝试解析 JSON 请求体，提取 conversationState.currentMessage.userInputMessage.modelId
    fn extract_model_from_request(request_body: &str) -> Option<String> {
        use serde_json::Value;

        let json: Value = serde_json::from_str(request_body).ok()?;

        json.get("conversationState")?
            .get("currentMessage")?
            .get("userInputMessage")?
            .get("modelId")?
            .as_str()
            .map(|s| s.to_string())
    }

    /// 从请求体中提取会话 ID。
    ///
    /// 这里使用转换后的 Kiro `conversationId`，避免重复解析 Anthropic metadata。
    fn extract_session_id_from_request(request_body: &str) -> Option<String> {
        use serde_json::Value;

        let json: Value = serde_json::from_str(request_body).ok()?;

        json.get("conversationState")?
            .get("conversationId")?
            .as_str()
            .map(|s| s.to_string())
    }

    /// 检测是否为「模型 ID 无权限/不可用」类错误。
    ///
    /// Kiro 会用 400 + INVALID_MODEL_ID 表示当前凭据没有该模型权限。这是凭据级能力
    /// 差异，不代表请求体一定无效，因此应给其他凭据一次故障转移机会。
    fn is_invalid_model_id(body: &str) -> bool {
        let lower = body.to_ascii_lowercase();
        lower.contains("invalid_model_id") || lower.contains("invalid model")
    }

    fn compute_max_retries(total_credentials: usize, available: usize) -> usize {
        let budget = total_credentials.saturating_mul(MAX_RETRIES_PER_CREDENTIAL);
        let floor = available.max(1);
        budget.max(floor).min(MAX_TOTAL_RETRIES.max(floor))
    }

    fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
        let seconds = headers
            .get("retry-after")?
            .to_str()
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()?;
        Some(Self::clamp_rate_limit_cooldown(Duration::from_secs(
            seconds,
        )))
    }

    fn clamp_rate_limit_cooldown(duration: Duration) -> Duration {
        duration.clamp(
            Duration::from_secs(DEFAULT_RATE_LIMIT_COOLDOWN_SECS),
            Duration::from_secs(MAX_RATE_LIMIT_COOLDOWN_SECS),
        )
    }

    fn classify_429_cooldown(body: &str, retry_after: Option<Duration>) -> Option<Duration> {
        if Self::is_insufficient_model_capacity(body) {
            return None;
        }
        Some(Self::clamp_rate_limit_cooldown(retry_after.unwrap_or(
            Duration::from_secs(DEFAULT_RATE_LIMIT_COOLDOWN_SECS),
        )))
    }

    fn is_insufficient_model_capacity(body: &str) -> bool {
        let matches = |value: &str| {
            value
                .to_ascii_uppercase()
                .contains("INSUFFICIENT_MODEL_CAPACITY")
        };
        if matches(body) {
            return true;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return false;
        };
        value
            .get("reason")
            .and_then(|value| value.as_str())
            .is_some_and(matches)
            || value
                .pointer("/error/reason")
                .and_then(|value| value.as_str())
                .is_some_and(matches)
    }

    fn retry_delay(attempt: usize) -> Duration {
        // 指数退避 + 少量抖动，避免上游抖动时放大故障
        const BASE_MS: u64 = 200;
        const MAX_MS: u64 = 2_000;
        let exp = BASE_MS.saturating_mul(2u64.saturating_pow(attempt.min(6) as u32));
        let backoff = exp.min(MAX_MS);
        let jitter_max = (backoff / 4).max(1);
        let jitter = fastrand::u64(0..=jitter_max);
        Duration::from_millis(backoff.saturating_add(jitter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_session_id_from_request() {
        let body = r#"{"conversationState":{"conversationId":"conv-123"}}"#;

        assert_eq!(
            KiroProvider::extract_session_id_from_request(body),
            Some("conv-123".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_from_request_missing_or_invalid() {
        assert_eq!(
            KiroProvider::extract_session_id_from_request(r#"{"conversationState":{}}"#),
            None
        );
        assert_eq!(KiroProvider::extract_session_id_from_request("not-json"), None);
    }

    #[test]
    fn test_is_invalid_model_id_detects_reason_and_message() {
        assert!(KiroProvider::is_invalid_model_id(
            r#"{"message":"Model access denied","reason":"INVALID_MODEL_ID"}"#
        ));
        assert!(KiroProvider::is_invalid_model_id(
            r#"{"error":{"message":"Invalid model: claude-sonnet-4"}}"#
        ));
        assert!(KiroProvider::is_invalid_model_id(
            r#"{"message":"invalid MODEL for this credential"}"#
        ));
        assert!(!KiroProvider::is_invalid_model_id(
            r#"{"message":"Improperly formed request","reason":"BAD_REQUEST"}"#
        ));
    }

    #[test]
    fn test_compute_max_retries_covers_every_available_credential() {
        assert_eq!(KiroProvider::compute_max_retries(1, 1), 3);
        assert!(KiroProvider::compute_max_retries(10, 10) >= 10);
        assert!(KiroProvider::compute_max_retries(50, 50) >= 50);
        assert!(KiroProvider::compute_max_retries(100, 100) >= 100);
        assert!(KiroProvider::compute_max_retries(20, 7) >= 7);
    }

    #[test]
    fn test_classify_429_cooldown_and_capacity_exception() {
        assert_eq!(
            KiroProvider::classify_429_cooldown(
                r#"{"reason":"RATE_LIMIT_EXCEEDED"}"#,
                None,
            ),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            KiroProvider::classify_429_cooldown(
                r#"{"message":"temporary limits due to suspicious activity","reason":null}"#,
                Some(Duration::from_secs(90)),
            ),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            KiroProvider::classify_429_cooldown(
                r#"{"reason":"INSUFFICIENT_MODEL_CAPACITY"}"#,
                Some(Duration::from_secs(90)),
            ),
            None
        );
        assert_eq!(
            KiroProvider::classify_429_cooldown("rate limited", Some(Duration::from_secs(999))),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn metering_recorder_commits_latest_value_once() {
        let mut credential = KiroCredentials::default();
        credential.id = Some(1);
        let manager = Arc::new(MultiTokenManager::new(
            crate::model::config::Config::default(), vec![credential], None, None, false,
        ).unwrap());
        let recorder = MeteringRecorder {
            token_manager: manager.clone(),
            credential_id: 1,
            latest_credits: Arc::new(Mutex::new(None)),
            committed: Arc::new(AtomicBool::new(false)),
        };
        recorder.observe(Some(1.25));
        recorder.observe(Some(1.5));
        recorder.commit();
        recorder.commit();
        let entry = manager.snapshot().entries.remove(0);
        assert_eq!(entry.metered_credits, 1.5);
        assert_eq!(entry.metered_request_count, 1);
    }
}
