/**
 * Copyright 2020 Google LLC
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

import * as child_process from "child_process";

import * as vscode from "vscode";

import {
  LanguageClient,
  LanguageClientOptions,
  Executable,
  State,
  RevealOutputChannelOn,
} from "vscode-languageclient/node";

const DEFAULT_SERVER_COMMAND = "mojom-lsp";

let client: LanguageClient | null = null;
let lspStatusBarItem: vscode.StatusBarItem;
let outputChannel: vscode.OutputChannel;

function startClient(configuration: vscode.WorkspaceConfiguration) {
  const command = getServerPath(configuration);
  const rawArgs = configuration.get<string[]>("languageServerArguments") || [];
  const args = rawArgs.map((arg) => substituteVariableReferences(arg));
  const serverOptions: Executable = {
    command,
    args,
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "mojom" }],
    outputChannel,
    revealOutputChannelOn: RevealOutputChannelOn.Never,
  };

  client = new LanguageClient(
    "mojomLanguageServer",
    "Mojom Language Server",
    serverOptions,
    clientOptions
  );

  client.onDidChangeState((event) => {
    switch (event.newState) {
      case State.Starting:
        lspStatusBarItem.tooltip = "Starting";
        break;
      case State.Running:
        lspStatusBarItem.tooltip = "Running";
        break;
      case State.Stopped:
        lspStatusBarItem.hide();
        if (event.oldState !== State.Running) {
          // Failed to start the server, update the configuration to disable the server.
          const configuration = vscode.workspace.getConfiguration("mojom");
          configuration.update("enableLanguageServer", "Disabled");
        }
        break;
    }
  });
  client.start();

  if (IsMojomTextEditor(vscode.window.activeTextEditor)) {
    lspStatusBarItem.show();
  }
}

async function stopClient(): Promise<void> {
  if (!client) {
    return;
  }

  const result = client.stop();
  client = null;
  return result;
}

function substituteVariableReferences(value: string): string {
  return value.replace(/\$\{(.+)\}/g, (match, capture) => {
    return resolveVariableReference(capture) || match;
  });
}

// Resolves subset of variable references.
// https://code.visualstudio.com/docs/editor/variables-reference
function resolveVariableReference(name: string): string | undefined {
  if (name === "workspaceRoot" && vscode.workspace.workspaceFolders.length > 0) {
    return vscode.workspace.workspaceFolders[0].uri.fsPath;
  }
  if (name.startsWith("env:")) {
    return process.env[name.substring(4)] || "";
  }
  if (name === "userHome") {
    return process.env["HOME"];
  }
  return undefined;
}

async function hasCommand(command: string): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    const checkCommand = process.platform === "win32" ? "where" : "command -v";
    const proc = child_process.exec(`${checkCommand} ${command}`);
    proc.on("exit", (code) => {
      resolve(code === 0);
    });
  });
}

async function isCargoAvaiable(): Promise<boolean> {
  return hasCommand("cargo");
}

async function installServerBinary(): Promise<boolean> {
  const task = new vscode.Task(
    { type: "cargo", task: "install" },
    vscode.workspace.workspaceFolders![0],
    "Installing mojom-lsp",
    "mojom-lsp",
    new vscode.ShellExecution("cargo install mojom-lsp")
  );
  const promise = new Promise<boolean>((resolve) => {
    vscode.tasks.onDidEndTask((e) => {
      if (e.execution.task === task) {
        e.execution.terminate();
      }
    });
    vscode.tasks.onDidEndTaskProcess((e) => {
      resolve(e.exitCode === 0);
    });
  });
  vscode.tasks.executeTask(task);

  return promise;
}

function getServerPath(configuration: vscode.WorkspaceConfiguration): string {
  const rawServerPath = configuration.get<string>("languageServerPath");
  return substituteVariableReferences(rawServerPath);
}

async function tryToInstallLanguageServer(
  configuration: vscode.WorkspaceConfiguration
) {
  const hasCargo = await isCargoAvaiable();
  if (!hasCargo) {
    configuration.update("enableLanguageServer", "Disabled");
    return;
  }

  const message = "Install Mojom Language Server? (Rust toolchain required)";
  const selected = await vscode.window.showInformationMessage(
    message,
    "Yes",
    "No",
    "Never"
  );
  if (selected === "Yes") {
    const installed = await installServerBinary();
    if (installed) {
      startClient(configuration);
    } else {
      configuration.update("enableLanguageServer", "Disabled");
    }
  } else if (selected === "Never") {
    configuration.update("enableLanguageServer", "Never");
  }
}

async function applyConfigurations() {
  const configuration = vscode.workspace.getConfiguration("mojom");
  const serverPath = getServerPath(configuration);
  const enableLanguageServer = configuration.get<string>("enableLanguageServer");
  const shouldStartClient = (enableLanguageServer === "Enabled") && (await hasCommand(serverPath));
  if (shouldStartClient) {
    startClient(configuration);
  } else if (enableLanguageServer === "Enabled" && serverPath === DEFAULT_SERVER_COMMAND) {
    tryToInstallLanguageServer(configuration);
  } else if (enableLanguageServer !== "Enabled") {
    await stopClient();
  }
}

function IsMojomTextEditor(editor: vscode.TextEditor | undefined): boolean {
  return editor && editor.document.languageId === "mojom";
}

export async function activate(context: vscode.ExtensionContext) {
  const subscriptions = context.subscriptions;

  // Set up a status bar item for mojom-lsp.
  lspStatusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left);
  lspStatusBarItem.text = "mojom-lsp";
  lspStatusBarItem.hide();
  subscriptions.push(lspStatusBarItem);

  subscriptions.push(vscode.workspace.onDidChangeConfiguration((event) => {
    if (event.affectsConfiguration("mojom")) {
      applyConfigurations();
    }
  }));
  subscriptions.push(vscode.window.onDidChangeActiveTextEditor((editor) => {
    const shouldShowStatusBarItem = IsMojomTextEditor(editor) && client;
    if (shouldShowStatusBarItem) {
      lspStatusBarItem.show();
    } else {
      lspStatusBarItem.hide();
    }
  }));

  // Register commands.
  subscriptions.push(vscode.commands.registerCommand("mojom.installLanguageServer", async () => {
    installServerBinary();
  }));
  subscriptions.push(vscode.commands.registerCommand("mojom.restartLanguageServer", async () => {
    if (!client) {
      return;
    }
    await stopClient();
    const configuration = vscode.workspace.getConfiguration("mojom");
    startClient(configuration);
  }));

  outputChannel = vscode.window.createOutputChannel("Mojom Language Server");
  subscriptions.push(outputChannel);

  applyConfigurations();
}

export function deactivate(): Thenable<void> | undefined {
  if (!client) {
    return undefined;
  }
  return stopClient();
}
