// Runs inside the VS Code extension host: the real client, the real trekr
// binary, through VS Code's own provider commands.
import * as assert from "node:assert/strict";
import * as path from "node:path";
import * as vscode from "vscode";

type Def = vscode.Location | vscode.LocationLink;

/** The position `offset` characters into the first occurrence of `needle`. */
function at(doc: vscode.TextDocument, needle: string, offset: number): vscode.Position {
  const line = doc.getText().split("\n").findIndex((l) => l.includes(needle));
  assert.ok(line >= 0, `fixture has no ${needle}`);
  return new vscode.Position(line, doc.lineAt(line).text.indexOf(needle) + offset);
}

function where(root: string, uri: vscode.Uri, range: vscode.Range): string {
  return `${path.relative(root, uri.fsPath)}:${range.start.line + 1}`;
}

/** Ask until the server answers — it starts asynchronously with the window. */
async function eventually<T>(ask: () => Thenable<T>, ok: (t: T) => boolean): Promise<T> {
  const deadline = Date.now() + 20_000;
  for (;;) {
    const answer = await ask();
    if (ok(answer) || Date.now() > deadline) return answer;
    await new Promise((r) => setTimeout(r, 200));
  }
}

export async function run() {
  const root = vscode.workspace.workspaceFolders![0].uri;
  const doc = await vscode.workspace.openTextDocument(vscode.Uri.joinPath(root, "app.rb"));
  await vscode.window.showTextDocument(doc);
  await vscode.extensions.getExtension("dpep.trekr")!.activate();

  // Go to Definition: `w.save`, where `w = Widget.new` — the receiver is typed.
  const defs = await eventually(
    () => vscode.commands.executeCommand<Def[]>("vscode.executeDefinitionProvider", doc.uri, at(doc, "w.save", 3)),
    (d) => d.length > 0,
  );
  assert.deepEqual(
    defs.map((d) => ("targetUri" in d ? where(root.fsPath, d.targetUri, d.targetRange) : where(root.fsPath, d.uri, d.range))),
    ["lib/widget.rb:6"],
  );

  // Find References on Widget#save: the typed call site is confirmed.
  const refs = await vscode.commands.executeCommand<vscode.Location[]>(
    "vscode.executeReferenceProvider",
    doc.uri,
    at(doc, "w.save", 3),
  );
  const lines = refs.map((r) => where(root.fsPath, r.uri, r.range)).sort();
  assert.ok(lines.includes("app.rb:4"), `references: ${lines}`);

  // Completion after `Widget.` offers the class method, not the instance's.
  const editor = await vscode.window.showTextDocument(doc);
  const end = new vscode.Position(doc.lineCount - 1, 0);
  await editor.edit((b) => b.insert(end, "Widget.\n"));
  const list = await vscode.commands.executeCommand<vscode.CompletionList>(
    "vscode.executeCompletionItemProvider",
    doc.uri,
    new vscode.Position(end.line, "Widget.".length),
    ".",
  );
  const labels = list.items.map((i) => (typeof i.label === "string" ? i.label : i.label.label));
  assert.ok(labels.includes("build"), `completion: ${labels.slice(0, 10)}`);
  assert.ok(!labels.includes("save"), "an instance method is not offered on the class");

  // Hover: a resolved answer is the signature and where it lives, with no
  // caveat; a guess says so in words, never as a number (DEC-052).
  const hover = async (needle: string, offset: number) => {
    const hovers = await vscode.commands.executeCommand<vscode.Hover[]>("vscode.executeHoverProvider", doc.uri, at(doc, needle, offset));
    return hovers.flatMap((h) => h.contents.map((c) => (typeof c === "string" ? c : c.value))).join("\n");
  };
  const resolved = await hover("w.save", 3);
  assert.match(resolved, /```ruby\n.*save/, resolved);
  assert.match(resolved, /Defined in .*lib\/widget\.rb:6/, resolved);
  assert.doesNotMatch(resolved, /confidence|status|resolved_via/i, resolved);

  const guess = await hover("w.zap", 3);
  assert.match(guess, /`Widget` has no `zap`.*may come from a gem, a DSL, or `method_missing`/, guess);
  assert.doesNotMatch(guess, /\d/, `a guess is words, not a number: ${guess}`);
  assert.doesNotMatch(guess, /Defined in|```/, "a guess shows no signature or location");

  // The outline is nested: `run` inside `Job`.
  const symbols = await vscode.commands.executeCommand<vscode.DocumentSymbol[]>(
    "vscode.executeDocumentSymbolProvider",
    doc.uri,
  );
  assert.equal(symbols[0].name, "Job");
  assert.equal(symbols[0].children[0].name, "run");

  // A syntax error is published as a diagnostic from trekr.
  await editor.edit((b) => b.insert(new vscode.Position(doc.lineCount - 1, 0), "def broken(\n"));
  const diagnostics = await eventually(
    () => Promise.resolve(vscode.languages.getDiagnostics(doc.uri)),
    (d) => d.some((x) => x.source === "trekr"),
  );
  assert.ok(diagnostics.some((d) => d.source === "trekr"), "a syntax diagnostic from trekr");

  // A template reaches a trekr that says it reads them: its tags are Ruby
  // at the template's own positions, and its markup is no syntax error.
  const view = await vscode.workspace.openTextDocument(vscode.Uri.joinPath(root, "app/views/widgets/show.html.erb"));
  await vscode.window.showTextDocument(view);
  const helper = await eventually(
    () => vscode.commands.executeCommand<Def[]>("vscode.executeDefinitionProvider", view.uri, at(view, "badge", 2)),
    (d) => d.length > 0,
  );
  assert.deepEqual(
    helper.map((d) => ("targetUri" in d ? where(root.fsPath, d.targetUri, d.targetRange) : where(root.fsPath, d.uri, d.range))),
    ["app/helpers/widgets_helper.rb:2"],
  );
  const markup = vscode.languages.getDiagnostics(view.uri).filter((d) => d.source === "trekr");
  assert.deepEqual(markup.map((d) => d.message), [], "markup is no syntax error");
}
