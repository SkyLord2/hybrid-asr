use hybrid_asr::{AsrState, HybridAsrConfig, HybridAsrSession, LocalAsrConfig};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 示例只演示 API 连接方式；真实运行前请替换为有效的模型路径。
    let config = HybridAsrConfig::Local(LocalAsrConfig {
        asr_model_dir: "models/sense-voice-zh-en".to_string(),
        vad_model_dir: "models/silero_vad".to_string(),
        auto_download: false,
        // ASR 主模型完整下载 URL。
        download_url: None,
        target_dir: None,
        // VAD 独立完整下载 URL。
        vad_download_url: None,
        vad_target_dir: None,
        sample_rate: 16_000,
        num_threads: 4,
        use_itn: true,
        provider: "cpu".to_string(),
        min_silence_duration: 0.5,
        min_speech_duration: 0.25,
        vad_threshold: 0.5,
    });

    if std::env::var_os("RUN_HYBRID_ASR_LOCAL_EXAMPLE").is_some() {
        let mut session = HybridAsrSession::new(config)?;
        session.start()?;
        assert_eq!(session.state(), AsrState::Ready);

        let partial = session.accept_samples(&vec![0.0; 1600])?;
        println!("partial result count: {}", partial.len());

        let final_results = session.finish()?;
        println!("final result count: {}", final_results.len());
    } else {
        println!("prepared local config");
    }

    Ok(())
}
