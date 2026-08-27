use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use model_installer::{
    ModelDownloadEvent, ModelDownloadObserver, ModelDownloadPhase, ModelInstallRequest,
    ModelResourceKind as InstallerResourceKind, install_model_package, resolve_target_dir_path,
};
use vad_detector::{
    VadDetectorConfig, prepare_vad_assets, will_prepare_vad_assets_download,
};

use crate::config::{ASR_CHANNEL_QUEUE_MAX_SECONDS, LocalAsrConfig};
use crate::error::HybridAsrResult;
use crate::event::{
    AsrEvent, AsrEventHub, ModelLifecycleEvent, ModelLifecyclePhase,
    ModelResourceKind as AsrModelResourceKind,
};

/// 本地 ASR 资源准备结果。
#[derive(Clone, Debug)]
pub struct PreparedLocalAsrAssets {
    pub resolved_config: LocalAsrConfig,
    pub downloaded_anything: bool,
    pub install_root: Option<PathBuf>,
}

/// 在真正创建本地 ASR 引擎前，先确保所需模型资产存在。
///
/// 这里的“本地 ASR 资源”包含两部分：
/// - SenseVoice 主模型 + tokens
/// - VAD 模型
pub fn prepare_local_asr_assets(
    config: &LocalAsrConfig,
    observer: Option<ModelDownloadObserver>,
) -> HybridAsrResult<PreparedLocalAsrAssets> {
    prepare_local_asr_assets_with_event_hub(config, observer, None)
}

/// 显式触发本地 ASR 资源下载，不受 `auto_download` 开关影响。
pub fn download_local_asr_assets(
    config: &LocalAsrConfig,
    observer: Option<ModelDownloadObserver>,
) -> HybridAsrResult<PreparedLocalAsrAssets> {
    download_local_asr_assets_with_event_hub(config, observer, None)
}

/// 预检当前本地 ASR 配置是否会触发下载。
///
/// 返回值语义固定为：
/// - `Ok(false)`：主模型与 VAD 都已可用，不需要下载。
/// - `Ok(true)`：至少一类资源缺失，但下载参数完整，可以安全启动后台下载。
/// - `Err(...)`：存在缺失资源，但下载参数不完整，不能伪装成“已开始后台下载”。
pub fn inspect_local_asr_assets_download(config: &LocalAsrConfig) -> HybridAsrResult<bool> {
    let asr_missing = !main_assets_exist(config) && try_resolve_existing_asr_assets(config)?.is_none();
    let vad_config = build_vad_config(config);
    let vad_missing = will_prepare_vad_assets_download(&vad_config)
        .map_err(|error| -> crate::error::HybridAsrError { Box::new(error) })?;

    if !asr_missing && !vad_missing {
        return Ok(false);
    }

    if asr_missing {
        require_download_field(config.download_url.clone(), "download_url")?;
    }

    if vad_missing {
        require_download_field(
            config
                .vad_download_url
                .clone()
                .or_else(|| config.download_url.clone()),
            "vad_download_url",
        )?;
    }

    Ok(true)
}

