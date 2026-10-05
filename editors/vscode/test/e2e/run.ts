// Launches a real VS Code (downloaded by @vscode/test-electron) on a copy of
// the fixture committed to a scratch git repo, with the extension under
// development pointed at a trekr build and an isolated index.
import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { runTests } from "@vscode/test-electron";

async function main() {
  const ext = path.resolve(__dirname, "../../..");
  // The build under test: TREKR_BIN, else the debug build in cargo's target
  // directory — CARGO_TARGET_DIR's, relative to the repo as cargo reads it
  // there, when set.
  const repo = path.resolve(ext, "../..");
  const target = path.resolve(repo, process.env.CARGO_TARGET_DIR || "target");
  const trekr = process.env.TREKR_BIN || path.join(target, "debug", "trekr");
  if (!fs.existsSync(trekr)) throw new Error(`no trekr binary at ${trekr} — cargo build, or set TREKR_BIN`);

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "trekr-vscode-e2e-"));
  const workspace = path.join(tmp, "workspace");
  const db = path.join(tmp, "trekr.db");
  fs.cpSync(path.join(ext, "test", "fixture"), workspace, { recursive: true });
  fs.mkdirSync(path.join(workspace, ".vscode"));
  fs.writeFileSync(path.join(workspace, ".vscode", "settings.json"), JSON.stringify({ "trekr.path": trekr }));

  // trekr indexes git checkouts; the index is built up front so the suite
  // tests answers, not how long a background index takes.
  // Without git's locating variables: a run under `git rebase --exec` exports
  // GIT_DIR, and the fixture's `git init` would write into the real repo.
  // Cleared on this process, so VS Code and the server it launches lose them too.
  delete process.env.GIT_DIR;
  delete process.env.GIT_WORK_TREE;
  delete process.env.GIT_INDEX_FILE;
  const env = process.env;
  const git = (...args: string[]) => execFileSync("git", args, { cwd: workspace, env });
  git("init", "-q");
  git("add", "-A");
  git("-c", "user.email=t@e.st", "-c", "user.name=test", "commit", "-qm", "fixture");
  execFileSync(trekr, ["--index", "--no-gems"], { cwd: workspace, env: { ...env, TREKR_DB: db } });

  try {
    await runTests({
      extensionDevelopmentPath: ext,
      extensionTestsPath: path.join(__dirname, "suite"),
      launchArgs: [workspace, "--disable-extensions", "--user-data-dir", path.join(tmp, "user")],
      extensionTestsEnv: { TREKR_DB: db, TREKR_LOG: path.join(tmp, "lsp.log") },
      // Reuse a VS Code download when one is cached, rather than fetching it again.
      cachePath: process.env.VSCODE_TEST_CACHE,
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
