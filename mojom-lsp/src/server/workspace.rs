use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::Url as Uri;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use super::document_symbol::DocumentSymbol;
use super::mojomast::check_mojom_text;

#[derive(Debug)]
pub(crate) enum WorkspaceMessage {
    FindSymbol(String, oneshot::Sender<Vec<lsp_types::SymbolInformation>>),
    DidChangeTextDocument(Uri),
    DocumentSymbolsParsed(Uri, ParsedSymbols),
}

#[derive(Debug)]
pub(crate) struct ParsedSymbols {
    #[allow(unused)]
    module_name: Option<String>,
    symbols: Vec<DocumentSymbol>,
}

struct Workspace {
    symbols: HashMap<Uri, ParsedSymbols>,
}

impl Workspace {
    fn new() -> Self {
        Workspace {
            symbols: HashMap::new(),
        }
    }

    async fn find_symbol(
        &self,
        query: String,
        sender: oneshot::Sender<Vec<lsp_types::SymbolInformation>>,
    ) -> anyhow::Result<()> {
        let mut symbols = Vec::new();

        let target_name = match query.split("::").filter(|s| s.len() > 0).last() {
            Some(name) => name,
            None => return Ok(()),
        };

        for (uri, parsed) in self.symbols.iter() {
            for symbol in parsed.symbols.iter() {
                if symbol.name() == target_name {
                    let location =
                        lsp_types::Location::new(uri.clone(), symbol.name_range().clone());
                    #[allow(deprecated)]
                    let symbol = lsp_types::SymbolInformation {
                        name: symbol.name().to_string(),
                        kind: symbol.kind(),
                        location,
                        tags: None,
                        deprecated: None,
                        container_name: None,
                    };
                    symbols.push(symbol);
                }
            }
        }
        sender.send(symbols).unwrap();
        Ok(())
    }

    async fn did_change_text_document(&mut self, uri: Uri) -> anyhow::Result<()> {
        self.symbols.remove(&uri);
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(err) => {
                log::debug!("Failed to convert path: {:?}", err);
                return Ok(());
            }
        };

        let (uri, parsed) = match parse_mojom_symbols(path).await {
            Ok((uri, parsed)) => (uri, parsed),
            Err(_) => {
                // Maybe editing the file.
                return Ok(());
            }
        };
        self.symbols.insert(uri, parsed);
        Ok(())
    }

    async fn document_symbols_parsed(
        &mut self,
        uri: Uri,
        parsed: ParsedSymbols,
    ) -> anyhow::Result<()> {
        self.symbols.insert(uri, parsed);
        Ok(())
    }
}

pub(crate) async fn workspace_task(
    root_path: PathBuf,
    sender: mpsc::Sender<WorkspaceMessage>,
    mut receiver: mpsc::Receiver<WorkspaceMessage>,
) -> anyhow::Result<()> {
    let mut workspace = Workspace::new();

    parse_all_mojom(&root_path, sender)?;

    while let Some(message) = receiver.recv().await {
        match message {
            WorkspaceMessage::FindSymbol(query, sender) => {
                workspace.find_symbol(query, sender).await?
            }
            WorkspaceMessage::DidChangeTextDocument(uri) => {
                workspace.did_change_text_document(uri).await?;
            }
            WorkspaceMessage::DocumentSymbolsParsed(uri, parsed) => {
                workspace.document_symbols_parsed(uri, parsed).await?
            }
        }
    }

    Ok(())
}

fn parse_all_mojom(
    root_path: impl AsRef<Path>,
    sender: mpsc::Sender<WorkspaceMessage>,
) -> anyhow::Result<()> {
    let mut directories = Vec::new();
    directories.push(root_path.as_ref().to_owned());
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(directory)?;
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() && entry.file_name() != "out" {
                directories.push(entry.path());
                continue;
            }
            if file_type.is_file() {
                let path = entry.path();
                if path.extension().map(|ext| ext == "mojom").unwrap_or(false) {
                    let sender = sender.clone();
                    tokio::spawn(check_mojom_file(path, sender));
                }
            }
        }
    }
    Ok(())
}

async fn check_mojom_file(
    path: PathBuf,
    sender: mpsc::Sender<WorkspaceMessage>,
) -> anyhow::Result<()> {
    let (uri, parsed) = parse_mojom_symbols(path).await?;
    sender
        .send(WorkspaceMessage::DocumentSymbolsParsed(uri, parsed))
        .await?;
    Ok(())
}

async fn parse_mojom_symbols(path: PathBuf) -> anyhow::Result<(Uri, ParsedSymbols)> {
    let text = tokio::fs::read_to_string(&path).await?;
    let mut result = check_mojom_text(&text);
    let module_name = result.module_name.take();
    let ast = result.create_ast(path, text)?;
    let symbols = ast.get_document_symbols();
    let uri = ast.uri();
    let parsed = ParsedSymbols {
        module_name,
        symbols,
    };
    Ok((uri, parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_all_mojom() -> anyhow::Result<()> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata");
        let (sender, mut receiver) = mpsc::channel(64);

        parse_all_mojom(&path, sender)?;

        let mut num_parsed = 0;
        while let Some(message) = receiver.recv().await {
            match message {
                WorkspaceMessage::DocumentSymbolsParsed(_, _) => num_parsed += 1,
                _ => unreachable!(),
            }
        }

        assert_eq!(num_parsed, 3);
        Ok(())
    }
}
