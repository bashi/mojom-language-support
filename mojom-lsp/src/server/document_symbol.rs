#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InterfaceSymbol {
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MethodSymbol {
    pub(crate) name: String,
    pub(crate) interface_name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StructSymbol {
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UnionSymbol {
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EnumSymbol {
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ConstSymbol {
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DocumentSymbol {
    Method(MethodSymbol),
    Interface(InterfaceSymbol),
    Struct(StructSymbol),
    Union(UnionSymbol),
    Enum(EnumSymbol),
    Const(ConstSymbol),
}

impl DocumentSymbol {
    pub(crate) fn name(&self) -> &str {
        match self {
            DocumentSymbol::Method(s) => &s.name,
            DocumentSymbol::Interface(s) => &s.name,
            DocumentSymbol::Struct(s) => &s.name,
            DocumentSymbol::Union(s) => &s.name,
            DocumentSymbol::Enum(s) => &s.name,
            DocumentSymbol::Const(s) => &s.name,
        }
    }

    pub(crate) fn range(&self) -> &lsp_types::Range {
        match self {
            DocumentSymbol::Method(s) => &s.range,
            DocumentSymbol::Interface(s) => &s.range,
            DocumentSymbol::Struct(s) => &s.range,
            DocumentSymbol::Union(s) => &s.range,
            DocumentSymbol::Enum(s) => &s.range,
            DocumentSymbol::Const(s) => &s.range,
        }
    }

    pub(crate) fn kind(&self) -> lsp_types::SymbolKind {
        match self {
            DocumentSymbol::Method(_) => lsp_types::SymbolKind::METHOD,
            DocumentSymbol::Interface(_) => lsp_types::SymbolKind::INTERFACE,
            DocumentSymbol::Struct(_) => lsp_types::SymbolKind::STRUCT,
            DocumentSymbol::Union(_) => lsp_types::SymbolKind::STRUCT,
            DocumentSymbol::Enum(_) => lsp_types::SymbolKind::ENUM,
            DocumentSymbol::Const(_) => lsp_types::SymbolKind::CONSTANT,
        }
    }

    pub(crate) fn to_proxy_symbol(self) -> DocumentSymbol {
        match self {
            DocumentSymbol::Interface(s) => DocumentSymbol::Interface(InterfaceSymbol {
                name: s.name + "Proxy",
                range: s.range,
            }),
            DocumentSymbol::Method(s) => DocumentSymbol::Method(MethodSymbol {
                name: s.name,
                interface_name: s.interface_name + "Proxy",
                range: s.range,
            }),
            _ => self,
        }
    }
}
