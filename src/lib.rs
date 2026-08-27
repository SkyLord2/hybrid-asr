mod assets;
mod backend;
mod config;
mod error;
mod event;
mod local_backend;
mod session;
mod types;

pub mod engine {
    pub mod local;
    pub mod online;
    pub mod online_auth;
    pub mod online_protocol;
}

pub use assets::{
    PreparedLocalAsrAssets, create_model_download_observer, download_local_asr_assets,
    download_local_asr_assets_with_event_hub, inspect_local_asr_assets_download,
    prepare_local_asr_assets, prepare_local_asr_assets_with_event_hub,
};
pub use backend::RealtimeAsrBackend;
pub use config::{
    DEFAULT_ONLINE_ASR_URL, DEFAULT_ONLINE_AUDIO_FORMAT, DEFAULT_ONLINE_FRAME_BYTES,
    DEFAULT_ONLINE_VISIT_K_CODE, HybridAsrConfig, LocalAsrConfig, MAX_ONLINE_FRAME_BYTES,
    OnlineAsrConfig, infer_app_auth_url_from_online_url,
};
pub use error::{HybridAsrError, HybridAsrResult};
pub use event::{
    AsrEvent, AsrStateChangeReason, AsrStateTransitionEvent, EventHub as AsrEventHub,
    ModelLifecycleEvent, ModelLifecyclePhase, ModelResourceKind, SessionEvent,
};
pub use session::HybridAsrSession;
pub use types::{AsrMode, AsrResult, AsrState};
