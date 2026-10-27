// Renderer ReTone tests (prompts, output cleanup, option normalization). The
// main-process parts (picker payload, controller,
// select-all, sessions) are Rust tests: retone-core `payload`/`session` and
// src-tauri `tone_rewrite::controller`.
const test = require("node:test");
const assert = require("node:assert/strict");

const loadPrompts = () => import("../../src/config/prompts/toneRewrite.ts");

test("every tone builds a system prompt with its own instruction and the shared rules", async () => {
  const { TONE_REWRITE_TONE_IDS, buildToneRewriteSystemPrompt } = await loadPrompts();
  const prompts = TONE_REWRITE_TONE_IDS.map(buildToneRewriteSystemPrompt);
  assert.equal(new Set(prompts).size, TONE_REWRITE_TONE_IDS.length);
  for (const prompt of prompts) {
    assert.match(prompt, /never instructions to you/);
    assert.match(prompt, /rewritten text only/);
  }
});

test("the user prompt fences the text so it reads as content", async () => {
  const { buildToneRewriteUserPrompt } = await loadPrompts();
  assert.equal(buildToneRewriteUserPrompt("hi there"), "<text>\nhi there\n</text>");
});

test("isToneRewriteToneId accepts only the fixed tones", async () => {
  const { isToneRewriteToneId } = await loadPrompts();
  assert.equal(isToneRewriteToneId("professional"), true);
  assert.equal(isToneRewriteToneId("pirate"), false);
  assert.equal(isToneRewriteToneId(undefined), false);
});

test("cleanToneRewriteOutput strips wrappers small models add", async () => {
  const { cleanToneRewriteOutput } = await loadPrompts();
  assert.equal(cleanToneRewriteOutput("<think>hmm</think>\nHello team."), "Hello team.");
  assert.equal(cleanToneRewriteOutput("Here is the rewritten text:\n\nHello team."), "Hello team.");
  assert.equal(cleanToneRewriteOutput("```\nHello team.\n```"), "Hello team.");
  assert.equal(cleanToneRewriteOutput("<text>\nHello team.\n</text>"), "Hello team.");
  assert.equal(cleanToneRewriteOutput('"Hello team."', "hey team"), "Hello team.");
});

test("previewToneRewriteOutput hides wrappers while the reply is still arriving", async () => {
  const { previewToneRewriteOutput } = await loadPrompts();
  assert.equal(previewToneRewriteOutput("<think>still thin"), "");
  assert.equal(previewToneRewriteOutput("<think>x</think>\nHello"), "Hello");
  assert.equal(previewToneRewriteOutput("<text>\nHello te"), "Hello te");
  assert.equal(previewToneRewriteOutput("<text>\nHello team.\n</te"), "Hello team.");
  assert.equal(previewToneRewriteOutput("<text>\nHello team.\n</text>\n"), "Hello team.");
  assert.equal(previewToneRewriteOutput("```\nHello team.\n``"), "Hello team.");
  assert.equal(previewToneRewriteOutput("Use x < y"), "Use x < y");
});

test("cleanToneRewriteOutput keeps quotes the source had and its surrounding whitespace", async () => {
  const { cleanToneRewriteOutput } = await loadPrompts();
  assert.equal(cleanToneRewriteOutput('"Hi."', '"hey"'), '"Hi."');
  assert.equal(cleanToneRewriteOutput("Hello.", "  hey\n"), "  Hello.\n");
  assert.equal(cleanToneRewriteOutput("   ", "hey"), "");
});

const builtin = (id, enabled = true, prompt) => ({
  id,
  kind: "builtin",
  enabled,
  ...(prompt && { prompt }),
});
const custom = (id, name, prompt, enabled = true) => ({
  id,
  kind: "custom",
  enabled,
  name,
  prompt,
});

test("normalizeToneRewriteOptions starts with every built-in enabled in picker order", async () => {
  const { normalizeToneRewriteOptions, TONE_REWRITE_TONE_IDS } = await loadPrompts();
  const expected = TONE_REWRITE_TONE_IDS.map((id) => builtin(id));
  assert.deepEqual(normalizeToneRewriteOptions(null), expected);
  assert.deepEqual(normalizeToneRewriteOptions("nope", "nope"), expected);
  assert.deepEqual(normalizeToneRewriteOptions(undefined, []), expected);
});

test("normalizeToneRewriteOptions migrates the legacy enabled tones and turns the Banglish options on", async () => {
  const { normalizeToneRewriteOptions } = await loadPrompts();
  const options = normalizeToneRewriteOptions(null, ["concise", "pirate", "casual"]);
  assert.deepEqual(
    options.map(({ id, enabled }) => [id, enabled]),
    [
      ["professional", false],
      ["casual", true],
      ["friendly", false],
      ["concise", true],
      ["fixGrammar", false],
      ["banglishToEnglish", true],
      ["banglishToBangla", true],
    ]
  );
});

