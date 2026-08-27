use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use crate::backend::{RealtimeAsrBackend, create_realtime_asr_backend};
use crate::config::HybridAsrConfig;
use crate::error::HybridAsrResult;
use crate::event::{
    AsrEvent, AsrEventHub, AsrStateChangeReason, AsrStateTransitionEvent, SessionEvent,
};
use crate::types::{AsrResult, AsrState};

/// Session wrapper that owns one reusable realtime ASR backend.
///
/// The session stays stable across multiple user turns in the smart-call
/// module. `prepare_next_turn()` is intentionally distinct from `finish()`: the
/// former keeps the backend hot, while the latter ends the whole ASR session.
pub struct HybridAsrSession {
    backend: Box<dyn RealtimeAsrBackend>,
    state: AsrState,
    event_hub: Arc<AsrEventHub>,
}

impl HybridAsrSession {
    pub fn new(config: HybridAsrConfig) -> HybridAsrResult<Self> {
        let event_hub = Arc::new(AsrEventHub::default());
        Self::new_with_event_hub(config, event_hub)
    }

    /// 使用外部事件总线创建会话。
    ///
    /// 这样调用方就可以先订阅事件，再触发 backend 构造过程，
    /// 从而稳定接收本地模型在构造早期发出的 missing/downloading 事件。
    pub fn new_with_event_hub(
        config: HybridAsrConfig,
        event_hub: Arc<AsrEventHub>,
    ) -> HybridAsrResult<Self> {
        let backend = match create_realtime_asr_backend(config, Some(Arc::clone(&event_hub))) {
            Ok(backend) => backend,
            Err(error) => {
                // 构造失败时也要显式关闭事件流，避免外部桥接线程继续阻塞在 recv()。
                event_hub.close();
                return Err(error);
            }
        };

        let session = Self {
            backend,
            state: AsrState::Idle,
            event_hub,
        };
        session.publish_session_event("ASR 会话已创建");
        Ok(session)
    }

    pub fn new_with_backend(backend: Box<dyn RealtimeAsrBackend>) -> Self {
        let session = Self {
            backend,
            state: AsrState::Idle,
            event_hub: Arc::new(AsrEventHub::default()),
        };
        session.publish_session_event("ASR 会话已创建");
        session
    }

    /// 订阅统一 ASR 事件流。
    pub fn subscribe_events(&self) -> Receiver<AsrEvent> {
        self.event_hub.subscribe()
    }

    pub fn start(&mut self) -> HybridAsrResult<()> {
        self.transition_state(AsrState::Initializing, AsrStateChangeReason::StartRequested);
        match self.backend.start() {
            Ok(()) => {
                self.transition_state(AsrState::Ready, AsrStateChangeReason::StartSucceeded);
                Ok(())
            }
            Err(err) => Err(self.handle_error(err)),
        }
    }

    pub fn accept_samples(&mut self, samples: &[f32]) -> HybridAsrResult<Vec<AsrResult>> {
        if self.state == AsrState::Paused {
            return Ok(Vec::new());
        }
        self.transition_state(
            AsrState::Recognizing,
            AsrStateChangeReason::RecognitionRequested,
        );
        match self.backend.accept_samples(samples) {
            Ok(results) => Ok(results),
            Err(err) => Err(self.handle_error(err)),
        }
    }

    pub fn pause(&mut self) -> HybridAsrResult<()> {
        match self.backend.reset() {
            Ok(()) => {
                self.transition_state(AsrState::Paused, AsrStateChangeReason::PauseRequested);
                Ok(())
            }
            Err(err) => Err(self.handle_error(err)),
        }
    }

    pub fn resume(&mut self) {
        self.transition_state(AsrState::Recognizing, AsrStateChangeReason::ResumeRequested);
    }

    pub fn finish(&mut self) -> HybridAsrResult<Vec<AsrResult>> {
        self.transition_state(AsrState::Finishing, AsrStateChangeReason::FinishRequested);
        match self.backend.finish() {
            Ok(results) => {
                self.transition_state(AsrState::Finished, AsrStateChangeReason::FinishRequested);
                Ok(results)
            }
            Err(err) => Err(self.handle_error(err)),
        }
    }

    /// Finishes one turn and keeps the backend ready for the next one.
    pub fn finish_turn(&mut self) -> HybridAsrResult<Vec<AsrResult>> {
        self.finish()
    }

    /// Clears per-turn buffers while retaining the initialized backend.
    pub fn prepare_next_turn(&mut self) -> HybridAsrResult<()> {
        match self.backend.prepare_next_turn() {
            Ok(()) => {
                self.transition_state(
                    AsrState::Ready,
                    AsrStateChangeReason::PrepareNextTurnRequested,
                );
                Ok(())
            }
            Err(err) => Err(self.handle_error(err)),
        }
    }

    pub fn reset(&mut self) -> HybridAsrResult<()> {
        match self.backend.reset() {
            Ok(()) => {
                self.transition_state(AsrState::Ready, AsrStateChangeReason::ResetRequested);
                Ok(())
            }
            Err(err) => Err(self.handle_error(err)),
        }
    }

