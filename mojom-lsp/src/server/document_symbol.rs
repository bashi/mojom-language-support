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
}
