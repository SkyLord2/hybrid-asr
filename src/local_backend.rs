use crate::backend::RealtimeAsrBackend;
use crate::config::LocalAsrConfig;
use crate::engine::local::AsrEngine;
use crate::error::HybridAsrResult;
use crate::event::AsrEventHub;
use crate::types::AsrResult;
use std::sync::Arc;

pub struct LocalAsrBackend {
    engine: AsrEngine,
}

impl LocalAsrBackend {
    pub fn new(
        options: LocalAsrConfig,
        event_hub: Option<Arc<AsrEventHub>>,
    ) -> HybridAsrResult<Self> {
        Ok(Self {
            engine: AsrEngine::new(&options, event_hub)?,
        })
    }
}

impl RealtimeAsrBackend for LocalAsrBackend {
    fn start(&mut self) -> HybridAsrResult<()> {
        Ok(())
    }

    fn accept_samples(&mut self, samples: &[f32]) -> HybridAsrResult<Vec<AsrResult>> {
        Ok(self
            .engine
            .accept_samples(samples)
            .into_iter()
            .map(|result| AsrResult {
                text: result.text,
                start_time: result.start_time,
                end_time: result.end_time,
                start_sample: result.start_sample,
                sample_count: result.sample_count,
                is_final: true,
            })
            .collect())
    }

    fn reset(&mut self) -> HybridAsrResult<()> {
        self.engine.reset();
        Ok(())
    }

    fn prepare_next_turn(&mut self) -> HybridAsrResult<()> {
        // Local SenseVoice/VAD state is fully in-memory, so preparing the next
        // question is equivalent to clearing buffered audio and segment queues.
        self.engine.reset();
        Ok(())
    }

    fn finish(&mut self) -> HybridAsrResult<Vec<AsrResult>> {
        Ok(self
            .engine
            .flush()
            .into_iter()
            .map(|result| AsrResult {
                text: result.text,
                start_time: result.start_time,
                end_time: result.end_time,
                start_sample: result.start_sample,
                sample_count: result.sample_count,
                is_final: true,
            })
            .collect())
    }
}
