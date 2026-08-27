use std::error::Error;

pub type HybridAsrError = Box<dyn Error + Send + Sync>;
pub type HybridAsrResult<T> = Result<T, HybridAsrError>;
