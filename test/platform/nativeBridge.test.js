const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const TYPES_PATH = path.join(__dirname, "../../src/types/nativeApi.ts");
const BRIDGE_PATH = path.join(__dirname, "../../src/platform/nativeBridge.ts");

// Method names declared on Window["retoneAPI"] in src/types/nativeApi.ts.
function declaredMethods() {
  const source = fs.readFileSync(TYPES_PATH, "utf8");
  const start = source.indexOf("retoneAPI: {");
  assert.notEqual(start, -1, "retoneAPI block not found");
  const end = source.indexOf("\n    };", start);
  const names = new Set();
  for (const line of source.slice(start, end).split("\n")) {
    const match = line.match(/^ {6}([A-Za-z]\w*)\??:/);
    if (match) names.add(match[1]);
  }
  return names;
}

// A fake Tauri runtime: records invokes and keeps event handlers so a test can
// fire events at them.
function installFakeTauri() {
  const invokes = [];
  const callbacks = new Map();
  const listeners = [];
  let nextId = 1;
  globalThis.window = {
    __TAURI_INTERNALS__: {
      metadata: {
        currentWindow: { label: "settings" },
        currentWebview: { windowLabel: "settings", label: "settings" },
      },
      transformCallback(callback) {
        const id = nextId++;
        callbacks.set(id, callback);
        return id;
      },
      unregisterCallback(id) {
        callbacks.delete(id);
      },
      async invoke(cmd, args) {
        invokes.push({ cmd, args });
        if (cmd === "plugin:event|listen") {
          listeners.push({ event: args.event, target: args.target, handler: args.handler });
          return listeners.length;
        }
        return null;
      },
    },
    __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener() {} },
  };
  const emit = (event, payload) => {
    for (const listener of listeners) {
      if (listener.event === event) callbacks.get(listener.handler)?.({ event, id: 0, payload });
    }
  };
  return { invokes, listeners, emit };
}

const flush = () => new Promise((resolve) => setImmediate(resolve));

test("the native bridge implements exactly the methods src/types/nativeApi.ts declares", async () => {
  installFakeTauri();
  const { nativeBridge } = await import(BRIDGE_PATH);
  const declared = declaredMethods();
  assert.ok(declared.size > 40, `parsed only ${declared.size} methods`);

  const implemented = new Set(Object.keys(nativeBridge));
  const missing = [...declared].filter((name) => !implemented.has(name));
  const extra = [...implemented].filter((name) => !declared.has(name));
  assert.deepEqual(missing, [], "declared but not implemented");
  assert.deepEqual(extra, [], "implemented but not declared");
  for (const name of implemented) {
    assert.equal(typeof nativeBridge[name], "function", name);
  }
});

test("importing the native bridge installs window.retoneAPI and getPlatform is synchronous darwin", async () => {
  installFakeTauri();
  const { nativeBridge, installNativeBridge } = await import(BRIDGE_PATH);
  installNativeBridge(globalThis.window);
  assert.equal(globalThis.window.retoneAPI, nativeBridge);
  assert.equal(nativeBridge.getPlatform(), "darwin");
});

test("methods invoke the snake_case command with named arguments", async () => {
  const { invokes } = installFakeTauri();
  const { nativeBridge: api } = await import(BRIDGE_PATH);

  await api.saveOpenAIKey("sk-1");
  await api.toneRewriteApply("session-1", "rewritten");
  await api.processLocalReasoning("text", "qwen", null, { maxTokens: 5 });
  await api.setUiLanguage("bn");
  await api.log({ level: "info", message: "hi" });

  assert.deepEqual(
    invokes.map(({ cmd, args }) => [cmd, args]),
    [
      ["save_openai_key", { key: "sk-1" }],
      ["tone_rewrite_apply", { sessionId: "session-1", text: "rewritten" }],
      [
        "process_local_reasoning",
        { text: "text", modelId: "qwen", agentName: null, config: { maxTokens: 5 } },
      ],
      ["set_ui_language", { language: "bn" }],
      ["app_log", { entry: { level: "info", message: "hi" } }],
    ]
  );
});

test("listeners subscribe on the current window and keep the (event, data) signatures", async () => {
  const { listeners, emit } = installFakeTauri();
  const { nativeBridge: api } = await import(BRIDGE_PATH);

  const progress = [];
  const keys = [];
  const stopProgress = api.onModelDownloadProgress((...args) => progress.push(args));
  api.onApiKeyUpdated((storeKey) => keys.push(storeKey));
  await flush();

  assert.deepEqual(
    listeners.map((l) => [l.event, l.target]),
    [
      ["model-download-progress", { kind: "WebviewWindow", label: "settings" }],
      ["api-key-updated", { kind: "WebviewWindow", label: "settings" }],
    ]
  );

  emit("model-download-progress", { modelId: "m", progress: 10 });
  emit("api-key-updated", "groqApiKey");
  stopProgress();
  emit("model-download-progress", { modelId: "m", progress: 20 });

  assert.deepEqual(progress, [[null, { modelId: "m", progress: 10 }]]);
  assert.deepEqual(keys, ["groqApiKey"]);
});

test("a listener removed before it finishes subscribing never fires", async () => {
  const { emit } = installFakeTauri();
  const { nativeBridge: api } = await import(BRIDGE_PATH);
  let calls = 0;
  const stop = api.onToneRewriteReset(() => calls++);
  stop();
  await flush();
  emit("tone-rewrite-reset", null);
  assert.equal(calls, 0);
});
