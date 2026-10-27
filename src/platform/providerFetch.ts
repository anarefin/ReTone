// `fetch` for direct provider calls. The webview enforces CORS and Tauri
// cannot turn web security off, so inside the app requests go through
// `@tauri-apps/plugin-http`, which runs them in Rust and streams the body back.
// Scope (src-tauri/capabilities/default.json): any https URL, and plain http to
// 127.0.0.1 / localhost (the bundled llama-server and local servers).
//
// The plugin rejects with plain strings and errors a cancelled body with one,
// so failures are normalized to what the browser's fetch raises and the
// callers already classify: an `AbortError` DOMException once the caller's
// signal has fired, a `TypeError("fetch failed: …")` otherwise
// (asProviderError reads that as unreachable).
import { fetch as tauriFetch } from "@tauri-apps/plugin-http";

type FetchImpl = (input: string | URL, init?: RequestInit) => Promise<Response>;

function abortError(): Error {
  return new DOMException("The operation was aborted.", "AbortError");
}

function networkError(cause: unknown): TypeError {
  const detail =
    typeof cause === "string" ? cause : cause instanceof Error ? cause.message : String(cause);
  const error = new TypeError(`fetch failed: ${detail}`);
  (error as TypeError & { cause?: unknown }).cause = cause;
  return error;
}

function normalizeBody(response: Response, signal?: AbortSignal | null): Response {
  if (!response.body) return response;
  const reader = response.body.getReader();
  const body = new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const { done, value } = await reader.read();
        if (done) controller.close();
        else controller.enqueue(value);
      } catch (error) {
        controller.error(signal?.aborted ? abortError() : networkError(error));
      }
    },
    cancel(reason) {
      return reader.cancel(reason);
    },
  });
  const wrapped = new Response(body, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  });
  Object.defineProperty(wrapped, "url", { value: response.url });
  return wrapped;
}

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Builds a provider fetch over `rustFetch`, falling back to the global fetch outside Tauri. */
export function createProviderFetch(
  rustFetch: FetchImpl,
  inTauri: () => boolean = isTauriRuntime
): FetchImpl {
  return async (input, init = {}) => {
    if (!inTauri()) return globalThis.fetch(input, init);
    const signal = init.signal;
    if (signal?.aborted) throw abortError();
    let response: Response;
    try {
      // The plugin deletes its own options from `init`, so it gets a copy.
      response = await rustFetch(input, { ...init });
    } catch (error) {
      throw signal?.aborted ? abortError() : networkError(error);
    }
    return normalizeBody(response, signal);
  };
}

export const providerFetch: FetchImpl = createProviderFetch(tauriFetch);
