use hybrid_asr::{AsrResult, AsrState, HybridAsrResult, HybridAsrSession, RealtimeAsrBackend};

#[derive(Default)]
struct TestBackend {
    finished: bool,
    prepare_next_turn_calls: usize,
}

impl RealtimeAsrBackend for TestBackend {
    fn start(&mut self) -> HybridAsrResult<()> {
        Ok(())
    }

    fn accept_samples(&mut self, samples: &[f32]) -> HybridAsrResult<Vec<AsrResult>> {
        Ok(vec![AsrResult {
            text: format!("samples={}", samples.len()),
            start_time: 0.0,
            end_time: 0.5,
            start_sample: 0,
            sample_count: samples.len() as i64,
            is_final: false,
        }])
    }

    fn reset(&mut self) -> HybridAsrResult<()> {
        Ok(())
    }

    fn prepare_next_turn(&mut self) -> HybridAsrResult<()> {
        self.prepare_next_turn_calls += 1;
        Ok(())
    }

    fn finish(&mut self) -> HybridAsrResult<Vec<AsrResult>> {
        self.finished = true;
        Ok(vec![AsrResult {
            text: "final".to_string(),
            start_time: 0.0,
            end_time: 1.0,
            start_sample: 0,
            sample_count: 16_000,
            is_final: true,
        }])
    }
}

#[test]
fn session_supports_backend_injection_for_external_integrators() {
    let mut session = HybridAsrSession::new_with_backend(Box::new(TestBackend::default()));
    session.start().unwrap();
    let partial = session.accept_samples(&[0.1, 0.2, 0.3]).unwrap();
    assert_eq!(session.state(), AsrState::Recognizing);
    assert_eq!(partial[0].text, "samples=3");

    let final_results = session.finish().unwrap();
    assert_eq!(session.state(), AsrState::Finished);
    assert_eq!(final_results[0].text, "final");
    assert!(final_results[0].is_final);
}

#[test]
fn session_accepts_plain_sample_slices_without_recorder_types() {
    let mut session = HybridAsrSession::new_with_backend(Box::new(TestBackend::default()));
    session.start().unwrap();

    let samples = vec![0.0_f32; 320];
    let results = session.accept_samples(&samples).unwrap();

    assert_eq!(session.state(), AsrState::Recognizing);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].text, "samples=320");
}

#[test]
fn session_can_finish_one_turn_and_prepare_next_turn() {
    let mut session = HybridAsrSession::new_with_backend(Box::new(TestBackend::default()));
    session.start().unwrap();
    session.accept_samples(&[0.1, 0.2, 0.3]).unwrap();

    let final_results = session.finish_turn().unwrap();
    assert_eq!(session.state(), AsrState::Finished);
    assert_eq!(final_results[0].text, "final");

    session.prepare_next_turn().unwrap();
    assert_eq!(session.state(), AsrState::Ready);

    let next_results = session.accept_samples(&[0.5, 0.6]).unwrap();
    assert_eq!(session.state(), AsrState::Recognizing);
    assert_eq!(next_results[0].text, "samples=2");
}
