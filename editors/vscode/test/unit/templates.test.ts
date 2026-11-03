import * as assert from "node:assert/strict";
import { test } from "node:test";
import { templateGate, templateKind } from "../../src/templates";

const ruby = { uri: { path: "/app/models/widget.rb" }, languageId: "ruby" };
const erb = { uri: { path: "/app/views/widgets/show.html.erb" }, languageId: "erb" };
const rabl = { uri: { path: "/app/views/widgets/show.json.rabl" }, languageId: "plaintext" };
const haml = { uri: { path: "/app/views/widgets/show.html.haml" }, languageId: "haml" };

// What a trekr older than 0.8.7 answers `initialize` with: no templates.
const old = { capabilities: { definitionProvider: true } };
const current = { capabilities: { experimental: { trekr: { templates: ["erb", "rabl"] } } } };

/** Runs a hook, saying whether it reached the server. */
async function reaches(initializeResult: unknown, hook: string, ...args: unknown[]): Promise<boolean> {
  let sent = false;
  await templateGate(() => initializeResult)[hook](...args, () => (sent = true));
  return sent;
}

test("a server that does not say it reads templates is never sent one", async () => {
  for (const document of [erb, rabl]) {
    assert.equal(await reaches(old, "didOpen", document), false);
    assert.equal(await reaches(old, "didChange", { document, contentChanges: [] }), false);
    assert.equal(await reaches(old, "provideHover", document, {}, {}), false);
  }
  assert.equal(templateGate(() => old).provideDefinition(erb, {}, {}, () => ["x"]), null);
});

test("nothing published for a template is shown unless the server reads it", async () => {
  assert.equal(await reaches(old, "handleDiagnostics", erb.uri, []), false);
  assert.equal(await reaches(current, "handleDiagnostics", erb.uri, []), true);
});

test("Ruby always reaches the server", async () => {
  assert.equal(await reaches(old, "didOpen", ruby), true);
  assert.equal(await reaches(undefined, "provideDefinition", ruby, {}, {}), true);
});

test("a server that lists a template kind is sent that kind only", async () => {
  const erbOnly = { capabilities: { experimental: { trekr: { templates: ["erb"] } } } };
  assert.equal(await reaches(current, "didOpen", rabl), true);
  assert.equal(await reaches(erbOnly, "didOpen", erb), true);
  assert.equal(await reaches(erbOnly, "didOpen", rabl), false);
});

test("a Haml template reaches only a server that lists haml", async () => {
  // A Haml extension's language id is no ERB's: the name decides.
  assert.equal(templateKind(haml), "haml");
  assert.equal(await reaches(current, "didOpen", haml), false);
  const withHaml = { capabilities: { experimental: { trekr: { templates: ["erb", "haml", "rabl"] } } } };
  assert.equal(await reaches(withHaml, "didOpen", haml), true);
});

test("an ERB language id on a file not named .erb is a template", () => {
  assert.equal(templateKind({ uri: { path: "/a/show.rhtml" }, languageId: "html.erb" }), "erb");
  assert.equal(templateKind({ uri: { path: "/a/Gemfile" }, languageId: "ruby" }), undefined);
});
