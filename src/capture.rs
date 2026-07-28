//! Opt-in, root-only request/response capture for short-lived diagnostics.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;
use tokio::fs;
use uuid::Uuid;

use crate::kiro::parser::frame::Frame;

const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Serialize)]
struct CapturedEvent {
    sequence: usize,
    message_type: Option<String>,
    event_type: Option<String>,
    payload: String,
}

#[derive(Debug, Serialize)]
struct CaptureDocument {
    capture_id: String,
    started_at: String,
    finished_at: Option<String>,
    model: String,
    stream: bool,
    credential_id: Option<u64>,
    request_body: String,
    upstream_events: Vec<CapturedEvent>,
    outcome: Option<String>,
    error: Option<String>,
    truncated: bool,
    captured_bytes: usize,
}

pub struct CaptureSession {
    directory: Option<PathBuf>,
    max_bytes: usize,
    document: CaptureDocument,
}

impl CaptureSession {
    pub fn new(request_body: &str, model: &str, stream: bool) -> Self {
        let directory = std::env::var_os("KIRO_CAPTURE_DIR").map(PathBuf::from);
        let max_bytes = std::env::var("KIRO_CAPTURE_MAX_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_MAX_BYTES);

        Self::with_config(directory, max_bytes, request_body, model, stream)
    }

    fn with_config(
        directory: Option<PathBuf>,
        max_bytes: usize,
        request_body: &str,
        model: &str,
        stream: bool,
    ) -> Self {
        let capture_id = Uuid::new_v4().to_string();
        let request_len = request_body.len();
        let (request_body, truncated, captured_bytes) = if request_len > max_bytes {
            (
                request_body[..floor_char_boundary(request_body, max_bytes)].to_string(),
                true,
                max_bytes,
            )
        } else {
            (request_body.to_string(), false, request_len)
        };

        Self {
            directory,
            max_bytes,
            document: CaptureDocument {
                capture_id,
                started_at: Utc::now().to_rfc3339(),
                finished_at: None,
                model: model.to_string(),
                stream,
                credential_id: None,
                request_body,
                upstream_events: Vec::new(),
                outcome: None,
                error: None,
                truncated,
                captured_bytes,
            },
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.directory.is_some()
    }

    pub fn capture_id(&self) -> &str {
        &self.document.capture_id
    }

    pub fn set_credential_id(&mut self, credential_id: u64) {
        self.document.credential_id = Some(credential_id);
    }

    pub fn record_frame(&mut self, frame: &Frame) {
        self.record_payload(
            frame.message_type().map(str::to_string),
            frame.event_type().map(str::to_string),
            &frame.payload_as_str(),
        );
    }

    pub fn record_payload(
        &mut self,
        message_type: Option<String>,
        event_type: Option<String>,
        payload: &str,
    ) {
        if !self.is_enabled() || self.document.truncated {
            return;
        }

        let remaining = self.max_bytes.saturating_sub(self.document.captured_bytes);
        if payload.len() > remaining {
            let end = floor_char_boundary(&payload, remaining);
            self.document.upstream_events.push(CapturedEvent {
                sequence: self.document.upstream_events.len(),
                message_type,
                event_type,
                payload: payload[..end].to_string(),
            });
            self.document.captured_bytes += end;
            self.document.truncated = true;
            return;
        }

        self.document.captured_bytes += payload.len();
        self.document.upstream_events.push(CapturedEvent {
            sequence: self.document.upstream_events.len(),
            message_type,
            event_type,
            payload: payload.to_string(),
        });
    }

    pub async fn finish(mut self, outcome: &str, error: Option<String>) {
        let Some(root) = self.directory.take() else {
            return;
        };

        self.document.finished_at = Some(Utc::now().to_rfc3339());
        self.document.outcome = Some(outcome.to_string());
        self.document.error = error;

        if let Err(err) = write_capture(&root, &self.document).await {
            tracing::error!(
                capture_id = %self.document.capture_id,
                error = %err,
                "写入 Kiro 诊断捕获失败"
            );
        }
    }
}

fn floor_char_boundary(value: &str, requested: usize) -> usize {
    let mut index = requested.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

async fn write_capture(root: &Path, document: &CaptureDocument) -> std::io::Result<()> {
    let day = Utc::now().format("%Y-%m-%d").to_string();
    let directory = root.join(day);
    fs::create_dir_all(&directory).await?;
    set_mode(&directory, 0o700).await?;

    let filename = format!("{}.json", document.capture_id);
    let path = directory.join(filename);
    let bytes = serde_json::to_vec(document).map_err(std::io::Error::other)?;
    fs::write(&path, bytes).await?;
    set_mode(&path, 0o600).await?;

    tracing::info!(
        capture_id = %document.capture_id,
        credential_id = ?document.credential_id,
        outcome = ?document.outcome,
        truncated = document.truncated,
        path = %path.display(),
        "Kiro 诊断捕获已写入"
    );
    Ok(())
}

#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await
}

#[cfg(not(unix))]
async fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        let session = CaptureSession::with_config(
            Some(PathBuf::from("/tmp/not-written")),
            5,
            "韩文abc",
            "test-model",
            true,
        );

        assert_eq!(session.document.request_body, "韩");
        assert!(session.document.truncated);
    }

    #[test]
    fn capture_is_disabled_without_directory() {
        let session = CaptureSession::with_config(None, 128, "{}", "test-model", true);
        assert!(!session.is_enabled());
    }

    #[tokio::test]
    async fn writes_private_capture_with_request_and_response() {
        let root = std::env::temp_dir().join(format!("kiro-capture-test-{}", Uuid::new_v4()));
        let mut session =
            CaptureSession::with_config(Some(root.clone()), 1024, "request", "test-model", true);
        let capture_id = session.capture_id().to_string();
        session.set_credential_id(87);
        session.record_payload(
            Some("event".to_string()),
            Some("assistantResponseEvent".to_string()),
            "response",
        );
        session.finish("complete", None).await;

        let path = root
            .join(Utc::now().format("%Y-%m-%d").to_string())
            .join(format!("{}.json", capture_id));
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(document["request_body"], "request");
        assert_eq!(document["upstream_events"][0]["payload"], "response");
        assert_eq!(document["credential_id"], 87);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        fs::remove_dir_all(root).await.unwrap();
    }
}
