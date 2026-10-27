const LLM_REQUEST_TIMEOUT_SECONDS = 30;
const LLM_STREAMING_TIMEOUT_SECONDS = 60;
// A local model may load and prefill for minutes before its first streamed byte.
const LLM_FIRST_BYTE_LOCAL_TIMEOUT_SECONDS = 600;

export const LLM_REQUEST_TIMEOUT_CODE = "LLM_REQUEST_TIMEOUT";

/**
 * Client-side deadline for a non-streaming LLM request, or idle timeout for a stream.
 * @param {{ streaming?: boolean, scope?: string }} [options] `scope` is the
 *   request's inference scope; every scope shares one deadline today.
 */
export function getLlmRequestTimeoutSeconds({ streaming = false } = {}) {
  return streaming ? LLM_STREAMING_TIMEOUT_SECONDS : LLM_REQUEST_TIMEOUT_SECONDS;
}

/**
 * Local and self-hosted models can spend minutes loading or prefilling before
 * sending any bytes. Once bytes arrive, the regular streaming idle timeout applies.
 * @param {{ selfHosted?: boolean }} [options] Includes the built-in local server.
 */
export function getLlmStreamingFirstByteTimeoutSeconds({ selfHosted = false } = {}) {
  return selfHosted ? LLM_FIRST_BYTE_LOCAL_TIMEOUT_SECONDS : LLM_STREAMING_TIMEOUT_SECONDS;
}

/** The coded error a request raises when its client-side deadline expires; createApiRetryStrategy never retries it. */
export function llmRequestTimeoutError(timeoutSeconds) {
  return Object.assign(new Error(`Request timed out after ${timeoutSeconds}s`), {
    code: LLM_REQUEST_TIMEOUT_CODE,
  });
}