/// 在 session 装配路径里复用的内部预检入口。
///
/// 公开 API 仍然保持 `prepare_local_asr_assets(config, observer)` 不变；
/// 只有 `hybrid-asr` 自己在构造本地会话时，才额外把统一事件总线传进来，
/// 从而补齐稳定的 `Missing / Downloaded / Failed` 业务语义。
pub fn prepare_local_asr_assets_with_event_hub(
    config: &LocalAsrConfig,
    observer: Option<ModelDownloadObserver>,
    event_hub: Option<&AsrEventHub>,
) -> HybridAsrResult<PreparedLocalAsrAssets> {
    let should_prepare_asr =
        !main_assets_exist(config) && try_resolve_existing_asr_assets(config)?.is_none();
    let vad_config = build_vad_config(config);
    let should_prepare_vad = will_prepare_vad_assets_download(&vad_config)
        .map_err(|error| -> crate::error::HybridAsrError { Box::new(error) })?;

    if !should_prepare_asr && !should_prepare_vad {
        let resolved_config = if let Some(existing) = try_resolve_existing_asr_assets(config)? {
            existing.resolved_config
        } else {
            config.clone()
        };
        return Ok(PreparedLocalAsrAssets {
            resolved_config,
            downloaded_anything: false,
            install_root: None,
        });
    }

    if !config.auto_download {
        return Err("本地 ASR/VAD 模型缺失，且 auto_download=false，请先调用显式下载 API".into());
    }

    if should_prepare_asr {
        emit_missing_event(
            event_hub,
            AsrModelResourceKind::Asr,
            resolve_target_dir(config.target_dir.as_deref())?,
            format!(
                "本地 ASR 模型缺失，准备下载或安装，model_dir={}",
                config.asr_model_dir
            ),
        );
    }
    if should_prepare_vad {
        emit_missing_event(
            event_hub,
            AsrModelResourceKind::Vad,
            resolve_target_dir(vad_config.target_dir.as_deref())?,
            format!(
                "本地 VAD 模型缺失，准备下载或安装，model_dir={}",
                config.vad_model_dir
            ),
        );
    }

    let failed_asr_emitted = Arc::new(AtomicBool::new(false));
    let failed_vad_emitted = Arc::new(AtomicBool::new(false));
    let tracked_observer = observer.map(|observer| {
        create_tracking_download_observer(observer, Arc::clone(&failed_asr_emitted), Arc::clone(&failed_vad_emitted))
    });

    let prepared_asr = match prepare_main_asr_assets(config, tracked_observer.clone()) {
        Ok(prepared) => prepared,
        Err(error) => {
            if should_prepare_asr && !failed_asr_emitted.load(Ordering::SeqCst) {
                emit_failed_event(
                    event_hub,
                    AsrModelResourceKind::Asr,
                    resolve_target_dir(config.target_dir.as_deref())?,
                    error.to_string(),
                );
            }
            return Err(error);
        }
    };

    let prepared_vad = match prepare_vad_assets(&vad_config, tracked_observer) {
        Ok(prepared) => prepared,
        Err(error) => {
            if should_prepare_vad && !failed_vad_emitted.load(Ordering::SeqCst) {
                emit_failed_event(
                    event_hub,
                    AsrModelResourceKind::Vad,
                    resolve_target_dir(vad_config.target_dir.as_deref())?,
                    error.to_string(),
                );
            }
            return Err(Box::new(error));
        }
    };

    let mut resolved_config = if should_prepare_asr {
        prepared_asr.resolved_config
    } else if let Some(existing) = try_resolve_existing_asr_assets(config)? {
        existing.resolved_config
    } else {
        config.clone()
    };
    resolved_config.vad_model_dir = Path::new(&prepared_vad.resolved_config.model_path)
        .parent()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| config.vad_model_dir.clone());

    if should_prepare_asr {
        emit_downloaded_event(
            event_hub,
            AsrModelResourceKind::Asr,
            prepared_asr
                .install_root
                .clone()
                .unwrap_or_else(|| PathBuf::from(&resolved_config.asr_model_dir)),
            "本地 SenseVoice 模型已经准备完成".to_string(),
        );
    }
    if should_prepare_vad {
        emit_downloaded_event(
            event_hub,
            AsrModelResourceKind::Vad,
            prepared_vad
                .install_root
                .clone()
                .unwrap_or_else(|| PathBuf::from(&resolved_config.vad_model_dir)),
            "本地 VAD 模型已经准备完成".to_string(),
        );
    }

    Ok(PreparedLocalAsrAssets {
        resolved_config,
        downloaded_anything: prepared_asr.downloaded_anything || prepared_vad.downloaded_anything,
        // 按既有语义继续返回主 ASR 安装根。
        install_root: prepared_asr.install_root,
    })
}

