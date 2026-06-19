# kiro.rs balanced 模式负载均衡修复（round-robin + 会话粘性）

> 对应 #token_proxy task #12（2026-06-14）。修复"balanced 模式下流量长时间全压单个账号"的调度缺陷。配套见 `docs/kiro-cache-investigation.md`。

## 一、问题现象

生产 kiro.rs 用 balanced（均衡）负载均衡模式，配置 4 个账号。线上多人同时使用时，**全部流量持续约 13 小时压在同一个账号上**，其余 3 个账号几乎无流量。预期应是新会话分散到 4 个账号。

## 二、根因（调度机制缺陷，非限流/非配置/非 bug 误用）

诊断经过了几轮假设排除（priority 配置、账号限流、session sticky 副作用均被只读核查排除），最终从生产 `kiro_stats.json` 定位：

- balanced 模式的 "least-used" 选账号用的是**持久化、跨重启累加的累计 `success_count`**。
- 账号样本：3 个老账号累计 ~8300，1 个**昨晚新加的账号累计仅 3566**（从 0 起）。
- least-used 逻辑判定新账号"用得最少"，于是**把所有流量灌给它去追平**，按当前流量速率追平 ~4700 的差距需要十几个小时——表现就是"13 小时单账号垄断"。

**本质**：用"累计历史计数"而非"近期分布"做均衡，导致任何新加/被重置的低计数账号会长时间垄断流量，而不是即时均摊。

## 三、修复方案

只改 `src/kiro/token_manager.rs` + `src/kiro/provider.rs`：

1. **balanced 选新账号：累计 `success_count` 最少 → 进程内 round-robin**
   - 在**当前可调度池**（最高优先级、未 disabled、模型支持）上取模轮转：`index = cursor % normal_pool.len()`，`cursor` 自增。
   - `normal_pool` 先按账号 id 排序保证顺序稳定；冷却/禁用账号被过滤出池，游标天然跳过。
   - 低优先级账号仍只在最高优先级池不可用时 fallback。
   - 不依赖历史计数 → 新账号/重置账号不再垄断，即时均摊。

2. **会话级账号粘性（session sticky）正式合入**
   - key = 转换后的 Kiro `conversationState.conversationId`（由 Anthropic `metadata.user_id` 的 session UUID 派生），只存 SHA256 hash，不持久化。
   - TTL 30 分钟，命中滑动续期。
   - **只在 balanced 模式生效**；priority 模式保持原 current_id/优先级行为。
   - round-robin 只用于"新会话首次绑定"和"无 session_id 请求"；已绑定会话直接命中 sticky，不参与轮转。
   - **不做活跃 session 的常规迁移**——迁移会把会话从有缓存的账号挪到无缓存账号，主动打散 Kiro 缓存。再平衡仅作保护机制：凭据失败/禁用/额度耗尽/refreshToken 永久失效时解绑；切换负载均衡模式时清空 sticky。

3. **日志**：只输出 `session_hash` / `credential_id` / `selection_reason` / `active_session_count`，不输出原始 session 或凭据。

## 四、设计权衡（重要）

用户最初设想的是"定期统计各账号 session 数，多的往少的迁移"。**没有采用**，因为：
- Kiro 缓存按账号隔离，迁移活跃会话 = 主动丢弃该会话在原账号建立的缓存前缀，与缓存命中目标直接对冲。
- 真实目标是"别压垮单账号"，用 round-robin 分散**新会话**即可达成，无需迁移**已有活跃会话**。

最终方案："新会话 round-robin 均摊 + 已有会话粘账号保缓存"，同时满足负载均衡和缓存命中，不互相牺牲。

## 五、验证

- 单测：`test_balanced_round_robin_distributes_new_sessions`、`test_balanced_round_robin_ignores_historical_success_count_skew`（账号 1/2/3 各 +100 计数模拟生产 skew，发 8 个不同 session，断言 4 账号各得 2 个）、`test_balanced_round_robin_skips_disabled_and_uses_same_priority_pool`。`cargo test kiro::token_manager::tests` 49 passed、`kiro::provider::tests` 2 passed。
- 生产上线后日志铁证：新会话 round-robin 分布 `8 → 9 → 10 → 7 → 8 → 9 → 10 → 7`（账号 10 正常进入轮转，不再独占）；同 `session_hash` 持续命中同 `credential_id`（sticky 生效）。

## 六、部署信息

- 镜像 `kiro-rs:balanced-sticky-rr-20260614`，commit `179d825`（master 已 fast-forward 对齐，避免后续从 master 构建回退本修复）。
- 配置保持 `loadBalancingMode=balanced`。
- 生产备份：`/root/kiro2api-backups/20260614-1308-balanced-sticky-rr`，旧容器保留可回滚。

## 七、后续可观测建议

观察真实业务流量下 4 个账号的分钟级请求分布。注意：因 sticky 粘性，分布会受各 session 活跃度影响，不会像探测请求那样绝对均匀——只要没有单账号长期独占即正常。
