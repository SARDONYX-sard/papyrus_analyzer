import { NotificationType0, RequestType, RequestType0 } from "vscode-languageclient/node";

/** Result of the `papyrus/status` and `papyrus/restart` requests. */
export interface ServerStatus {
  files: number;
  openFiles: number;
  indexing: boolean;
  /** The check of the scripts not open, while it runs. */
  checking?: { done: number; total: number; current: string; remainingMs?: number | null } | null;
  generation: number;
  roots: string[];
  importDirectories: string[];
  health?: "ok" | "limited" | "warning" | "error";
  problems?: string[];
  manifest?: string | null;
  manifestError?: string | null;
  sources?: string[];
  testSources?: string[];
  buildDirs?: BuildDirs[];
  flagsFile?: string | null;
  variables?: Record<string, string>;
}

/** Where a workspace folder builds: `.pex` files and decompiled scripts. */
export interface BuildDirs {
  root: string;
  compile: string;
  decompile: string;
  /** `[compiler] path`; absent or `null` for the built-in compiler. */
  compiler?: string | null;
}

/** One compiler invocation of `papyrus/compileCommands`. */
export interface CompileCommand {
  program: string;
  args: string[];
  cwd: string;
  output: string;
  commandLine: string;
}

/** Result of `papyrus/compileCommands`. */
export interface CompileCommands {
  manifest: string;
  output: string;
  commands: CompileCommand[];
}

/**
 * `papyrus/restart`: the server drops its indexes, applies `settings`,
 * re-indexes the workspace and republishes diagnostics. Implemented on the
 * server side because an activated extension cannot restart itself.
 */
export const RestartRequest = new RequestType<{ settings?: unknown }, ServerStatus, void>("papyrus/restart");

/** `papyrus/status`: what the server currently knows. */
export const StatusRequest = new RequestType0<ServerStatus, void>("papyrus/status");

/** `papyrus/statusChanged`: the projects changed; ask `papyrus/status` again. */
export const StatusChangedNotification = new NotificationType0("papyrus/statusChanged");

/** `papyrus/syntaxTree`: a debug dump of the syntax tree of a document. */
export const SyntaxTreeRequest = new RequestType<{ textDocument: { uri: string } }, string, void>(
  "papyrus/syntaxTree",
);

/** `papyrus/compileCommands`: the compiler invocations of `papyrus.toml`. */
export const CompileCommandsRequest = new RequestType<
  { textDocument?: { uri: string }; files?: { uri: string }[]; all: boolean },
  CompileCommands,
  void
>("papyrus/compileCommands");

/** `papyrus/manifestTemplate`: a `papyrus.toml` to start from. */
export const ManifestTemplateRequest = new RequestType0<string, void>("papyrus/manifestTemplate");

/** `papyrus/decompile`: the source of a `.pex` file, or its assembly. */
export const DecompileRequest = new RequestType<{ uri: string; assembly?: boolean }, string, void>(
  "papyrus/decompile",
);

/** Result of `papyrus/decompileFiles`. */
export interface DecompileFilesResult {
  output: string;
  results: { file: string; psc?: string | null; error?: string | null }[];
}

/**
 * `papyrus/decompileFiles`: writes the source of `.pex` files below the
 * decompile directory, keeping their path relative to their workspace folder.
 */
export const DecompileFilesRequest = new RequestType<{ files: string[] }, DecompileFilesResult, void>(
  "papyrus/decompileFiles",
);
