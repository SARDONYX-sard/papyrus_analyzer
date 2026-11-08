import * as path from "node:path";
import * as vscode from "vscode";

import { isWithin } from "../paths";
import { state } from "../state";
import { runTests } from "../tests";
import { runCompile } from "./task";

/** Key of the scripts picked last, in the workspace state. */
const SELECTION_KEY = "papyrus.compileSelection";

/**
 * `papyrus.compile`: from a code lens (a URI string), the explorer context
 * menu (the clicked URI and the selection) or the active editor.
 */
export async function compileCommand(arg?: string | vscode.Uri, selection?: vscode.Uri[]): Promise<void> {
  let uris: vscode.Uri[];
  if (selection && selection.length > 0) {
    uris = selection;
  } else if (arg) {
    uris = [typeof arg === "string" ? vscode.Uri.parse(arg) : arg];
  } else if (vscode.window.activeTextEditor?.document.languageId === "papyrus") {
    uris = [vscode.window.activeTextEditor.document.uri];
  } else {
    void vscode.window.showWarningMessage("Open a Papyrus script to compile it.");
    return;
  }
  uris = uris.filter((u) => u.fsPath.toLowerCase().endsWith(".psc"));
  // Test scripts (`[test] sources`) are never compiled: they import the
  // built-in `PapyrusAnalyzer`, unknown to the compiler. Alone, their tests
  // run instead, like their button in the explorer.
  const isTest = (u: vscode.Uri) => state.testSources.some((dir) => isWithin(u.fsPath, dir));
  const scripts = uris.filter((u) => !isTest(u));
  if (scripts.length === 0 && uris.length > 0) {
    await runTests(uris.map((u) => u.fsPath));
    return;
  }
  await runCompile({ type: "papyrus", files: scripts.map((u) => u.fsPath) });
}

/** `papyrus.compileAll`: every script of `[build] sources`. */
export async function compileAll(): Promise<void> {
  await runCompile({ type: "papyrus", all: true });
}

/**
 * `papyrus.compileSelected`: picks scripts of `[build] sources` (the workspace
 * folders without papyrus.toml), remembering the last pick, and compiles them.
 */
export async function compileSelected(): Promise<void> {
  const context = state.context;
  if (!context) {
    return;
  }
  const dirs =
    state.sources.length > 0 ? state.sources : (vscode.workspace.workspaceFolders ?? []).map((f) => f.uri.fsPath);
  const found = await Promise.all(
    dirs.map((dir) => vscode.workspace.findFiles(new vscode.RelativePattern(vscode.Uri.file(dir), "**/*.psc"))),
  );
  const files = [...new Set(found.flat().map((u) => u.fsPath))].sort();
  if (files.length === 0) {
    void vscode.window.showWarningMessage("No Papyrus scripts found to compile.");
    return;
  }
  const previous = new Set(context.workspaceState.get<string[]>(SELECTION_KEY, []));
  const items = files.map((file) => ({
    label: path.basename(file, path.extname(file)),
    description: vscode.workspace.asRelativePath(path.dirname(file)),
    picked: previous.has(file),
    file,
  }));
  const picked = await vscode.window.showQuickPick(items, {
    canPickMany: true,
    title: "Compile Papyrus scripts",
    placeHolder: "Select the scripts to compile (the selection is remembered)",
  });
  if (!picked || picked.length === 0) {
    return;
  }
  const selected = picked.map((p) => p.file);
  await context.workspaceState.update(SELECTION_KEY, selected);
  await runCompile({ type: "papyrus", files: selected });
}
