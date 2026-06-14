# kiro.rs 后续优化分析

> 对应 #token_proxy task #13（2026-06-15）。本文只分析当前 `DonYum/kiro.rs`
> 自维护分支还能优化什么，不包含本次未验证的代码改动。

## 结论先行

当前 `kiro.rs` 的核心链路已经从"能跑"推进到"可生产使用"：Kiro metering credits 已透传给
sub2api，会话粘性和 balanced round-robin 已部署，Anthropic/Claude 兼容层也合入了必要的上游修复。

下一阶段最有价值的优化不是继续堆复杂调度，而是三类：

1. **安全兜底**：避免 debug 排障时把 `Authorization`、refresh token、API key 打进日志。
2. **可观测性闭环**：把"当前账号池为什么只选某个账号"从日志 grep 变成可查询的结构化指标。
3. **路由与缓存质量**：针对缺 `metadata.user_id`、瞬态 429/5xx、Opus 订阅过滤等边界做可控优化。

## 当前基线

- 本地分支基线：`origin/master@179d825`，即已部署的 balanced sticky round-robin 修复。
- 生产容器抽样：`peaceful_hellman`，镜像 `kiro-rs:balanced-sticky-rr-20260614`，容器已运行约 13 小时。
- 生产健康抽样：`/admin -> 200`，`/v1/models` 无 key `-> 401`。
- 最近 2 小时日志抽样：只看到 1 条上游瞬态 500 warn，没有 ERROR/panic。
- 没有读取或输出生产 `credentials.json` / `config.json` 内容。

## 观察到的待验证现象

### 1. 近 2 小时 sticky 日志仍只落到 `credential_id=10`

只基于日志聚合，近 2 小时：

| 日志类型 | 账号分布 |
|---|---|
| `Kiro sticky session bound` | `credential_id=10` 共 190 次 |
| `Kiro sticky session hit` | `credential_id=10` 共 281 次 |

这不能直接判定 task #12 的修复失效，因为没有读取凭据内容，无法区分以下情况：

- 最高优先级可调度池当前确实只剩账号 10。
- 其他账号对当前 Opus 模型不满足 `supports_opus()` 过滤。
- 其他账号被 quota / refresh / failure 自动禁用。
- round-robin 对新会话有效，但日志窗口刚好只覆盖某类探针/业务流量。
- 调度仍存在未暴露的边界问题。

因此它不是一个可直接改代码的 bug 结论，而是一个强烈的可观测性缺口：需要系统自己报告"当时的候选池有哪些、为什么过滤掉哪些账号、最终为什么选 10"。

### 2. 约 22% 请求缺 `metadata.user_id`

近 2 小时 `Anthropic request fingerprint` 抽样：

| `metadata_user_id_present` | 数量 |
|---|---:|
| `true` | 367 |
| `false` | 103 |

缺 `metadata.user_id` 时，转换层会生成新的随机 `conversationId`。结果是：

- 同一真实会话无法稳定映射到同一 Kiro `conversationId`。
- balanced sticky 无法基于真实会话稳定命中。
- Kiro 侧 prompt/cache 复用基础变弱。

这类流量可能来自非 Claude Code 客户端、探针、或上游代理改写。需要先在不记录原始用户 ID 的前提下增加来源/路径/客户端指纹统计，再决定是否引入可配置 fallback。

## P0：安全日志脱敏

### 问题

当前已通过 `sensitive-logs` feature gate 保护完整请求体，但仍有两个风险点：

- `src/kiro/provider.rs` 在 `RUST_LOG=debug` 时会遍历打印实际请求头，可能包含 `Authorization: Bearer <token>`。
- `src/main.rs` 有 `tracing::debug!("主凭证: {:?}", first_credentials);`，debug 启动时可能打印 refreshToken、accessToken、clientSecret、kiroApiKey 等字段。

在生产排障中临时打开 `RUST_LOG=debug` 是高概率操作，这两个点属于凭据泄露风险。

### 建议