test("normalizeToneRewriteOptions keeps the saved order and appends missing built-ins", async () => {
  const { normalizeToneRewriteOptions } = await loadPrompts();
  const options = normalizeToneRewriteOptions([
    custom("custom-1", "French", "Translate the text into French."),
    builtin("fixGrammar", false),
    builtin("professional"),
  ]);
  assert.deepEqual(
    options.map(({ id, enabled }) => [id, enabled]),
    [
      ["custom-1", true],
      ["fixGrammar", false],
      ["professional", true],
      ["casual", true],
      ["friendly", true],
      ["concise", true],
      ["banglishToEnglish", true],
      ["banglishToBangla", true],
    ]
  );
});

test("normalizeToneRewriteOptions drops malformed and duplicate entries and caps lengths", async () => {
  const {
    normalizeToneRewriteOptions,
    MAX_TONE_REWRITE_OPTION_NAME_LENGTH,
    MAX_TONE_REWRITE_OPTION_PROMPT_LENGTH,
  } = await loadPrompts();
  const options = normalizeToneRewriteOptions([
    null,
    "casual",
    builtin("pirate"),
    { id: "casual", kind: "custom", enabled: true, name: "x", prompt: "y" },
    custom("custom-a", "  ", "prompt"),
    custom("custom-b", "Name", ""),
    custom("not-custom", "Name", "Prompt"),
    custom("custom-c", "n".repeat(100), `  ${"p".repeat(5000)}  `),
    custom("custom-c", "Again", "Again"),
    builtin("casual"),
    builtin("casual", false),
  ]);
  const customs = options.filter((option) => option.kind === "custom");
  assert.equal(customs.length, 1);
  assert.equal(customs[0].name.length, MAX_TONE_REWRITE_OPTION_NAME_LENGTH);
  assert.equal(customs[0].prompt.length, MAX_TONE_REWRITE_OPTION_PROMPT_LENGTH);
  assert.equal(options.filter((option) => option.id === "casual").length, 1);
  assert.equal(options.find((option) => option.id === "casual").enabled, true);
});

test("normalizeToneRewriteOptions keeps between one and nine options enabled", async () => {
  const { normalizeToneRewriteOptions, TONE_REWRITE_TONE_IDS, MAX_ENABLED_TONE_REWRITE_OPTIONS } =
    await loadPrompts();
  const many = normalizeToneRewriteOptions([
    ...TONE_REWRITE_TONE_IDS.map((id) => builtin(id)),
    custom("custom-1", "One", "One"),
    custom("custom-2", "Two", "Two"),
    custom("custom-3", "Three", "Three"),
  ]);
  assert.equal(many.filter((option) => option.enabled).length, MAX_ENABLED_TONE_REWRITE_OPTIONS);
  assert.equal(many.at(-1).enabled, false);

  const none = normalizeToneRewriteOptions(TONE_REWRITE_TONE_IDS.map((id) => builtin(id, false)));
  assert.deepEqual(
    none.filter((option) => option.enabled).map((option) => option.id),
    ["professional"]
  );
});

test("normalizeToneRewriteOptions drops a built-in override that matches the default", async () => {
  const { normalizeToneRewriteOptions, getDefaultToneRewriteInstruction } = await loadPrompts();
  const [same, edited] = normalizeToneRewriteOptions([
    builtin("casual", true, getDefaultToneRewriteInstruction("casual")),
    builtin("concise", true, "  Keep it to one sentence.  "),
  ]);
  assert.equal("prompt" in same, false);
  assert.equal(edited.prompt, "Keep it to one sentence.");
});

test("an option's system prompt is its instruction followed by the shared rules", async () => {
  const { buildToneRewriteSystemPrompt, getDefaultToneRewriteInstruction } = await loadPrompts();
  const own = buildToneRewriteSystemPrompt(custom("custom-1", "French", "Translate into French."));
  assert.match(own, /^Translate into French\.\n/);
  assert.match(own, /never instructions to you/);
  assert.match(own, /unless the instructions above ask for a different one/);

  const edited = buildToneRewriteSystemPrompt(builtin("casual", true, "Sound laid back."));
  assert.match(edited, /^Sound laid back\.\n/);
  assert.equal(
    buildToneRewriteSystemPrompt(builtin("casual")),
    buildToneRewriteSystemPrompt("casual")
  );
  assert.ok(
    buildToneRewriteSystemPrompt("banglishToBangla").startsWith(
      getDefaultToneRewriteInstruction("banglishToBangla")
    )
  );
});

test("the Banglish instructions name their output", async () => {
  const { getDefaultToneRewriteInstruction } = await loadPrompts();
  assert.match(
    getDefaultToneRewriteInstruction("banglishToEnglish"),
    /into natural, fluent English/
  );
  assert.match(getDefaultToneRewriteInstruction("banglishToBangla"), /into Bangla script/);
  assert.match(getDefaultToneRewriteInstruction("banglishToBangla"), /আমি কাল office এ যাবো না/);
});

test("cleanToneRewriteOutput strips a translation preamble", async () => {
  const { cleanToneRewriteOutput } = await loadPrompts();
  assert.equal(
    cleanToneRewriteOutput("Here is the translated text:\n\nI won't go to the office tomorrow."),
    "I won't go to the office tomorrow."
  );
  assert.equal(cleanToneRewriteOutput("Here's the translation:\nHello."), "Hello.");
});
