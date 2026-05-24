use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MetricsError {
    #[error("metric file was empty")]
    Empty,
    #[error("metric value was not an unsigned integer")]
    InvalidInteger,
}

pub fn parse_memory_current(input: &str) -> Result<u64, MetricsError> {
    let value = input.trim();
    if value.is_empty() {
        return Err(MetricsError::Empty);
    }
    value.parse().map_err(|_| MetricsError::InvalidInteger)
}
