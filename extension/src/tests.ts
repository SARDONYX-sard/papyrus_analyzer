import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";

import { serverPath } from "./settings";
import { state } from "./state";

/** The folder of the papyrus.toml of `file`, else its workspace folder. */
export function projectDir(file: string): string {
  let dir = path.dirname(file);
  while (!fs.existsSync(path.join(dir, "papyrus.toml")) && path.dirname(dir) !== dir) {
    dir = path.dirname(dir);
  }
  if (fs.existsSync(path.join(dir, "papyrus.toml"))) {
    return dir;
  }
  return vscode.workspace.getWorkspaceFolder(vscode.Uri.file(file))?.uri.fsPath ?? path.dirname(file);
}

/** Runs `papyrus_analyzer ARGS` in a terminal, from `cwd`. */
export async function runServerTask(args: string[], cwd: string, name: string): Promise<void> {
  if (!state.context) {
    return;
  }
  const execution = new vscode.ProcessExecution(serverPath(state.context), args, { cwd });
  const task = new vscode.Task({ type: "papyrus-run" }, vscode.TaskScope.Workspace, name, "papyrus", execution);
  task.presentationOptions = { reveal: vscode.TaskRevealKind.Always, clear: true };
  await vscode.tasks.executeTask(task);
}

/**
 * `papyrus_analyzer test FILES...`: the tests of these scripts or folders;
 * `doc`: their doc tests only (`--doc`).
 */
export async function runTests(files: string[], doc = false): Promise<void> {
  if (files.length === 0) {
    return;
  }
  const cwd = projectDir(files[0]);
  const args = files.map((f) => path.relative(cwd, f) || ".");
  const what = doc ? "doc test" : "test";
  const name = files.length === 1 ? `${what} ${path.basename(files[0])}` : `${what} ${files.length} paths`;
  await runServerTask(["test", ...(doc ? ["--doc"] : []), ...args], cwd, name);
}
