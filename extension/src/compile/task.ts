import * as cp from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as vscode from "vscode";

import { type CompileCommand, CompileCommandsRequest } from "../protocol";
import { runningClient, state } from "../state";

/** Task definition of `papyrus` tasks, see `taskDefinitions` in package.json. */
export interface PapyrusTaskDefinition extends vscode.TaskDefinition {
  all?: boolean;
  files?: string[];
}

/** The state of one script while a compile task runs. */
export interface CompileEvent {
  script: string;
  status: "running" | "passed" | "failed";
}

/** Fired for every script a compile task runs, for the scripts view. */
export const compileEvents = new vscode.EventEmitter<CompileEvent>();

/** The scripts a command compiles (servers before `scripts` sent one per command). */
function scriptsOf(command: CompileCommand): string[] {
  if (command.scripts?.length) {
    return command.scripts;
  }
  return [command.args.find((a) => a.toLowerCase().endsWith(".psc")) ?? command.args[0]];
}

/** A path compared as Windows does: case and slashes do not matter. */
function pathKey(file: string): string {
  return path.resolve(file).replace(/\\/g, "/").toLowerCase();
}

/**
 * The scripts of a command that failed. A command compiling one script failed
 * when it exits with an error; one compiling several (the built-in compiler)
 * names its failures: `No output generated for <script>, compilation failed.`
 * or `<script>: cannot read the file`.
 */
function failedScripts(scripts: string[], code: number, output: string): Set<string> {
  if (code === 0) {
    return new Set();
  }
  if (scripts.length === 1) {
    return new Set(scripts);
  }
  const named = new Set<string>();
  const pattern = /^(?:No output generated for (.+), compilation failed\.|(.+\.psc): cannot read the file)\s*$/gim;
  for (const match of output.matchAll(pattern)) {
    named.add(pathKey(match[1] ?? match[2]));
  }
  const failed = scripts.filter((s) => named.has(pathKey(s)));
  // An error no script is named in (an output that cannot be written): all of them.
  return new Set(failed.length > 0 ? failed : scripts);
}

/** Saves the open documents and runs a compile task. */
export async function runCompile(definition: PapyrusTaskDefinition): Promise<void> {
  // Compile what is on screen, not the last saved version.
  await vscode.workspace.saveAll(false);
  await vscode.tasks.executeTask(compileTask(definition));
}

/** A task running the compiler commands of `papyrus.toml` in a pseudo terminal. */
export function compileTask(definition: PapyrusTaskDefinition): vscode.Task {
  const files = definition.files ?? [];
  const name = definition.all
    ? "compile all"
    : files.length === 1
      ? `compile ${path.basename(files[0])}`
      : `compile ${files.length} scripts`;
  const execution = new vscode.CustomExecution(async () => new CompileTerminal(definition));
  const task = new vscode.Task(definition, vscode.TaskScope.Workspace, name, "papyrus", execution, ["$papyrus"]);
  task.group = vscode.TaskGroup.Build;
  task.presentationOptions = { reveal: vscode.TaskRevealKind.Always, clear: true };
  return task;
}

/** `path(line,col): message`, as both compilers print their errors. */
const DIAGNOSTIC = /^(.+?\.psc)\(\d+,\d+\):.*$/i;

/**
 * Why `script` failed, from the output of its command: its errors
 * (`path(line,col): message`, which the `$papyrus` problem matcher turns into
 * diagnostics), else the end of the output of a command compiling it alone.
 */
function causes(script: string, scripts: string[], output: string): string[] {
  const lines = output.split(/\r?\n/).filter((l) => l.trim() !== "");
  const key = pathKey(script);
  const own = lines.filter((l) => {
    const m = DIAGNOSTIC.exec(l) ?? /^(.+?\.psc): cannot read the file/i.exec(l);
    return m !== null && pathKey(m[1]) === key;
  });
  if (own.length > 0 || scripts.length > 1) {
    return own;
  }
  const errors = lines.filter((l) => DIAGNOSTIC.test(l));
  return errors.length > 0 ? errors : lines.slice(-5);
}

/** A failed script and why. */
interface Failure {
  script: string;
  code: number;
  causes: string[];
}

/**
 * Runs the compiler commands: one per script for an external compiler (it
 * cannot compile several files at once), one per directory for the built-in
 * one. Like `cargo nextest`: a progress line while they run, then a summary
 * and each failure with its errors. The commands and their whole output go to
 * the Papyrus Analyzer output channel.
 */
class CompileTerminal implements vscode.Pseudoterminal {
  private readonly writeEmitter = new vscode.EventEmitter<string>();
  private readonly closeEmitter = new vscode.EventEmitter<number>();
  private readonly children = new Set<cp.ChildProcess>();
  private cancelled = false;
  readonly onDidWrite = this.writeEmitter.event;
  readonly onDidClose = this.closeEmitter.event;

  constructor(private readonly definition: PapyrusTaskDefinition) {}

  open(): void {
    void this.run().then(
      (code) => this.closeEmitter.fire(code),
      (error: unknown) => {
        this.writeLine(`\x1b[31m${error instanceof Error ? error.message : String(error)}\x1b[0m`);
        this.closeEmitter.fire(1);
      },
    );
  }

  close(): void {
    this.cancelled = true;
    for (const child of this.children) {
      child.kill();
    }
  }