/// 显式触发本地 ASR 资源下载并桥接事件，不受 `auto_download` 开关影响。
pub fn download_local_asr_assets_with_event_hub(
    config: &LocalAsrConfig,
    observer: Option<ModelDownloadObserver>,
    event_hub: Option<&AsrEventHub>,
) -> HybridAsrResult<PreparedLocalAsrAssets> {
    let mut download_config = config.clone();
    download_config.auto_download = true;
    let should_prepare_asr =
        !main_assets_exist(&download_config) && try_resolve_existing_asr_assets(&download_config)?.is_none();
    let should_prepare_vad = will_prepare_vad_assets_download(&build_vad_config(&download_config))
        .map_err(|error| -> crate::error::HybridAsrError { Box::new(error) })?;
    let prepared = prepare_local_asr_assets_with_event_hub(&download_config, observer, event_hub)?;
    if !should_prepare_asr {
        emit_downloaded_event(
            event_hub,
            AsrModelResourceKind::Asr,
            PathBuf::from(&prepared.resolved_config.asr_model_dir),
            "本地 SenseVoice 模型已经准备完成".to_string(),
        );
    }
    if !should_prepare_vad {
        emit_downloaded_event(
            event_hub,
            AsrModelResourceKind::Vad,
            PathBuf::from(&prepared.resolved_config.vad_model_dir),
            "本地 VAD 模型已经准备完成".to_string(),
        );
    }
    Ok(prepared)
}

/// 为 `hybrid-asr` 会话创建模型事件桥接观察者。
pub fn create_model_download_observer(event_hub: Arc<AsrEventHub>) -> ModelDownloadObserver {
    Arc::new(move |event: ModelDownloadEvent| {
        if let Some(model_event) = map_download_event(&event) {
            event_hub.emit(AsrEvent::Model(model_event));
        }
    })
}

pub(crate) fn emit_model_lifecycle_event(
    event_hub: &AsrEventHub,
    resource_kind: AsrModelResourceKind,
    phase: ModelLifecyclePhase,
    target_dir: PathBuf,
    message: impl Into<String>,
) {
    event_hub.emit(AsrEvent::Model(ModelLifecycleEvent {
        resource_kind,
        phase,
        downloaded_bytes: None,
        total_bytes: None,
        target_dir,
        message: message.into(),
        timestamp: SystemTime::now(),
    }));
}

fn prepare_main_asr_assets(
    config: &LocalAsrConfig,
    observer: Option<ModelDownloadObserver>,
) -> HybridAsrResult<PreparedLocalAsrAssets> {
    if main_assets_exist(config) {
        return Ok(PreparedLocalAsrAssets {
            resolved_config: config.clone(),
            downloaded_anything: false,
            install_root: None,
        });
    }

    if let Some(existing) = try_resolve_existing_asr_assets(config)? {
        return Ok(existing);
    }

    if !config.auto_download {
        return Err("本地 ASR 模型缺失，且 auto_download=false，请先调用显式下载 API".into());
    }

    let download_url = require_download_field(config.download_url.clone(), "download_url")?;

    let install_result = install_model_package(
        &ModelInstallRequest {
            resource_kind: InstallerResourceKind::Asr,
            download_url,
            target_dir: config.target_dir.clone(),
            package_key: "hybrid-asr".to_string(),
        },
        observer,
    )
    .map_err(|error| -> crate::error::HybridAsrError { Box::new(error) })?;

    let mut resolved_config = config.clone();
    resolved_config.asr_model_dir =
        resolve_installed_asr_model_dir(&install_result.install_root, &resolved_config)?;

    Ok(PreparedLocalAsrAssets {
        resolved_config,
        downloaded_anything: install_result.downloaded_anything,
        install_root: Some(install_result.install_root),
    })
}

fn create_tracking_download_observer(
    observer: ModelDownloadObserver,
    failed_asr_emitted: Arc<AtomicBool>,
    failed_vad_emitted: Arc<AtomicBool>,
) -> ModelDownloadObserver {
    Arc::new(move |event: ModelDownloadEvent| {
        if event.phase == ModelDownloadPhase::Failed {
            match event.resource_kind {
                InstallerResourceKind::Asr | InstallerResourceKind::Tts => {
                    failed_asr_emitted.store(true, Ordering::SeqCst);
                }
                InstallerResourceKind::Vad => {
                    failed_vad_emitted.store(true, Ordering::SeqCst);
                }
            }
        }
        observer(event);
    })
}

