//! WKWebView backend stub (macOS). Returns `PlatformUnsupported` in v1.
//! Post-v1 work replaces this with a real implementation.

use super::{background::BackgroundHandle, Backend, InjectionRequest};
use crate::{registry::ExtensionId, Error, Result};

/// Placeholder. Exists to keep the Backend trait platform-agnostic from day
/// one — the real impl is not scheduled for v1.
pub struct WkWebViewBackend;

#[async_trait::async_trait]
impl Backend for WkWebViewBackend {
    async fn inject(&self, _request: InjectionRequest) -> Result<()> {
        Err(Error::PlatformUnsupported)
    }

    async fn spawn_background(
        &self,
        _extension: ExtensionId,
        _manifest: serde_json::Value,
    ) -> Result<BackgroundHandle> {
        Err(Error::PlatformUnsupported)
    }
}
