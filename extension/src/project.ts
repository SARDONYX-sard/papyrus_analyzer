import * as path from "node:path";
import * as vscode from "vscode";

import { state } from "./state";
import { runServerTask } from "./tests";

/**
 * The commands on the whole project of papyrus.toml: `papyrus_analyzer fmt`
 * and `lint` on its `[build] sources` and `[test] sources`, in a terminal.
 */

/** The source directories of papyrus.toml, `undefined` (and a message) without it. */
function projectSources(): { dirs: string[]; cwd: string } | undefined {
  const dirs = [...state.sources, ...state.testSources];
  if (!state.manifest || dirs.length === 0) {
    void vscode.window.showWarningMessage("No papyrus.toml with [build] sources: nothing to do.");
    return undefined;
  }
  return { dirs, cwd: path.dirname(state.manifest) };
}

/** Saves the modified scripts, which the command line reads from the disk. */
async function saveScripts(): Promise<void> {
  const dirty = vscode.workspace.textDocuments.filter((d) => d.isDirty && d.languageId === "papyrus");
  await Promise.all(dirty.map((d) => d.save()));
}

/** `papyrus_analyzer fmt` on every script of the project. */
export async function formatProject(): Promise<void> {
  const project = projectSources();
  if (project) {
    await saveScripts();
    await runServerTask(["fmt", ...project.dirs], project.cwd, "fmt project");
  }
}

/** `papyrus_analyzer lint` (`--fix`) on every script of the project. */
export async function lintProject(fix: boolean): Promise<void> {
  const project = projectSources();
  if (project) {
    if (fix) {
      await saveScripts();
    }
    const args = fix ? ["lint", "--fix", ...project.dirs] : ["lint", ...project.dirs];
    await runServerTask(args, project.cwd, fix ? "lint --fix project" : "lint project");
  }
}
