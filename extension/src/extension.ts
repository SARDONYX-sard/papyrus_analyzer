import * as vscode from "vscode";

import { compileAll, compileCommand, compileSelected } from "./compile/commands";
import { compileTask, type PapyrusTaskDefinition } from "./compile/task";
import { registerDecompiler } from "./decompile";
import { createManifest } from "./manifest";
import { registerExplorerView } from "./explorerView";
import { restartServer, startServer, stopServer } from "./server";
import { serverPath } from "./settings";
import { state } from "./state";
import { createStatusBar } from "./statusBar";
import { showStatus } from "./statusCommand";
import { showSyntaxTree } from "./syntaxTree";
import { formatProject, lintProject } from "./project";
import { showReferences } from "./references";
import { projectDir, runServerTask, runTests } from "./tests";

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  state.context = context;
  state.output = vscode.window.createOutputChannel("Papyrus Analyzer", { log: true });
  context.subscriptions.push(state.output);
  createStatusBar(context);
  registerDecompiler(context);
  registerExplorerView(context);

  context.subscriptions.push(
    vscode.commands.registerCommand("papyrus.restartServer", restartServer),
    vscode.commands.registerCommand("papyrus.showStatus", showStatus),
    vscode.commands.registerCommand("papyrus.showReferences", showReferences),
    vscode.commands.registerCommand("papyrus.showSyntaxTree", showSyntaxTree),
    vscode.commands.registerCommand("papyrus.compile", compileCommand),
    vscode.commands.registerCommand("papyrus.compileAll", compileAll),
    vscode.commands.registerCommand("papyrus.compileSelected", compileSelected),
    vscode.commands.registerCommand("papyrus.createManifest", createManifest),
    vscode.commands.registerCommand("papyrus.formatProject", formatProject),
    vscode.commands.registerCommand("papyrus.lintProject", () => lintProject(false)),
    vscode.commands.registerCommand("papyrus.fixProject", () => lintProject(true)),
    // The `▶ Run Test` code lens: `papyrus_analyzer run Script.Function` in a
    // terminal, from the folder of the papyrus.toml of the script.
    vscode.commands.registerCommand("papyrus.runTest", async (uri: string, target: string) => {
      await runServerTask(["run", target], projectDir(vscode.Uri.parse(uri).fsPath), `run ${target}`);
    }),
    // The `▶ Run Tests` code lens of test scripts: `papyrus_analyzer test FILE`.
    vscode.commands.registerCommand("papyrus.runTests", async (uri: string) => {
      await runTests([vscode.Uri.parse(uri).fsPath]);
    }),
    // The `▶ Run Doctest` code lens: `papyrus_analyzer test --doc "Script.Function (line 5)"`.
    vscode.commands.registerCommand("papyrus.runDocTest", async (uri: string, name: string) => {
      await runServerTask(["test", "--doc", name], projectDir(vscode.Uri.parse(uri).fsPath), `doc test ${name}`);
    }),
    // The `⚙ Debug` code lens: the test in the debugger of the Papyrus VM.
    vscode.commands.registerCommand("papyrus.debugTest", async (uri: string, target: string) => {
      const folder = vscode.workspace.getWorkspaceFolder(vscode.Uri.parse(uri));
      await vscode.debug.startDebugging(folder, {
        type: "papyrus",
        request: "launch",
        name: `Debug ${target}`,
        test: target,
        cwd: projectDir(vscode.Uri.parse(uri).fsPath),
      });
    }),
    // `papyrus_analyzer debug`: the debug adapter, over stdio.
    vscode.debug.registerDebugAdapterDescriptorFactory("papyrus", {
      createDebugAdapterDescriptor: () => new vscode.DebugAdapterExecutable(serverPath(context), ["debug"]),
    }),
    vscode.tasks.registerTaskProvider("papyrus", {
      provideTasks: () => [compileTask({ type: "papyrus", all: true })],
      resolveTask: (task) => compileTask(task.definition as PapyrusTaskDefinition),
    }),
  );

  await startServer(context);
}

export async function deactivate(): Promise<void> {
  await stopServer();
}