- 默认日志永远不输出 credential/debug struct 的敏感字段。
- header debug 只输出 allowlist 或对敏感头脱敏：
  - `authorization`
  - `x-api-key`
  - `proxy-authorization`
  - `cookie`
  - 任何包含 `token` / `secret` / `key` 的 header
- 如果确实需要完整 header/body，只允许在 `sensitive-logs` feature 下输出，并在启动日志中明确提示。

### 成功标准

- `RUST_LOG=debug` 且未启用 `sensitive-logs` 时，不出现 bearer token、refresh token、client secret、API key。
- 增加单测覆盖 header redaction 函数。
- 用 `rg "Authorization|refreshToken|clientSecret|kiroApiKey"` 检查日志调用点。

### 取舍

会降低 debug 时的直接可见性，但保留 hash/长度/host/endpoint 等派生信息足够定位大多数协议问题。真实全量排障仍可通过显式 `sensitive-logs` 编译产物完成。

## P0：调度可观测性与池状态快照

### 问题

task #12 的根因是靠人工查 `kiro_stats.json` 和日志定位的。当前线上再次看到 2 小时只选 `credential_id=10`，仍然无法从系统输出中直接回答"为什么只选 10"。

### 建议

增加一个无密钥、无 token 的只读状态视图，优先放在 Admin API，必要时再接 Prometheus/metrics：

- 当前 load balancing mode。
- 每个 credential 的脱敏 ID、priority、disabled、disabled_reason、failure_count、refresh_failure_count、success_count、last_used_at。
- 每个模型类别下的候选池：
  - `sonnet_pool`
  - `opus_pool`
  - 被过滤原因：disabled、unsupported_opus、lower_priority、quota_exceeded、invalid_refresh_token 等。
- balanced cursor 当前值。
- 近 N 分钟 route 计数：`bound_count`、`hit_count`、`request_count`，按 `credential_id` 和 `model` 聚合。
- sticky active session count，按 credential 聚合。

### 成功标准

- 不读 credentials 文件内容，也能解释"为什么某一分钟只选账号 10"。
- 线上不需要 grep 彩色日志即可给出账号分布。
- 输出不包含 refresh token、access token、client secret、kiro api key、原始 session id。

### 取舍

内存聚合会增加少量状态维护。建议先做进程内 ring buffer，不上 DB，不追求长期报表；长期趋势交给 sub2api usage_logs 或外部日志系统。

## P1：请求级 trace_id，打通 kiro.rs 与 sub2api

### 问题

当前 kiro.rs 有 `session_hash`、`metadata_user_id_hash`、fingerprint、credits 日志；sub2api 有 usage row 和 cost 字段。但两边没有同一个请求级 ID，导致只能按时间窗口模糊对齐。

这会影响以下问题的定位：

- 某一条 sub2api usage 为什么折扣率异常。
- 某一条 kiro.rs sticky hit 是否对应到某条 sub2api 账单。
- 同一请求的 model / account / credits / upstream status / stop_reason 是否一致。

### 建议

- 在入口生成 `request_trace_id`，优先透传已有 `x-request-id` / `anthropic-request-id` / sub2api 自定义 header。
- kiro.rs 所有关键日志带同一个 `request_trace_id`：
  - request fingerprint
  - selected credential
  - sticky hit/bind/unbind
  - upstream status / retry
  - meteringEvent credits
  - final response usage
- 响应里可选返回 `x-kiro-rs-trace-id`，方便 sub2api 记录。

### 成功标准

- 任意一条 sub2api usage row 可定位到 kiro.rs 对应日志链。
- 不暴露原始 session/user/credential secret。

### 取舍

这不直接提升吞吐或缓存，但能显著降低后续排障成本。优先级应高于大规模调度重构。

## P1：缺 `metadata.user_id` 的会话 fallback 策略

### 问题

约 22% 生产请求缺 `metadata.user_id`。当前这类请求每次转换都会生成随机 `conversationId`，使会话级 sticky 和 Kiro 缓存都失去稳定锚点。

### 建议

先只观测，不立刻改路由：

