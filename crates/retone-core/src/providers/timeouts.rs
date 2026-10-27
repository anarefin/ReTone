//! Client-side LLM deadlines. The renderer keeps its own copy
//! (`src/helpers/llmRequestTimeout.js`) for the calls it makes; these apply to calls made here.

/// Deadline for a non-streaming request.
pub const LLM_REQUEST_TIMEOUT_SECONDS: u64 = 30;
/// Idle timeout for a stream.
pub const LLM_STREAMING_TIMEOUT_SECONDS: u64 = 60;
/// A local model may load and prefill for minutes before its first streamed byte.
pub const LLM_FIRST_BYTE_LOCAL_TIMEOUT_SECONDS: u64 = 600;

pub const LLM_REQUEST_TIMEOUT_CODE: &str = "LLM_REQUEST_TIMEOUT";

pub fn llm_request_timeout_seconds(streaming: bool) -> u64 {
    if streaming {
        LLM_STREAMING_TIMEOUT_SECONDS
    } else {
        LLM_REQUEST_TIMEOUT_SECONDS
    }
}

/// Local and self-hosted models get the long first-byte allowance.
pub fn llm_streaming_first_byte_timeout_seconds(self_hosted: bool) -> u64 {
    if self_hosted {
        LLM_FIRST_BYTE_LOCAL_TIMEOUT_SECONDS
    } else {
        LLM_STREAMING_TIMEOUT_SECONDS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_match_the_renderer() {
        assert_eq!(llm_request_timeout_seconds(false), 30);
        assert_eq!(llm_request_timeout_seconds(true), 60);
        assert_eq!(llm_streaming_first_byte_timeout_seconds(true), 600);
        assert_eq!(llm_streaming_first_byte_timeout_seconds(false), 60);
    }
}
