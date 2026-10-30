const test = require("node:test");
const assert = require("node:assert/strict");

// The bundled llama-server runs with a per-launch API key: the live preview,
// which streams to its port directly, must send it as a Bearer token.

function sse(chunks) {
  const encoder = new TextEncoder();
  const body = chunks
    .map((text) => `data: ${JSON.stringify({ choices: [{ delta: { content: text } }] })}\n\n`)
    .join("");
  return new Response(
    new ReadableStream({
      start(controller) {
        controller.enqueue(encoder.encode(body + "data: [DONE]\n\n"));
        controller.close();
      },
    }),
    { status: 200, headers: { "Content-Type": "text/event-stream" } }
  );
}

test("the local live preview sends the server's key as a Bearer token", async (t) => {
  const requests = [];
  const originalFetch = globalThis.fetch;
  const originalWindow = globalThis.window;
  t.after(() => {
    globalThis.fetch = originalFetch;
    globalThis.window = originalWindow;
  });
  globalThis.window = {
    ...(originalWindow || {}),
    addEventListener: () => {},
    retoneAPI: {
      llamaServerStart: async () => ({ success: true, port: 8231, apiKey: "launch-key" }),
      appLog: async () => {},
    },
  };
  globalThis.fetch = async (input, init = {}) => {
    requests.push({ url: String(input), headers: init.headers || {} });
    return sse(["Hel", "lo"]);
  };

  const loaded = await import("../../src/services/ReasoningService.ts");
  const reasoningService = loaded.default?.default ?? loaded.default;
  // Stops the key cache's cleanup interval so the test process can exit.
  t.after(() => reasoningService.destroy());
  let text = "";
  for await (const delta of reasoningService.processTextStreaming(
    [{ role: "user", content: "hi" }],
    "qwen3.5-2b-q4_k_m",
    "local",
    { systemPrompt: "Rewrite." }
  )) {
    text += delta;
  }

  assert.equal(text, "Hello");
  const chat = requests.filter((r) => r.url.endsWith("/v1/chat/completions"));
  assert.equal(chat.length, 1);
  assert.equal(chat[0].url, "http://127.0.0.1:8231/v1/chat/completions");
  assert.equal(chat[0].headers.Authorization, "Bearer launch-key");
});
