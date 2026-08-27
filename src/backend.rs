use crate::config::HybridAsrConfig;
use crate::engine::online::OnlineAsrBackend;
use crate::error::HybridAsrResult;
use crate::event::AsrEventHub;
use crate::local_backend::LocalAsrBackend;
use crate::types::AsrResult;
use std::sync::Arc;

/// Runtime backend contract shared by local and online realtime ASR engines.
///
/// The trait keeps the upper session state machine agnostic to backend details:
/// each backend only needs to expose streaming accept, lifecycle reset, and the
/// new "prepare next turn" hook used by the smart-call module for hot reuse.
pub trait RealtimeAsrBackend: Send {
    fn start(&mut self) -> HybridAsrResult<()>;
    fn accept_samples(&mut self, samples: &[f32]) -> HybridAsrResult<Vec<AsrResult>>;
    fn reset(&mut self) -> HybridAsrResult<()>;

    /// Clears per-turn buffers while keeping the underlying backend initialized.
    ///
    /// Local backends map this to an in-memory engine reset. Online backends may
    /// either preserve the socket or reconnect internally, but callers keep the
    /// same session object and do not pay the full re-construction cost again.
    fn prepare_next_turn(&mut self) -> HybridAsrResult<()>;

    fn finish(&mut self) -> HybridAsrResult<Vec<AsrResult>>;
}

pub fn create_realtime_asr_backend(
    config: HybridAsrConfig,
    event_hub: Option<Arc<AsrEventHub>>,
) -> HybridAsrResult<Box<dyn RealtimeAsrBackend>> {
    match config {
        HybridAsrConfig::Local(config) => Ok(Box::new(LocalAsrBackend::new(config, event_hub)?)),
        HybridAsrConfig::Online(config) => Ok(Box::new(OnlineAsrBackend::new(config))),
    }
}
