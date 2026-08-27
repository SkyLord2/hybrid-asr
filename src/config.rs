use std::path::{Path, PathBuf};

use crate::types::AsrMode;

/// 本地引擎内部 VAD/切段缓冲必须与顶层实时 ASR 入口的 60 秒预算对齐，
/// 否则会出现入口仍可积压而底层先触发缓冲瓶颈的背压错配。
pub const ASR_CHANNEL_QUEUE_MAX_SECONDS: usize = 60;
pub const DEFAULT_ONLINE_ASR_URL: &str =
    "wss://aivoice-test.haier.net/api/speech-service/ws/asr/recognize";
pub const DEFAULT_TEST_APP_AUTH_URL: &str =
    "https://techless-test.haier.net/api/appauth/create/token";
pub const DEFAULT_PROD_APP_AUTH_URL: &str = "https://techless.haier.net/api/appauth/create/token";
pub const DEFAULT_ONLINE_VISIT_K_CODE: &str = "S04877";
pub const DEFAULT_ONLINE_AUDIO_FORMAT: &str = "pcm_s16le";
pub const DEFAULT_ONLINE_FRAME_BYTES: usize = 4096;
pub const MAX_ONLINE_FRAME_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub struct LocalAsrConfig {
    pub asr_model_dir: String,
    pub vad_model_dir: String,
    pub auto_download: bool,
    // ASR 主模型完整下载 URL：用于 SenseVoice 模型和 tokens 的资源预检。
    pub download_url: Option<String>,
    pub target_dir: Option<String>,
    // VAD 独立完整下载 URL：用于 VadDetector 自己的资源预检与自动下载。
    pub vad_download_url: Option<String>,
    pub vad_target_dir: Option<String>,
    pub sample_rate: i32,
    pub num_threads: i32,
    pub use_itn: bool,
    pub provider: String,
    pub min_silence_duration: f32,
    pub min_speech_duration: f32,
    pub vad_threshold: f32,
}

impl LocalAsrConfig {
    pub const ASR_MODEL_FILE_NAME: &'static str = "model.int8.onnx";
    pub const ASR_TOKENS_FILE_NAME: &'static str = "tokens.txt";
    pub const VAD_MODEL_FILE_NAME: &'static str = "silero_vad_v5.onnx";

    pub fn asr_model_dir_path(&self) -> PathBuf {
        PathBuf::from(&self.asr_model_dir)
    }

    pub fn vad_model_dir_path(&self) -> PathBuf {
        PathBuf::from(&self.vad_model_dir)
    }

    pub fn model_path(&self) -> PathBuf {
        self.asr_model_dir_path().join(Self::ASR_MODEL_FILE_NAME)
    }

    pub fn tokens_path(&self) -> PathBuf {
        self.asr_model_dir_path().join(Self::ASR_TOKENS_FILE_NAME)
    }

    pub fn vad_model_path(&self) -> PathBuf {
        self.vad_model_dir_path().join(Self::VAD_MODEL_FILE_NAME)
    }
}

#[derive(Clone, Debug)]
pub struct OnlineAsrConfig {
    pub online_url: String,
    pub app_auth_url: String,
    pub k_code: String,
    pub k_secret: String,
    pub visit_k_code: String,
    pub trace_id: String,
    pub biz_id: String,
    pub hot_words: Option<String>,
    pub sample_rate: i32,
    pub audio_format: String,
    pub frame_bytes: usize,
}

#[derive(Clone, Debug)]
pub enum HybridAsrConfig {
    Local(LocalAsrConfig),
    Online(OnlineAsrConfig),
}

impl HybridAsrConfig {
    pub fn mode(&self) -> AsrMode {
        match self {
            Self::Local(_) => AsrMode::Local,
            Self::Online(_) => AsrMode::Online,
        }
    }
}

pub fn infer_app_auth_url_from_online_url(online_url: &str) -> String {
    if online_url.contains("aivoice-test.haier.net") {
        DEFAULT_TEST_APP_AUTH_URL.to_string()
    } else {
        DEFAULT_PROD_APP_AUTH_URL.to_string()
    }
}

pub fn ensure_file_exists(path: &str, description: &str) -> Result<(), String> {
    if Path::new(path).exists() {
        Ok(())
    } else {
        Err(format!("{} 不存在: {}", description, path))
    }
}
