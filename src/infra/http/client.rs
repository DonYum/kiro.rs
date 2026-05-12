//! HTTP Client 构建（迁移自 src/http_client.rs，TlsBackend 切到 config::net::TlsBackend）

use std::fmt;
use std::time::Duration;

use reqwest::{Client, Proxy};

use crate::config::net::TlsBackend;
use crate::domain::error::KiroError;

/// 代理配置（凭据级或全局，按 ProxyConfig 维度做 Client 缓存）
///
/// `Debug` 自定义实现把 `password` 脱敏为 `[REDACTED]`，避免 `{:?}` 打日志时泄露。
/// 业务路径打印 URL 时请用 [`mask_proxy_url`] 脱敏 `proxyUrl` 中可能内嵌的凭据。
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct ProxyConfig {
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ProxyConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            username: None,
            password: None,
        }
    }

    pub fn with_auth(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }

    /// 适合在日志/UI 中显示的 URL：脱敏 userinfo 部分的 password。
    pub fn display_url(&self) -> String {
        mask_proxy_url(&self.url)
    }
}

impl fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("url", &mask_proxy_url(&self.url))
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// 把 proxy URL 中的 password 替换为 ****（保留 scheme/user/host:port）。
///
/// 仅识别 `<scheme>://[user:password@]host:port` 形式；无 password / 解析失败时原样返回。
pub fn mask_proxy_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let (scheme, rest) = url.split_at(scheme_end);
    let after = &rest[3..];
    let Some(at_pos) = after.find('@') else {
        return url.to_string();
    };
    let userinfo = &after[..at_pos];
    let host_port = &after[at_pos + 1..];
    let Some(colon_pos) = userinfo.find(':') else {
        return url.to_string();
    };
    let user = &userinfo[..colon_pos];
    format!("{scheme}://{user}:****@{host_port}")
}

/// 构建 reqwest::Client
pub fn build_client(
    proxy: Option<&ProxyConfig>,
    timeout_secs: u64,
    tls_backend: TlsBackend,
) -> Result<Client, KiroError> {
    let mut builder = Client::builder().timeout(Duration::from_secs(timeout_secs));

    match tls_backend {
        TlsBackend::Rustls => {
            builder = builder.use_rustls_tls();
        }
        TlsBackend::NativeTls => {
            #[cfg(feature = "native-tls")]
            {
                builder = builder.use_native_tls();
            }
            #[cfg(not(feature = "native-tls"))]
            {
                return Err(KiroError::Endpoint(
                    "此构建版本未包含 native-tls 后端，请在配置中改用 rustls".into(),
                ));
            }
        }
    }

    if let Some(proxy_config) = proxy {
        let mut proxy = Proxy::all(&proxy_config.url)?;
        if let (Some(username), Some(password)) = (&proxy_config.username, &proxy_config.password) {
            proxy = proxy.basic_auth(username, password);
        }
        builder = builder.proxy(proxy);
        tracing::debug!("HTTP Client 使用代理: {}", proxy_config.url);
    }

    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_config_new_has_no_auth() {
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        assert_eq!(config.url, "http://127.0.0.1:7890");
        assert!(config.username.is_none());
        assert!(config.password.is_none());
    }

    #[test]
    fn proxy_config_with_auth_sets_credentials() {
        let config = ProxyConfig::new("socks5://127.0.0.1:1080").with_auth("user", "pass");
        assert_eq!(config.url, "socks5://127.0.0.1:1080");
        assert_eq!(config.username, Some("user".to_string()));
        assert_eq!(config.password, Some("pass".to_string()));
    }

    #[test]
    fn build_client_without_proxy_succeeds() {
        let client = build_client(None, 30, TlsBackend::Rustls);
        assert!(client.is_ok());
    }

    #[test]
    fn build_client_with_proxy_succeeds() {
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        let client = build_client(Some(&config), 30, TlsBackend::Rustls);
        assert!(client.is_ok());
    }

    #[test]
    fn debug_redacts_password_and_masks_userinfo_in_url() {
        let config = ProxyConfig::new("http://user:secret@host:8080").with_auth("u", "very-secret");
        let s = format!("{config:?}");
        assert!(!s.contains("secret"), "raw secret leaked: {s}");
        assert!(!s.contains("very-secret"), "raw with_auth pwd leaked: {s}");
        assert!(s.contains("[REDACTED]"));
        assert!(s.contains("user:****@host"));
    }

    #[test]
    fn display_url_masks_inline_password() {
        let cfg = ProxyConfig::new("http://user:pass@proxy:3128");
        assert_eq!(cfg.display_url(), "http://user:****@proxy:3128");

        let plain = ProxyConfig::new("http://proxy:3128");
        assert_eq!(plain.display_url(), "http://proxy:3128");
    }

    #[test]
    fn mask_proxy_url_handles_edge_cases() {
        assert_eq!(mask_proxy_url("not-a-url"), "not-a-url");
        assert_eq!(mask_proxy_url(""), "");
        assert_eq!(mask_proxy_url("http://user@host"), "http://user@host");
    }
}