fn map_download_event(event: &ModelDownloadEvent) -> Option<ModelLifecycleEvent> {
    let phase = match event.phase {
        ModelDownloadPhase::BundleDownloading | ModelDownloadPhase::PackagePreparing => {
            ModelLifecyclePhase::Downloading
        }
        ModelDownloadPhase::Completed => return None,
        ModelDownloadPhase::Failed => ModelLifecyclePhase::Failed,
        ModelDownloadPhase::Validating | ModelDownloadPhase::Extracting => return None,
    };

    Some(ModelLifecycleEvent {
        resource_kind: map_resource_kind(event.resource_kind),
        phase,
        downloaded_bytes: event.downloaded_bytes,
        total_bytes: event.total_bytes,
        target_dir: event.target_dir.clone(),
        message: event.message.clone(),
        timestamp: SystemTime::now(),
    })
}

fn emit_missing_event(
    event_hub: Option<&AsrEventHub>,
    resource_kind: AsrModelResourceKind,
    target_dir: PathBuf,
    message: String,
) {
    if let Some(event_hub) = event_hub {
        emit_model_lifecycle_event(
            event_hub,
            resource_kind,
            ModelLifecyclePhase::Missing,
            target_dir,
            message,
        );
    }
}

fn emit_downloaded_event(
    event_hub: Option<&AsrEventHub>,
    resource_kind: AsrModelResourceKind,
    target_dir: PathBuf,
    message: String,
) {
    if let Some(event_hub) = event_hub {
        emit_model_lifecycle_event(
            event_hub,
            resource_kind,
            ModelLifecyclePhase::Downloaded,
            target_dir,
            message,
        );
    }
}

fn emit_failed_event(
    event_hub: Option<&AsrEventHub>,
    resource_kind: AsrModelResourceKind,
    target_dir: PathBuf,
    message: String,
) {
    if let Some(event_hub) = event_hub {
        emit_model_lifecycle_event(
            event_hub,
            resource_kind,
            ModelLifecyclePhase::Failed,
            target_dir,
            message,
        );
    }
}

fn map_resource_kind(resource_kind: InstallerResourceKind) -> AsrModelResourceKind {
    match resource_kind {
        InstallerResourceKind::Asr => AsrModelResourceKind::Asr,
        InstallerResourceKind::Tts => AsrModelResourceKind::Asr,
        InstallerResourceKind::Vad => AsrModelResourceKind::Vad,
    }
}

fn main_assets_exist(config: &LocalAsrConfig) -> bool {
    has_required_asr_files(&config.asr_model_dir_path())
}

fn try_resolve_existing_asr_assets(
    config: &LocalAsrConfig,
) -> HybridAsrResult<Option<PreparedLocalAsrAssets>> {
    let target_dir = resolve_target_dir(config.target_dir.as_deref())?;
    if !target_dir.exists() {
        return Ok(None);
    }

    let mut resolved_config = config.clone();
    let Some(asr_model_dir) = try_resolve_existing_asr_model_dir(&target_dir, config)? else {
        return Ok(None);
    };

    resolved_config.asr_model_dir = asr_model_dir;
    Ok(Some(PreparedLocalAsrAssets {
        resolved_config,
        downloaded_anything: false,
        install_root: Some(target_dir),
    }))
}

fn build_vad_config(options: &LocalAsrConfig) -> VadDetectorConfig {
    VadDetectorConfig {
        model_path: options.vad_model_path().to_string_lossy().into_owned(),
        // 先走 VAD 专属下载 URL，未提供时回退到 ASR 通用字段，
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
        // 资源预检路径构造的默认 VAD 配置也要与运行时 60 秒入口预算保持一致，
        // 避免默认配置与实际本地会话使用的缓冲时长不一致。
        max_buffer_seconds: ASR_CHANNEL_QUEUE_MAX_SECONDS as f32,
        num_threads: 1,
        speech_start_padding_duration: vad_detector::DEFAULT_SPEECH_START_PADDING_DURATION,
    }
}