- 按 endpoint、client header、model、tools_hash、system_hash 统计缺失来源。
- 如果缺失主要来自固定客户端，可以让上游 sub2api 或客户端补稳定 `metadata.user_id`。
- 如果无法上游修复，再考虑可配置 fallback key，例如：
  - `x-session-id` / `x-conversation-id` header。
  - sub2api 注入的 request/user/session 标识。
  - 对非 Claude Code 流量，允许用 `api_key_hash + client_user_hash + conversation_hint` 作为 sticky key。

### 不建议

不建议用 message hash 直接当会话 key。消息会随上下文增长、压缩、工具结果变化，稳定性差；还可能把不同真实会话错误粘到一起。

### 成功标准

- 缺 `metadata.user_id` 的比例下降，或这类请求也有可解释的稳定 session key。
- 同一真实会话 sticky hit 率提升。
- Kiro credits 折扣率不下降。

## P1：瞬态 429/5xx 的有限 failover 机制

### 问题

当前 `provider.rs` 对 408/429/5xx 的策略是重试但不禁用、不切账号。这保护了缓存，也避免把网络抖动误判成账号坏。但代价是：如果单个 Kiro 账号进入短时 high traffic / 500 状态，同一请求可能一直打同一账号，直到总重试耗尽。

### 建议

不要回到"遇到 429/5xx 就全局禁用账号"。更安全的方案是引入短 TTL 的 per-credential cooldown：

- 只对特定可识别的短时拥塞错误生效，例如 high traffic、throttling、500 burst。
- cooldown 不改变 `disabled`，不写 credentials，不清长期 sticky。
- 当前请求可以在同优先级池内尝试其他账号。
- 对已 sticky 的会话，只有当 sticky 账号进入 cooldown 才临时旁路；成功后是否回粘需明确策略。

### 成功标准

- 单账号短时 429/5xx 不会导致同请求 9 次都打同一账号。
- 不把普通网络抖动误禁用为永久失败。
- Kiro 缓存损失仅发生在错误场景，不影响正常 sticky。

### 取舍

会牺牲少量缓存命中来换取可用性。建议做成小范围、短 TTL、可配置，默认保守。

## P1：修复已知失败测试，恢复全量门禁可信度

### 问题

当前 `cargo test --locked kiro:: -- --nocapture` 有一个已知失败：

- `kiro::model::requests::kiro::tests::test_kiro_request_deserialize`
- 原因是测试 JSON 缺少 required `envState`。

这不是生产故障，但会让后续所有 kiro.rs 修改都无法用"全量测试绿"作为验收条件。

### 建议

- 更新测试 fixture，补齐当前协议必填字段。
- 或者如果真实协议允许缺失，就把模型字段改成 optional 并加兼容测试。

### 成功标准

- `cargo test --locked kiro:: -- --nocapture` 全绿。
- 后续 PR 不再需要解释"这个失败是旧问题"。

## P1：模型与订阅能力过滤可解释化

### 问题

`select_next_credential(model)` 对 Opus 模型会用 `supports_opus()` 过滤凭据。当前生产主要流量是：

| model | 近 2 小时请求数 |
|---|---:|
| `claude-opus-4-8` | 222 |
| `claude-opus-4-7` | 105 |
| `claude-sonnet-4-6` | 103 |
| `claude-opus-4-6` | 40 |

如果账号 7/8/9 的订阅信息过期、为空、或被判定不支持 Opus，那么 Opus 流量只走 10 是合理结果；但当前没有输出让运维直接确认。

### 建议

- 在 Admin 状态中展示每个账号对 `opus` 的判定结果和来源：订阅等级、上次刷新时间、是否未知。
- 当模型过滤导致候选池只剩 1 个账号时，打一条 rate-limited info/warn：
  - `model`
  - `selected_pool_size`
  - `filtered_disabled_count`
  - `filtered_unsupported_opus_count`
  - `filtered_lower_priority_count`

### 成功标准

- 看到"只选账号 10"时，能直接回答是否由 Opus 订阅过滤导致。
- 不需要人工读 credentials 文件。

