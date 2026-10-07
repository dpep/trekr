// Which LSP features the user has switched off, as client middleware — pure,
// so it is unit-tested without VS Code.

export const FEATURES = [
  "definition",
  "declaration",
  "references",
  "hover",
  "documentSymbol",
  "workspaceSymbol",
  "implementation",
  "callHierarchy",
  "completion",
  "diagnostics",
  "documentLink",
  "documentHighlight",
] as const;

export type Feature = (typeof FEATURES)[number];

/** The middleware hooks that carry each feature. A disabled feature's hooks answer nothing. */
const HOOKS: Record<Feature, string[]> = {
  definition: ["provideDefinition"],
  declaration: ["provideDeclaration"],
  references: ["provideReferences"],
  hover: ["provideHover"],
  documentSymbol: ["provideDocumentSymbols"],
  workspaceSymbol: ["provideWorkspaceSymbols"],
  implementation: ["provideImplementation"],
  callHierarchy: ["prepareCallHierarchy", "provideCallHierarchyIncomingCalls", "provideCallHierarchyOutgoingCalls"],
  completion: ["provideCompletionItem"],
  diagnostics: ["handleDiagnostics"],
  documentLink: ["provideDocumentLinks", "resolveDocumentLink"],
  documentHighlight: ["provideDocumentHighlights"],
};

/**
 * Middleware that silences every disabled feature. Unknown names in the
 * setting are ignored rather than fatal — a typo should not take the server
 * down with it.
 */
export function middlewareFor(enabled: readonly string[]): Record<string, (...args: unknown[]) => unknown> {
  const on = new Set(enabled);
  const middleware: Record<string, (...args: unknown[]) => unknown> = {};
  for (const feature of FEATURES) {
    if (on.has(feature)) continue;
    for (const hook of HOOKS[feature]) {
      // handleDiagnostics(uri, diagnostics, next): publish nothing.
      // Every provider hook: answer nothing, without asking the server.
      middleware[hook] = hook === "handleDiagnostics" ? () => undefined : () => null;
    }
  }
  return middleware;
}
