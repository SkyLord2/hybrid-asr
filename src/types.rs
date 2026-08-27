#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsrMode {
    Local,
    Online,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsrState {
    Idle,
    Initializing,
    Ready,
    Recognizing,
    Paused,
    Finishing,
    Finished,
    Error,
}

#[derive(Clone, Debug)]
pub struct AsrResult {
    pub text: String,
    pub start_time: f64,
    pub end_time: f64,
    pub start_sample: i64,
    pub sample_count: i64,
    pub is_final: bool,
}