fn resolve_target_dir(raw: Option<&str>) -> HybridAsrResult<PathBuf> {
    resolve_target_dir_path(raw).map_err(|error| -> crate::error::HybridAsrError { Box::new(error) })
}

fn require_download_field(value: Option<String>, field: &str) -> HybridAsrResult<String> {
    let Some(value) = value.map(|value| value.trim().to_string()) else {
        return Err(format!("本地 ASR 模型缺失，且 {field} 未配置").into());
    };
    if value.is_empty() {
        return Err(format!("本地 ASR 模型缺失，且 {field} 为空").into());
    }
    Ok(value)
}

fn has_required_asr_files(path: &Path) -> bool {
    path.join(LocalAsrConfig::ASR_MODEL_FILE_NAME).exists()
        && path.join(LocalAsrConfig::ASR_TOKENS_FILE_NAME).exists()
}

fn try_resolve_existing_asr_model_dir(
    install_root: &Path,
    config: &LocalAsrConfig,
) -> HybridAsrResult<Option<String>> {
    let configured_dir = config.asr_model_dir_path();
    if has_required_asr_files(&configured_dir) {
        return Ok(Some(configured_dir.to_string_lossy().into_owned()));
    }

    let candidate = install_root.join(resolve_dir_name(
        &config.asr_model_dir,
        "sense-voice-zh-en",
    )?);
    if has_required_asr_files(&candidate) {
        return Ok(Some(candidate.to_string_lossy().into_owned()));
    }

    if has_required_asr_files(install_root) {
        return Ok(Some(install_root.to_string_lossy().into_owned()));
    }

    Ok(None)
}

fn resolve_installed_asr_model_dir(
    install_root: &Path,
    config: &LocalAsrConfig,
) -> HybridAsrResult<String> {
    try_resolve_existing_asr_model_dir(install_root, config)?.ok_or_else(|| {
        format!(
            "安装目录中缺少 {} 或 {}",
            LocalAsrConfig::ASR_MODEL_FILE_NAME,
            LocalAsrConfig::ASR_TOKENS_FILE_NAME
        )
        .into()
    })
}

