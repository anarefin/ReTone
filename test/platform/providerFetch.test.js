const test = require("node:test");
const assert = require("node:assert/strict");

const load = () => import("../../src/platform/providerFetch.ts");

const inTauri = () => true;

function streamOf(chunks, { failWith } = {}) {
  let i = 0;
  return new ReadableStream({
    pull(controller) {
      if (i < chunks.length) {
        controller.enqueue(new TextEncoder().encode(chunks[i++]));
      } else if (failWith !== undefined) {
        controller.error(failWith);
      } else {
        controller.close();
      }
    },
  });
}

test("outside Tauri it is the global fetch", async (t) => {
  const { createProviderFetch } = await load();
  const original = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = original;
  });
  const seen = [];
  globalThis.fetch = async (url, init) => {
    seen.push([url, init.method]);
    return new Response("ok");
  };
  const rustFetch = async () => assert.fail("the plugin is not used outside Tauri");
  const response = await createProviderFetch(rustFetch, () => false)("https://x.test/v1", {
    method: "POST",
  });
  assert.equal(await response.text(), "ok");
  assert.deepEqual(seen, [["https://x.test/v1", "POST"]]);
});

test("passes the request through and streams the body back", async () => {
  const { createProviderFetch } = await load();
  let received;
  const rustFetch = async (url, init) => {
    received = { url, init };
    return new Response(streamOf(['data: {"a":1}\n\n', "data: [DONE]\n\n"]), {
      status: 200,
      headers: { "x-request-id": "req_1" },
    });
  };
  const init = { method: "POST", headers: { Authorization: "Bearer k" }, body: "{}" };
  const response = await createProviderFetch(rustFetch, inTauri)("https://api.test/v1", init);
  assert.equal(received.url, "https://api.test/v1");
  assert.notEqual(received.init, init, "the plugin gets a copy it may mutate");
  assert.equal(received.init.headers.Authorization, "Bearer k");
  assert.equal(response.status, 200);
  assert.equal(response.ok, true);
  assert.equal(response.headers.get("x-request-id"), "req_1");

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  const parts = [];
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    parts.push(decoder.decode(value));
  }
  assert.deepEqual(parts, ['data: {"a":1}\n\n', "data: [DONE]\n\n"]);
});

test("error statuses keep their body for classification", async () => {
  const { createProviderFetch } = await load();
  const rustFetch = async () => new Response('{"error":"bad key"}', { status: 401 });
  const response = await createProviderFetch(rustFetch, inTauri)("https://api.test/v1");
  assert.equal(response.ok, false);
  assert.equal(response.status, 401);
  assert.equal(await response.text(), '{"error":"bad key"}');
});

test("a plugin rejection becomes the TypeError the browser's fetch raises", async () => {
  const { createProviderFetch } = await load();
  const rustFetch = async () => {
    throw "error sending request for url (https://api.test/v1)";
  };
  await assert.rejects(createProviderFetch(rustFetch, inTauri)("https://api.test/v1"), (error) => {
    assert.ok(error instanceof TypeError);
    assert.match(error.message, /^fetch failed: error sending request/);
    return true;
  });
});

test("a cancelled request is an AbortError, before, during and after the headers", async () => {
  const { createProviderFetch } = await load();
  const isAbort = (error) => error?.name === "AbortError";

  const early = new AbortController();
  early.abort();
  const never = async () => assert.fail("an aborted request is never sent");
  await assert.rejects(
    createProviderFetch(never, inTauri)("https://api.test", { signal: early.signal }),
    isAbort
  );

  const during = new AbortController();
  const cancelledInFlight = async () => {
    during.abort();
    throw new Error("Request cancelled");
  };
  await assert.rejects(
    createProviderFetch(cancelledInFlight, inTauri)("https://api.test", { signal: during.signal }),
    isAbort
  );

  const streaming = new AbortController();
  const rustFetch = async () =>
    new Response(streamOf(["data: 1\n\n"], { failWith: "Request cancelled" }), { status: 200 });
  const response = await createProviderFetch(rustFetch, inTauri)("https://api.test", {
    signal: streaming.signal,
  });
  const reader = response.body.getReader();
  await reader.read();
  streaming.abort();
  await assert.rejects(reader.read(), isAbort);
});

test("a body that fails without a cancel is a network TypeError", async () => {
  const { createProviderFetch } = await load();
  const rustFetch = async () =>
    new Response(streamOf([], { failWith: "connection reset" }), { status: 200 });
  const response = await createProviderFetch(rustFetch, inTauri)("https://api.test");
  await assert.rejects(response.text(), (error) => {
    assert.ok(error instanceof TypeError);
    return true;
  });
});
