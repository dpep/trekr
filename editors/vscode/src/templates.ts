// Which view templates the running server reads, as client middleware — pure,
// so it is unit-tested without VS Code.
//
// The document selector is fixed when the client starts, before the server has
// said what it is; a trekr older than 0.8.7 parses a `.erb` as Ruby and floods
// it with syntax errors. So templates are selected always, and every message
// about one is held back unless the server's `initialize` result lists its
// kind under `experimental.trekr.templates`.

export type TemplateKind = "erb" | "rabl";

/** The parts of a `TextDocument` (or a bare `Uri`) the gate reads. */
interface DocumentLike {
  uri?: { path: string };
  path?: string;
  languageId?: string;
}

/** The template kind a document is, or `undefined` for Ruby. */
export function templateKind(document: DocumentLike): TemplateKind | undefined {
  const path = document.uri?.path ?? document.path ?? "";
  if (path.endsWith(".rabl")) return "rabl";
  if (path.endsWith(".erb")) return "erb";
  // Selected by an ERB language id on a file not named `.erb`.
  if (document.languageId !== undefined && document.languageId !== "ruby") return "erb";
  return undefined;
}

/** The template kinds an `initialize` result says the server reads. */
export function servedTemplates(initializeResult: unknown): ReadonlySet<string> {
  const kinds = (initializeResult as { capabilities?: { experimental?: { trekr?: { templates?: unknown } } } })
    ?.capabilities?.experimental?.trekr?.templates;
  return new Set(Array.isArray(kinds) ? kinds.filter((k): k is string => typeof k === "string") : []);
}

/** Every hook whose first argument is (or carries) the document it is about. */
const HOOKS = [
  "didOpen",
  "didChange",
  "didSave",
  "didClose",
  "provideDefinition",
  "provideReferences",
  "provideHover",
  "provideDocumentSymbols",
  "provideImplementation",
  "prepareCallHierarchy",
  "provideCompletionItem",
  "provideDocumentLinks",
  "provideDocumentHighlights",
  "handleDiagnostics",
] as const;

/** `didChange`'s event carries its document; every other hook leads with it. */
function documentOf(first: unknown): DocumentLike {
  const arg = first as DocumentLike & { document?: DocumentLike };
  return arg?.document ?? arg ?? {};
}

/**
 * Middleware that holds back a template the server does not read: it is never
 * opened there, never asked about, and nothing published for it is shown.
 * `initializeResult` is read on each call — it exists only once the server has
 * answered `initialize`, which is before any document is opened there.
 */
export function templateGate(initializeResult: () => unknown): Record<string, (...args: unknown[]) => unknown> {
  const middleware: Record<string, (...args: unknown[]) => unknown> = {};
  for (const hook of HOOKS) {
    middleware[hook] = (...args: unknown[]) => {
      const kind = templateKind(documentOf(args[0]));
      const next = args[args.length - 1] as (...rest: unknown[]) => unknown;
      if (kind === undefined || servedTemplates(initializeResult()).has(kind)) {
        return next(...args.slice(0, -1));
      }
      // A notification is a promise of nothing; a provider answers nothing.
      return hook.startsWith("did") ? Promise.resolve() : hook === "handleDiagnostics" ? undefined : null;
    };
  }
  return middleware;
}
