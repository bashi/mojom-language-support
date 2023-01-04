// Copyright 2020 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(version)]
struct Args {
    /// Specify log level: off, error, warn, info, debug, trace.
    #[arg(long)]
    log_level: Option<log::Level>,
    /// Specify chromium out directory.
    #[arg(long, default_value = "out/Release")]
    out_dir: PathBuf,
    /// Enable clangd for C++ bindings symbol lookup.
    #[arg(long)]
    enable_clangd: bool,
    /// Specify a path to clangd binary.
    #[arg(long, default_value = "clangd")]
    clangd_path: PathBuf,
    /// Specify a path to look for compile_commands.json.
    #[arg(long)]
    clangd_compile_commands_dir: Option<PathBuf>,
    /// Specify clangd log level.
    #[arg(long)]
    clangd_log_level: Option<log::Level>,
}

pub fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize logger.
    {
        let mut builder = env_logger::builder();
        builder.target(env_logger::Target::Stderr).default_format();
        if let Some(level) = args.log_level {
            builder.filter_level(level.to_level_filter());
        }
        builder.init();
    }

    let clangd_params = if args.enable_clangd {
        Some(mojom_lsp::server::ClangdParams {
            clangd_path: args.clangd_path,
            out_dir: args.out_dir,
            compile_commands_dir: args.clangd_compile_commands_dir,
            log_level: args.clangd_log_level,
        })
    } else {
        None
    };

    let rt = tokio::runtime::Runtime::new()?;
    let exit_code = rt.block_on(mojom_lsp::server::run(
        tokio::io::stdin(),
        tokio::io::stdout(),
        clangd_params,
    ))?;
    std::process::exit(exit_code);
}