fn resolve_dir_name(path: &str, fallback: &str) -> HybridAsrResult<String> {
    Ok(Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(fallback)
        .to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use model_installer::{
        ModelDownloadEvent, ModelDownloadPhase, ModelResourceKind as InstallerKind,
    };

    use super::*;
    use crate::event::ModelResourceKind;

    fn build_missing_config(target_dir: &Path) -> LocalAsrConfig {
        LocalAsrConfig {
            asr_model_dir: "./missing/sense-voice-zh-en".to_string(),
            vad_model_dir: "./missing/silero_vad".to_string(),
            auto_download: false,
            download_url: None,
            vad_download_url: None,
            vad_target_dir: None,
            target_dir: Some(target_dir.to_string_lossy().into_owned()),
            sample_rate: 16_000,
            num_threads: 2,
            use_itn: true,
            provider: "cpu".to_string(),
            min_silence_duration: 0.5,
            min_speech_duration: 0.25,
            vad_threshold: 0.5,
        }
    }

    #[test]
    fn prepare_assets_emits_missing_before_download_validation() {
        let target_dir =
            std::env::temp_dir().join(format!("hybrid-asr-missing-event-{}", std::process::id()));
        let event_hub = AsrEventHub::default();
        let events = event_hub.subscribe();

        let error = prepare_local_asr_assets_with_event_hub(
            &build_missing_config(&target_dir),
            None,
            Some(&event_hub),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("download_url"));
        let emitted = events
            .try_iter()
            .find_map(|event| match event {
                AsrEvent::Model(event) => Some(event),
                _ => None,
            })
            .expect("missing model event");
        assert_eq!(emitted.resource_kind, ModelResourceKind::Asr);
        assert_eq!(emitted.phase, ModelLifecyclePhase::Missing);
        assert_eq!(emitted.target_dir, target_dir);
    }

    #[test]
    fn model_download_observer_only_maps_stable_business_phases() {
        let event_hub = Arc::new(AsrEventHub::default());
        let events = event_hub.subscribe();
        let observer = create_model_download_observer(Arc::clone(&event_hub));

        observer(ModelDownloadEvent {
            resource_kind: InstallerKind::Asr,
            phase: ModelDownloadPhase::PackagePreparing,
            downloaded_bytes: Some(128),
            total_bytes: Some(1024),
            target_dir: PathBuf::from("./models/asr"),
            message: "正在下载 ASR 模型".to_string(),
        });
        observer(ModelDownloadEvent {
            resource_kind: InstallerKind::Vad,
            phase: ModelDownloadPhase::Failed,
            downloaded_bytes: Some(256),
            total_bytes: Some(1024),
            target_dir: PathBuf::from("./models/vad"),
            message: "VAD 下载失败".to_string(),
        });
        observer(ModelDownloadEvent {
            resource_kind: InstallerKind::Asr,
            phase: ModelDownloadPhase::Completed,
            downloaded_bytes: None,
            total_bytes: None,
            target_dir: PathBuf::from("./models/asr"),
            message: "完成".to_string(),
        });

        let emitted = events
            .try_iter()
            .filter_map(|event| match event {
                AsrEvent::Model(event) => Some(event),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(emitted.len(), 2);
        assert_eq!(emitted[0].resource_kind, ModelResourceKind::Asr);
        assert_eq!(emitted[0].phase, ModelLifecyclePhase::Downloading);
        assert_eq!(emitted[1].resource_kind, ModelResourceKind::Vad);
        assert_eq!(emitted[1].phase, ModelLifecyclePhase::Failed);
    }

    #[test]
    fn inspect_download_returns_false_when_asr_and_vad_are_reusable() {
        let target_dir =
            std::env::temp_dir().join(format!("hybrid-asr-reusable-{}", std::process::id()));
        let _ = fs::remove_dir_all(&target_dir);
        fs::create_dir_all(target_dir.join("sense-voice-zh-en")).unwrap();
        fs::create_dir_all(target_dir.join("silero_vad")).unwrap();
        fs::write(
            target_dir.join("sense-voice-zh-en/model.int8.onnx"),
            b"fake-model",
        )
        .unwrap();
        fs::write(target_dir.join("sense-voice-zh-en/tokens.txt"), b"fake-tokens").unwrap();
        fs::write(
            target_dir.join("silero_vad/silero_vad_v5.onnx"),
            b"fake-vad",
        )
        .unwrap();

        let config = LocalAsrConfig {
            asr_model_dir: "./missing/sense-voice-zh-en".to_string(),
            vad_model_dir: "./silero_vad".to_string(),
            auto_download: false,
            download_url: None,
            vad_download_url: None,
            vad_target_dir: Some(target_dir.to_string_lossy().into_owned()),
            target_dir: Some(target_dir.to_string_lossy().into_owned()),
            sample_rate: 16_000,
            num_threads: 2,
            use_itn: true,
            provider: "cpu".to_string(),
            min_silence_duration: 0.5,
            min_speech_duration: 0.25,
            vad_threshold: 0.5,
        };

        let should_download = inspect_local_asr_assets_download(&config).unwrap();

        assert!(!should_download);
        let _ = fs::remove_dir_all(&target_dir);
    }

    #[test]
    fn prepare_assets_fails_fast_when_auto_download_disabled() {
        let target_dir = std::env::temp_dir()
            .join(format!("hybrid-asr-auto-download-disabled-{}", std::process::id()));
        let error = prepare_local_asr_assets_with_event_hub(
            &build_missing_config(&target_dir),
            None,
            None,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("auto_download=false"));
    }

    #[test]
    fn explicit_download_api_ignores_auto_download_flag_during_validation() {
        let target_dir = std::env::temp_dir()
            .join(format!("hybrid-asr-explicit-download-{}", std::process::id()));
        let error = download_local_asr_assets(&build_missing_config(&target_dir), None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("download_url"));
    }
}
