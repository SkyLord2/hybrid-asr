use hybrid_asr::{AsrState, HybridAsrConfig, HybridAsrSession, OnlineAsrConfig};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 在线示例保留了完整鉴权字段，方便外部集成者直接映射自己的配置中心。
    let config = HybridAsrConfig::Online(OnlineAsrConfig {
        online_url: "wss://aivoice-test.haier.net/api/speech-service/ws/asr/recognize".to_string(),
        app_auth_url: "https://techless-test.haier.net/api/appauth/create/token".to_string(),
        k_code: "YOUR_K_CODE".to_string(),
        k_secret: "YOUR_K_SECRET".to_string(),
        visit_k_code: "S04877".to_string(),
        trace_id: "demo-trace-id".to_string(),
        biz_id: "demo-biz-id".to_string(),
        hot_words: None,
        sample_rate: 16_000,
        audio_format: "pcm_s16le".to_string(),
        frame_bytes: 4096,
    });

    if std::env::var_os("RUN_HYBRID_ASR_ONLINE_EXAMPLE").is_some() {
        let mut session = HybridAsrSession::new(config)?;
        session.start()?;
        assert_eq!(session.state(), AsrState::Ready);

        let results = session.accept_samples(&vec![0.0; 3200])?;
        println!("sentence result count: {}", results.len());

        let final_results = session.finish()?;
        println!("final sentence count: {}", final_results.len());
    } else {
        println!("prepared online config");
    }

    Ok(())
}