## P2：账号命名与运维识别

### 问题

生产沟通中一直用数字 ID（7/8/9/10）。数字 ID 对脚本友好，但不利于人判断账号来源、用途、优先级策略和故障影响面。

### 建议

增加可选 `name` / `label` 字段，仅用于 Admin UI/API 展示和日志脱敏显示，例如：

- `id=10 name=kiro-opus-paid-04`
- `id=6 name=cfjwlpro-low-priority-fallback`

### 成功标准

- 日志和 Admin UI 同时显示 `credential_id` + `credential_name`。
- 名称不参与调度，不影响 credentials 兼容。

### 取舍

收益主要是运维效率。可以参考 upstream open PR #178 的方向，但不建议整包合并，优先做后端最小字段和展示。

## P2：凭据验证与定期巡检

### 问题

当前账号状态主要在请求路径上被动发现：refresh 失败、quota exhausted、API 失败后再处理。对账号池来说，更理想的是在低峰期主动巡检。

### 建议

- Admin API 增加只读验证 endpoint，或后台定时巡检：
  - refresh token 是否即将过期。
  - usage limits 是否可获取。
  - subscription title 是否为空/过期。
  - API key 凭据是否能访问当前默认 endpoint。
- 巡检结果只更新状态，不自动改变优先级；禁用动作仍保守。

### 成功标准

- 高峰前能发现不可用账号。
- 不需要等业务请求撞错才知道账号坏。

## P2：配置与统计文件原子写

### 问题

`persist_credentials()`、`save_stats()`、`save_balance_cache()` 直接 `std::fs::write` 到目标文件。正常情况下足够，但进程崩溃或磁盘异常时可能留下半写文件。

### 建议

- 写临时文件。
- `fsync` 临时文件。
- 原子 rename 到目标路径。
- 可选保留 `.bak`。

### 成功标准

- kill -9 / 磁盘异常测试下，credentials/config/stats 不出现半截 JSON。
- 读失败时有清晰错误和恢复路径。

## P2：减少日志噪音，保留聚合价值

### 问题

当前每个请求都会输出 `Anthropic request fingerprint`，每个 sticky hit/bind 也输出 info。排障时有用，但高流量下会造成日志量大、grep 成本高，也可能挤掉真正异常。

### 建议

- fingerprint 降到 debug，或按采样率输出。
- routing 指标走聚合计数，异常/单账号独占才 info/warn。
- credits 可按 request_trace_id 输出，但避免每个小探针都刷屏。

### 成功标准

- 正常流量日志量下降。
- 仍能通过 metrics/admin snapshot 判断路由和折扣。

## P3：OpenAI endpoint / webhook / 大 UI 改造暂不建议

upstream 当前还有 OpenAI compatible endpoint、webhook/email、Admin UI 大改等 PR。它们不是没有价值，但不建议现在进入主线：

- 当前核心问题集中在 `Claude Code -> sub2api -> kiro.rs -> Kiro` 链路。
- OpenAI endpoint 会扩大协议面，增加测试和账单语义复杂度。
- webhook/email 属于运维通知体系，只有在指标闭环完成后才有意义。
- UI 大改容易掩盖后端状态语义不清的问题。

建议先把 P0/P1 做扎实，再考虑这些产品化能力。

## 建议实施顺序

1. P0 安全日志脱敏。
2. P0 调度可观测性与池状态快照。
3. P1 请求级 trace_id。
4. P1 缺 `metadata.user_id` 来源统计与 fallback 设计。
5. P1 瞬态 429/5xx 短 TTL cooldown。
6. P1 修复全量测试失败。
7. P2 账号命名、凭据巡检、原子写、日志采样。

## 验收口径

这次分析任务本身的验收标准：

- 只读分析，不修改生产逻辑。
- 明确 P0/P1/P2/P3 优先级和取舍。
- 每个建议都有成功标准。
- 记录生产抽样现象，但不把无法拆因的现象武断定性为 bug。
