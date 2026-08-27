use model_installer::ModelDownloadObserver;
use std::sync::Arc;

use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig};
use vad_detector::{SpeechSegment, VadDetector, VadDetectorConfig};

use crate::assets::{create_model_download_observer, prepare_local_asr_assets_with_event_hub};
use crate::config::{ASR_CHANNEL_QUEUE_MAX_SECONDS, LocalAsrConfig, ensure_file_exists};
use crate::error::HybridAsrResult;
use crate::event::AsrEventHub;

pub struct AsrSegmentResult {
    pub text: String,
    pub start_time: f64,
    pub end_time: f64,
    pub start_sample: i64,
    pub sample_count: i64,
}

/// Local SenseVoice engine with VAD delegated to `vad-detector`.
///
/// `hybrid-asr` 仍然负责识别器构造和文本解码，
/// 而可复用的 VAD crate 继续负责端点检测与语音片段切分。
pub struct AsrEngine {
    recognizer: OfflineRecognizer,
    vad: VadDetector,
    sample_rate: i32,
}

impl AsrEngine {
    pub fn new(
        options: &LocalAsrConfig,
        event_hub: Option<Arc<AsrEventHub>>,
    ) -> HybridAsrResult<Self> {
        // 在构造识别器之前先做一次统一资源准备，
        // 这样直接使用 `hybrid-asr` 的调用方也能获得
        // “主 ASR + VAD 一起预检/下载/回填”的稳定语义。
        let observer: Option<ModelDownloadObserver> = event_hub
            .as_ref()
            .map(|event_hub| create_model_download_observer(Arc::clone(event_hub)));
        let prepared_assets =
            prepare_local_asr_assets_with_event_hub(options, observer, event_hub.as_deref())?;
        let options = prepared_assets.resolved_config;

        let model_path = options.model_path();
        let tokens_path = options.tokens_path();

        ensure_file_exists(model_path.to_string_lossy().as_ref(), "SenseVoice model")?;
        ensure_file_exists(tokens_path.to_string_lossy().as_ref(), "SenseVoice tokens")?;

        let mut recognizer_config = OfflineRecognizerConfig::default();
        recognizer_config.feat_config.sample_rate = options.sample_rate;
        recognizer_config.model_config.tokens = Some(tokens_path.to_string_lossy().into_owned());
        recognizer_config.model_config.num_threads = options.num_threads;
        recognizer_config.model_config.provider = Some(options.provider.clone());
        recognizer_config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(model_path.to_string_lossy().into_owned()),
            language: Some("auto".to_string()),
            use_itn: options.use_itn,
        };

        let recognizer = OfflineRecognizer::create(&recognizer_config)
            .ok_or_else(|| "SenseVoice offline recognizer creation failed".to_string())?;
        let vad = VadDetector::from_resolved_config(Self::build_vad_config(&options))
            .map_err(|error| -> crate::error::HybridAsrError { Box::new(error) })?;

        Ok(Self {
            recognizer,
            vad,
            sample_rate: options.sample_rate,
        })
    }

    /// Accepts one streaming audio frame and recognizes every VAD-completed
    /// speech segment produced by this frame.
    pub fn accept_samples(&mut self, samples: &[f32]) -> Vec<AsrSegmentResult> {
        self.vad
            .accept_and_drain(samples)
            .into_iter()
            .filter_map(|segment| self.recognize_segment(segment))
            .collect()
    }

    pub fn reset(&mut self) {
        self.vad.reset();
    }

    /// Flushes trailing speech from VAD before the session finishes.
    pub fn flush(&mut self) -> Vec<AsrSegmentResult> {
        self.vad
            .flush()
            .into_iter()
            .filter_map(|segment| self.recognize_segment(segment))
            .collect()
    }

    fn build_vad_config(options: &LocalAsrConfig) -> VadDetectorConfig {
        let vad_model_path = options.vad_model_path();
        VadDetectorConfig {
            model_path: vad_model_path.to_string_lossy().into_owned(),
            // 优先走 VAD 专属下载 URL，未提供时回退到 ASR 通用下载 URL，
            // 这样既满足新需求，也不会破坏现有调用方。
            download_url: options
                .vad_download_url
                .clone()
                .or_else(|| options.download_url.clone()),
            target_dir: options
                .vad_target_dir
                .clone()
                .or_else(|| options.target_dir.clone()),
            auto_download: options.auto_download,
            sample_rate: options.sample_rate,
            provider: options.provider.clone(),
            threshold: options.vad_threshold,
            min_silence_duration: options.min_silence_duration,
            min_speech_duration: options.min_speech_duration,
            window_size: 512,
            max_speech_duration: 0.0,
            // 顶层实时 ASR 入口允许积压 60 秒 PCM，本地 VAD 缓冲也必须使用同一时长，
            // 否则入口层与底层引擎会在不同阈值触发背压，导致容量语义不一致。
            max_buffer_seconds: ASR_CHANNEL_QUEUE_MAX_SECONDS as f32,
            num_threads: 1,
            speech_start_padding_duration: vad_detector::DEFAULT_SPEECH_START_PADDING_DURATION,
        }
    }

    fn recognize_segment(&self, segment: SpeechSegment) -> Option<AsrSegmentResult> {
        if segment.is_empty() {
            return None;
        }

        let stream = self.recognizer.create_stream();
        stream.accept_waveform(self.sample_rate, &segment.samples);
        self.recognizer.decode(&stream);
        let result = stream.get_result()?;
        let text = result.text.trim().to_string();
        if text.is_empty() {
            return None;
        }

        let sample_count = segment.sample_count();
        let start_time = segment.start_sample as f64 / self.sample_rate as f64;
        let end_time = start_time + sample_count as f64 / self.sample_rate as f64;
        Some(AsrSegmentResult {
            text,
            start_time,
            end_time,
            start_sample: segment.start_sample,
            sample_count,
        })
    }
}