  private writeLine(line: string): void {
    this.writeEmitter.fire(line.replace(/\r?\n/g, "\r\n") + "\r\n");
  }

  private async run(): Promise<number> {
    const client = runningClient();
    if (!client) {
      throw new Error("The Papyrus language server is not running.");
    }
    const params = {
      all: !!this.definition.all,
      files: (this.definition.files ?? []).map((f) => ({ uri: vscode.Uri.file(f).toString() })),
    };
    const plan = await client.sendRequest(CompileCommandsRequest, params);
    fs.mkdirSync(plan.output, { recursive: true });

    const started = Date.now();
    const total = plan.commands.reduce((n, c) => n + scriptsOf(c).length, 0);
    let done = 0;
    // By command, so that the summary follows the order of the scripts.
    const failuresOf: Failure[][] = plan.commands.map(() => []);
    // Several commands at a time: a compiler process spends most of its time
    // loading the imports on one thread (the built-in one then compiles on
    // all cores).
    const workers = Math.max(1, Math.min(plan.commands.length, Math.floor(os.availableParallelism() / 2)));
    let next = 0;
    const worker = async (): Promise<void> => {
      while (next < plan.commands.length && !this.cancelled) {
        const index = next++;
        const command = plan.commands[index];
        // The compiler does not create nested output directories itself.
        fs.mkdirSync(command.output, { recursive: true });
        const scripts = scriptsOf(command);
        for (const script of scripts) {
          compileEvents.fire({ script, status: "running" });
        }
        // The compiler exits with -1, which Node reports as an unsigned value.
        const { code: raw, output } = await this.spawn(command);
        const code = raw > 0x7fffffff ? raw - 0x100000000 : raw;
        state.output?.info(`> ${command.commandLine}${output.trim() === "" ? "" : `\n${output.trimEnd()}`}`);
        const failed = failedScripts(scripts, code, output);
        for (const script of scripts) {
          const ok = !failed.has(script);
          compileEvents.fire({ script, status: ok ? "passed" : "failed" });
          if (!ok) {
            failuresOf[index].push({ script, code: code === 0 ? 1 : code, causes: causes(script, scripts, output) });
          }
        }
        done += scripts.length;
        this.progress(done, total, failuresOf.flat().length);
      }
    };
    this.progress(0, total, 0);
    await Promise.all(Array.from({ length: workers }, worker));
    this.writeEmitter.fire("\r\x1b[K");
    if (this.cancelled) {
      return 130;
    }
    const failures = failuresOf.flat();
    this.summary(total, failures, Date.now() - started, plan.output);
    if (failures.length > 0) {
      void vscode.window.showErrorMessage(`Papyrus: ${failures.length} of ${total} scripts failed to compile.`);
    }
    return failures.length === 0 ? 0 : 1;
  }

  /** The progress line, rewritten in place. */
  private progress(done: number, total: number, failed: number): void {
    const failures = failed === 0 ? "" : ` \x1b[31m${failed} failed\x1b[0m`;
    this.writeEmitter.fire(`\r\x1b[K\x1b[36m   Compiling\x1b[0m [${done}/${total}]${failures}`);
  }

  /**
   * The summary, then each failure: `FAIL script` and its errors, which start
   * their lines so that the problem matcher finds them.
   */
  private summary(total: number, failures: Failure[], ms: number, output: string): void {
    const time = `[${(ms / 1000).toFixed(2).padStart(7)}s]`;
    const passed = total - failures.length;
    const counts =
      failures.length === 0
        ? `\x1b[32m${passed} passed\x1b[0m`
        : `${passed} passed, \x1b[31m${failures.length} failed\x1b[0m`;
    this.writeLine("────────────");
    this.writeLine(`\x1b[1m     Summary\x1b[0m ${time} ${total} scripts compiled: ${counts}`);
    if (failures.length === 0) {
      this.writeLine(`\x1b[2m             into ${output}\x1b[0m`);
      return;
    }
    for (const failure of failures) {
      const name = vscode.workspace.asRelativePath(failure.script);
      this.writeLine(`\x1b[31m        FAIL\x1b[0m ${name}${failure.code === 1 ? "" : ` (exit ${failure.code})`}`);
      for (const cause of failure.causes) {
        this.writeLine(cause);
      }
    }
    this.writeLine("\x1b[2mThe commands and their output: Output > Papyrus Analyzer.\x1b[0m");
  }

  /** Runs a command; resolves to its exit code and output, not shown. */
  private spawn(command: CompileCommand): Promise<{ code: number; output: string }> {
    return new Promise((resolve) => {
      if (!fs.existsSync(command.program)) {
        this.writeLine(`\x1b[31mcompiler not found: ${command.program} (set [compiler] path)\x1b[0m`);
        resolve({ code: 1, output: "" });
        return;
      }
      const child = cp.spawn(command.program, command.args, { cwd: command.cwd, windowsHide: true });
      this.children.add(child);
      let output = "";
      const keep = (data: Buffer) => {
        output += data.toString();
      };
      child.stdout?.on("data", keep);
      child.stderr?.on("data", keep);
      child.on("error", (error) => {
        this.children.delete(child);
        resolve({ code: 1, output: `${output}\n${error.message}` });
      });
      child.on("close", (code) => {
        this.children.delete(child);
        resolve({ code: code ?? 1, output });
      });
    });
  }
}
