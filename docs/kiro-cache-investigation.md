# Kiro 缓存命中与计费调查（kiro.rs 侧）

> 本文档记录 2026-06 针对"Claude Code 经 sub2api → kiro.rs → Kiro 链路缓存命中率差、计费偏高"问题的完整调查与 kiro.rs 侧的改动。配套文档见 sub2api 仓库 `docs/kiro-cache-investigation.md`。

## 1. 背景问题

用户现象：用 sub2api 代理的 Kiro 账号池，十几个 API key 中只有个别 key 缓存命中正常，其余很差；且怀疑缓存命中了但计费按"未命中"满价收。

最终定位为**多个独立问题叠加**，跨 sub2api 和 kiro.rs 两个项目。本文只记 kiro.rs 侧。

## 2. 关键事实：Kiro 的两个后端，事件不同

kiro.rs 当前打的是 **CodeWhisperer IDE 端点** `q.{region}.amazonaws.com/generateAssistantResponse`，
该端点返回 `meteringEvent`（credits）+ `contextUsageEvent`，**不返回** `metadataEvent.tokenUsage`。

对比：社交登录账号走的 **Amazon Q CLI runtime** `runtime.{region}.kiro.dev/` 才返回
`metadataEvent.tokenUsage.cacheReadInputTokens / cacheWriteInputTokens`（标准 Anthropic cache token）。

经实验验证（见第 4 节）：把 kiro.rs 请求完全复刻成 CLI runtime 形态（endpoint + X-Amz-Target +
origin=KIRO_CLI + UA=AmazonQ-For-CLI + profileArn）后，用 **AWS IAM Identity Center 企业 SSO** 凭证
请求，Kiro **仍然只返回 meteringEvent**。

**结论**：能否拿到标准 cache token 由**账号体系**决定（社交登录 vs 企业 SSO），不是代理软件或 endpoint 能改的。
企业 SSO 账号只能拿到 credits，缓存折扣体现在 credits 数值下降，而非标准 cache token 字段。

## 3. kiro.rs 侧的改动

### 3.1 暴露 Kiro metering usage（commit 链 `feature/kiro-metering-usage`）
- 新增 `meteringEvent` / `metadataEvent` 的 `tokenUsage` 解析（`src/kiro/model/events/usage.rs`）。
- 三条响应路径（非流式、流式 message_delta、缓冲流 message_start）在 Anthropic `usage` 里追加非标准字段：
  `upstream_kiro_credits` / `upstream_kiro_input_tokens` / `upstream_kiro_output_tokens`。
- **不伪造** `cache_creation_input_tokens` / `cache_read_input_tokens`（保持上游事实）。
- 目的：把 Kiro 的 credits 信号透传给 sub2api，供其计费与展示。

### 3.2 会话级账号粘性（commit 链 `feature/kiro-session-sticky`）
- 问题：`balanced` 负载均衡模式下，同一会话的连续请求被分散到不同 Kiro 账号；
  而 Kiro 缓存按账号隔离，换账号即丢缓存前缀 → 命中率差。
- 修复：在 `MultiTokenManager::acquire_context_for_session` 选择路径里加进程内
  `session_hash -> credential_id` 粘性映射。
  - **绑定 key**：从 `metadata.user_id` 提取的 session UUID（复用 `extract_session_id`），
    无 UUID 则降级原逻辑，**不用消息哈希**（避免碰撞）。
  - **存储**：进程内 `HashMap`，TTL 30 分钟，lookup 时清过期；不引入 Redis/DB/LRU。
  - **ctx 一致性**：粘性在选择路径内完成，选中的 `(id, credentials, token)` 三元组保持一致，
    refresh/report_success/report_failure 记在正确账号。
  - **优先级 fallback**：`balanced` 仅在当前最高优先级可用池内 least-used；低优先级账号只作
    fallback **且不写粘性绑定**（避免偶发兜底把会话永久锁死——与 sub2api 侧 sticky 修复同思路）。
  - **解绑**：quota 耗尽 / refresh 失败 / 连续失败达禁用阈值 / 手动 disable / delete /
    priority 变更后不再属于最高优先级池时解绑重选；普通瞬态 429/5xx 不解绑。
- 日志只打 `session_hash` 前缀 + `credential_id`，不输出原始 session UUID / metadata。

## 4. 调查中验证过、最终未采用的路线

`src/kiro/endpoint/runtime.rs`（实验分支，未合入主线）曾尝试切到 CLI runtime 端点以拿 metadataEvent。
结论见第 2 节：企业 SSO 凭证在 runtime 端点仍只返回 meteringEvent，故该路线关闭，标准 cache token 不可得。

## 5. 验收口径（重要）

- **不要用"复用 session 的原始 credits 是否都下降"判断缓存是否生效。** 真实多轮对话每轮新增内容是
  无缓存、需全价的，所以每轮 credits 会停在"缓存前缀(便宜)+新增内容(全价)"水平，不会降到接近 0
  （受控实验能降 86% 是因为发的是字节级完全相同、无新增内容的请求）。
- **正确指标是 sub2api 侧的"折扣率估算"**（credits vs 该请求全价 list price），它扣掉了"新增内容"
  这个变量，反映"相比完全不缓存省了多少"。
- kiro.rs 侧可验证：日志中 `Kiro sticky session bound/hit`、同 session_hash 命中同 credential_id、
  缓存命中级别的低 credits（如 keep-alive 探针的 0.018~0.026）确实存在。

## 6. 相关 PR

- `feature/kiro-metering-usage`：暴露 Kiro metering usage 字段。
- `feature/kiro-session-sticky`：会话级账号粘性。
