// A thin client: launch `trekr --lsp` for Ruby files and view templates, and
// get out of the way.
// Everything it answers comes from the server; this file only starts it,
// switches features off on request, and says so plainly when it cannot start.
import * as vscode from "vscode";
import { LanguageClient, LanguageClientOptions, ServerOptions, State } from "vscode-languageclient/node";
import { DOCUMENTS } from "./documents";
import { middlewareFor } from "./features";

let client: LanguageClient | undefined;
let output: vscode.OutputChannel;

export async function activate(ctx: vscode.ExtensionContext) {
  output = vscode.window.createOutputChannel("trekr");
  ctx.subscriptions.push(
    output,
    vscode.commands.registerCommand("trekr.restart", restart),
    vscode.commands.registerCommand("trekr.showOutput", () => output.show()),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("trekr")) {
        vscode.window
          .showInformationMessage("trekr: settings changed — restart the server to apply them.", "Restart")
          .then((choice) => choice && restart());
      }
    }),
  );
  await start();
}

export async function deactivate() {
  await client?.stop();
}

async function start() {
  const config = vscode.workspace.getConfiguration("trekr");
  const command = config.get<string>("path") || "trekr";
  const serverOptions: ServerOptions = { command, args: ["--lsp"] };
  const clientOptions: LanguageClientOptions = {
    documentSelector: DOCUMENTS,
    outputChannel: output,
    initializationOptions: {
      index: config.get<boolean>("index", true),
      referenceLimit: config.get<number>("referenceLimit", 1000),
      unresolved: config.get<string>("unresolved", "confident"),
    },
    middleware: middlewareFor(config.get<string[]>("features") ?? []),
  };
  client = new LanguageClient("trekr", "trekr", serverOptions, clientOptions);
  client.onDidChangeState((e) => {
    if (e.newState === State.Stopped) output.appendLine("trekr: server stopped");
  });
  try {
    await client.start();
  } catch (err) {
    reportUnstartable(command, err);
  }
}

async function restart() {
  if (client) {
    await client.stop().catch(() => undefined);
    client = undefined;
  }
  await start();
}

function reportUnstartable(command: string, err: unknown) {
  output.appendLine(`trekr: could not start "${command} --lsp": ${err}`);
  vscode.window
    .showErrorMessage(
      `trekr: couldn't start "${command}". Install it (brew install dpep/tools/trekr) or point the trekr.path setting at it.`,
      "Open Settings",
    )
    .then((choice) => {
      if (choice) vscode.commands.executeCommand("workbench.action.openSettings", "trekr.path");
    });
}

/** For the e2e suite: the running client, once started. */
export function currentClient(): LanguageClient | undefined {
  return client;
}
