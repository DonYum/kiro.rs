# Upstream PR Merge Log

## 2026-06-14: hank9999/kiro.rs #168 + #74 + #162

- Maintained repo: `DonYum/kiro.rs`
- Upstream repo: `hank9999/kiro.rs`
- Base: `origin/master` at `01f5f3b`
- Branch: `feature/upstream-pr-168-74-162`

### Scope

- #168 `c342186`: IdC / Builder ID credential requests no longer send `profileArn` where the upstream endpoint rejects it.
- #74 `4a5be1e`: adds the `sensitive-logs` feature flag so full request bodies are only logged when explicitly enabled.
- #162 `cfd132e`, `c4c8701`, `a3da663`: adds CLI endpoint support and tool schema compatibility fixes, including `application/x-amz-json-1.0`, `x-amz-target`, CLI origin metadata, `envState`, and empty tool description fallback.

### Conflict And Local Resolution

- `Cargo.toml`: #74 introduced a second `[features]` table. Resolution: keep the existing feature table and add only `sensitive-logs = []`.
- `src/anthropic/converter.rs`: #162 conflicted with maintained cache diagnostics. Resolution: keep upstream schema normalization and empty-description fallback while preserving existing cache-point insertion and fingerprint-related observability.
- `src/kiro/provider.rs`: #162 added final request-body debug logging after #74 had gated request-body logs. Resolution: keep the upstream debug point, but gate full body output behind `sensitive-logs`; default logging reports only byte length.

### Verification

- `git diff --check`: passed.
- `cargo check --locked`: passed in a disposable `rust:1.91` Docker container on `root@47.77.226.212` after copying the existing local `admin-ui/dist` build output into the temporary source tree. Local Cargo was not used because dependency resolution on the Mac was blocked by a machine-level `crates-io` replacement pointing to `git://mirrors.ustc.edu.cn/crates.io-index`.
- `cargo fmt --check`: not completed; local rustup has no default `cargo-fmt` toolchain configured, and the disposable `rust:1.91` container did not include the `rustfmt` component.

### Notes For Future Upstream PR Merges

- Re-check all full request/response body logs after merging PRs that touch provider or handler paths. They must either be behind `sensitive-logs` or log only derived metadata.
- When merging endpoint changes, verify whether the request shape changes for the existing IDE endpoint, not only for the newly added endpoint.
- Keep maintained production observability and cache/sticky behavior unless the upstream PR directly supersedes it and the replacement is verified.