    pub fn state(&self) -> AsrState {
        self.state
    }

    fn transition_state(&mut self, to: AsrState, reason: AsrStateChangeReason) {
        let from = self.state;
        self.state = to;
        self.event_hub
            .emit(AsrEvent::StateChanged(AsrStateTransitionEvent {
                from,
                to,
                reason,
                timestamp: SystemTime::now(),
            }));
    }

    fn publish_session_event(&self, message: impl Into<String>) {
        self.event_hub.emit(AsrEvent::Session(SessionEvent {
            message: message.into(),
            timestamp: SystemTime::now(),
        }));
    }

    fn handle_error(&mut self, err: crate::error::HybridAsrError) -> crate::error::HybridAsrError {
        let message = err.to_string();
        self.transition_state(AsrState::Error, AsrStateChangeReason::ErrorOccurred);
        self.event_hub.emit(AsrEvent::Error(message));
        err
    }
}

impl Drop for HybridAsrSession {
    fn drop(&mut self) {
        // Session 是事件流拥有者之一，析构时必须主动收口事件流。
        self.event_hub.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocalAsrConfig;
    use crate::event::{ModelLifecyclePhase, ModelResourceKind};
    use std::sync::mpsc::RecvTimeoutError;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Default)]
    struct BackendProbe {
        start_calls: usize,
        accept_calls: usize,
        reset_calls: usize,
        prepare_next_turn_calls: usize,
        finish_calls: usize,
        fail_start: bool,
        fail_accept: bool,
        fail_reset: bool,
        fail_prepare_next_turn: bool,
        fail_finish: bool,
    }

    struct FakeBackend {
        probe: Arc<Mutex<BackendProbe>>,
        results: Vec<AsrResult>,
    }

    impl FakeBackend {
        fn new(probe: Arc<Mutex<BackendProbe>>) -> Self {
            Self {
                probe,
                results: vec![AsrResult {
                    text: "hello".to_string(),
                    start_time: 0.0,
                    end_time: 1.0,
                    start_sample: 0,
                    sample_count: 16_000,
                    is_final: true,
                }],
            }
        }
    }

    impl RealtimeAsrBackend for FakeBackend {
        fn start(&mut self) -> HybridAsrResult<()> {
            let mut probe = self.probe.lock().unwrap();
            probe.start_calls += 1;
            if probe.fail_start {
                Err("start failed".into())
            } else {
                Ok(())
            }
        }

        fn accept_samples(&mut self, _samples: &[f32]) -> HybridAsrResult<Vec<AsrResult>> {
            let mut probe = self.probe.lock().unwrap();
            probe.accept_calls += 1;
            if probe.fail_accept {
                Err("accept failed".into())
            } else {
                Ok(self.results.clone())
            }
        }

        fn reset(&mut self) -> HybridAsrResult<()> {
            let mut probe = self.probe.lock().unwrap();
            probe.reset_calls += 1;
            if probe.fail_reset {
                Err("reset failed".into())
            } else {
                Ok(())
            }
        }

        fn prepare_next_turn(&mut self) -> HybridAsrResult<()> {
            let mut probe = self.probe.lock().unwrap();
            probe.prepare_next_turn_calls += 1;
            if probe.fail_prepare_next_turn {
                Err("prepare next turn failed".into())
            } else {
                Ok(())
            }
        }

        fn finish(&mut self) -> HybridAsrResult<Vec<AsrResult>> {
            let mut probe = self.probe.lock().unwrap();
            probe.finish_calls += 1;
            if probe.fail_finish {
                Err("finish failed".into())
            } else {
                Ok(self.results.clone())
            }
        }
    }

    fn new_session(probe: Arc<Mutex<BackendProbe>>) -> HybridAsrSession {
        HybridAsrSession::new_with_backend(Box::new(FakeBackend::new(probe)))
    }

    fn assert_receiver_disconnected<T>(result: Result<T, RecvTimeoutError>) {
        assert!(matches!(result, Err(RecvTimeoutError::Disconnected)));
    }

    #[test]
    fn starts_from_idle_state() {
        let probe = Arc::new(Mutex::new(BackendProbe::default()));
        let session = new_session(probe);
        assert_eq!(session.state(), AsrState::Idle);
    }

    #[test]
    fn start_transitions_to_ready() {
        let probe = Arc::new(Mutex::new(BackendProbe::default()));
        let mut session = new_session(probe.clone());
        session.start().unwrap();
        assert_eq!(session.state(), AsrState::Ready);
        assert_eq!(probe.lock().unwrap().start_calls, 1);
    }

    #[test]
    fn subscribe_events_reports_state_changes() {
        let probe = Arc::new(Mutex::new(BackendProbe::default()));
        let mut session = new_session(probe);
        let events = session.subscribe_events();

        session.start().unwrap();
        session.accept_samples(&[0.1, 0.2]).unwrap();
        session.finish().unwrap();

        let state_changes = events
            .try_iter()
            .filter_map(|event| match event {
                AsrEvent::StateChanged(event) => Some((event.from, event.to)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(state_changes.contains(&(AsrState::Idle, AsrState::Initializing)));
        assert!(state_changes.contains(&(AsrState::Initializing, AsrState::Ready)));
        assert!(state_changes.contains(&(AsrState::Ready, AsrState::Recognizing)));
        assert!(state_changes.contains(&(AsrState::Finishing, AsrState::Finished)));
    }

    #[test]
    fn accept_transitions_to_recognizing() {
        let probe = Arc::new(Mutex::new(BackendProbe::default()));
        let mut session = new_session(probe.clone());
        session.start().unwrap();
        let results = session.accept_samples(&[0.1, 0.2]).unwrap();
        assert_eq!(session.state(), AsrState::Recognizing);
        assert_eq!(results.len(), 1);
        assert_eq!(probe.lock().unwrap().accept_calls, 1);
    }

    #[test]
    fn pause_and_resume_update_state() {
        let probe = Arc::new(Mutex::new(BackendProbe::default()));
        let mut session = new_session(probe.clone());
        session.start().unwrap();
        session.pause().unwrap();
        assert_eq!(session.state(), AsrState::Paused);
        session.resume();
        assert_eq!(session.state(), AsrState::Recognizing);
        assert_eq!(probe.lock().unwrap().reset_calls, 1);
    }

    #[test]
    fn finish_transitions_to_finished() {
        let probe = Arc::new(Mutex::new(BackendProbe::default()));
        let mut session = new_session(probe.clone());
        session.start().unwrap();
        let results = session.finish().unwrap();
        assert_eq!(session.state(), AsrState::Finished);
        assert_eq!(results.len(), 1);
        assert_eq!(probe.lock().unwrap().finish_calls, 1);
    }

    #[test]
    fn prepare_next_turn_returns_to_ready_without_recreating_backend() {
        let probe = Arc::new(Mutex::new(BackendProbe::default()));
        let mut session = new_session(probe.clone());
        session.start().unwrap();
        session.accept_samples(&[0.1, 0.2]).unwrap();
        session.finish_turn().unwrap();
        session.prepare_next_turn().unwrap();

        let probe = probe.lock().unwrap();
        assert_eq!(session.state(), AsrState::Ready);
        assert_eq!(probe.start_calls, 1);
        assert_eq!(probe.finish_calls, 1);
        assert_eq!(probe.prepare_next_turn_calls, 1);
    }

    #[test]
    fn backend_errors_drive_error_state() {
        let probe = Arc::new(Mutex::new(BackendProbe {
            fail_accept: true,
            ..BackendProbe::default()
        }));
        let mut session = new_session(probe);
        let events = session.subscribe_events();
        session.start().unwrap();
        let error = session.accept_samples(&[0.1]).unwrap_err().to_string();
        assert!(error.contains("accept failed"));
        assert_eq!(session.state(), AsrState::Error);
        assert!(events.try_iter().any(
            |event| matches!(event, AsrEvent::Error(message) if message.contains("accept failed"))
        ));
    }

    #[test]
    fn paused_session_drops_intermediate_audio() {
        let probe = Arc::new(Mutex::new(BackendProbe::default()));
        let mut session = new_session(probe.clone());
        session.start().unwrap();
        session.pause().unwrap();
        let results = session.accept_samples(&[0.1, 0.2]).unwrap();
        assert!(results.is_empty());
        assert_eq!(probe.lock().unwrap().accept_calls, 0);
    }

    #[test]
    fn dropping_session_closes_event_stream() {
        let probe = Arc::new(Mutex::new(BackendProbe::default()));
        let receiver = {
            let session = new_session(probe);
            session.subscribe_events()
        };

        assert_receiver_disconnected(receiver.recv_timeout(Duration::from_millis(50)));
    }

    #[test]
    fn new_with_event_hub_exposes_early_model_missing_event() {
        let event_hub = Arc::new(AsrEventHub::default());
        let events = event_hub.subscribe();

        let result = HybridAsrSession::new_with_event_hub(
            HybridAsrConfig::Local(LocalAsrConfig {
                asr_model_dir: "missing-sense-voice-zh-en".to_string(),
                vad_model_dir: "missing-silero-vad".to_string(),
                auto_download: false,
                download_url: None,
                target_dir: None,
                vad_download_url: None,
                vad_target_dir: None,
                sample_rate: 16_000,
                num_threads: 1,
                use_itn: true,
                provider: "cpu".to_string(),
                min_silence_duration: 0.5,
                min_speech_duration: 0.25,
                vad_threshold: 0.5,
            }),
            Arc::clone(&event_hub),
        );

        let error = match result {
            Ok(_) => {
                panic!("expected local asr construction to fail when download config is missing")
            }
            Err(error) => error.to_string(),
        };

        assert!(error.contains("download_url"));
        let model_events = events
            .try_iter()
            .filter_map(|event| match event {
                AsrEvent::Model(event) => Some(event),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(model_events.iter().any(|event| {
            event.resource_kind == ModelResourceKind::Asr
                && event.phase == ModelLifecyclePhase::Missing
        }));
        assert_receiver_disconnected(events.recv_timeout(Duration::from_millis(50)));
    }
}
