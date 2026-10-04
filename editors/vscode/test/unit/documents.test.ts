import * as assert from "node:assert/strict";
import { test } from "node:test";
import { DOCUMENTS } from "../../src/documents";

test("view templates are served whatever language an extension gives them", () => {
  const languages = DOCUMENTS.map((d) => d.language).filter(Boolean);
  assert.deepEqual(languages, ["ruby", "erb", "html.erb"]);
  const patterns = DOCUMENTS.map((d) => d.pattern).filter(Boolean);
  assert.deepEqual(patterns, ["**/*.erb", "**/*.rabl"]);
});
