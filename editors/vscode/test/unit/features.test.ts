import * as assert from "node:assert/strict";
import { test } from "node:test";
import { FEATURES, middlewareFor } from "../../src/features";

test("every feature on means no middleware at all", () => {
  assert.deepEqual(middlewareFor(FEATURES), {});
});

test("a disabled feature answers nothing, for every hook that carries it", async () => {
  const middleware = middlewareFor(FEATURES.filter((f) => f !== "callHierarchy"));
  assert.deepEqual(Object.keys(middleware).sort(), [
    "prepareCallHierarchy",
    "provideCallHierarchyIncomingCalls",
    "provideCallHierarchyOutgoingCalls",
  ]);
  assert.equal(middleware.prepareCallHierarchy(), null);
});

test("disabled diagnostics publish nothing rather than passing through", () => {
  const middleware = middlewareFor(FEATURES.filter((f) => f !== "diagnostics"));
  let passed = false;
  middleware.handleDiagnostics("uri", [], () => (passed = true));
  assert.equal(passed, false);
});

test("an unknown name in the setting is ignored, not fatal", () => {
  assert.deepEqual(middlewareFor([...FEATURES, "formatting"]), {});
});
