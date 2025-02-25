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

use pest::{Parser, Position, Span};

use super::parser::{consume_token, MojomParser, Pair, Pairs, Rule};

#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    pub start: usize,
    pub end: usize,
}

impl Range {
    pub fn contains(&self, offset: usize) -> bool {
        self.start <= offset && self.end > offset
    }
}

impl<'a> From<Span<'a>> for Range {
    fn from(span: Span<'a>) -> Range {
        Range {
            start: span.start(),
            end: span.end(),
        }
    }
}

// Skips attribute list if exists. This is tentative.
fn skip_attribute_list(pairs: &mut Pairs) {
    match pairs.peek().unwrap().as_rule() {
        Rule::attribute_section => {
            pairs.next();
        }
        _ => (),
    }
}

fn consume_semicolon(pairs: &mut Pairs) {
    match pairs.next().unwrap().as_rule() {
        Rule::t_semicolon => (),
        _ => unreachable!(),
    };
}

fn consume_as_range(pairs: &mut Pairs) -> Range {
    pairs.next().unwrap().as_span().into()
}

fn consume_as_type(pairs: &mut Pairs) -> Range {
    pairs
        .next()
        .unwrap()
        .into_inner()
        .next()
        .unwrap()
        .as_span()
        .into()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub range: Range,
    pub name: Range,
}

fn into_module(pair: Pair) -> Module {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    consume_token(Rule::t_module, &mut pairs);
    let name = consume_as_range(&mut pairs);
    consume_semicolon(&mut pairs);
    Module { range, name }
}

#[derive(Debug, PartialEq)]
pub struct Import {
    pub range: Range,
    pub path: Range,
}

fn into_import(pair: Pair) -> Import {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    consume_token(Rule::t_import, &mut pairs);
    let path = consume_as_range(&mut pairs);
    consume_semicolon(&mut pairs);
    Import { range, path }
}

#[derive(Debug, PartialEq)]
pub struct Const {
    pub range: Range,
    pub typ: Range,
    pub name: Range,
    pub value: Range,
}

fn into_const(pair: Pair) -> Const {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    consume_token(Rule::t_const, &mut pairs);
    let typ = consume_as_type(&mut pairs);
    let name = consume_as_range(&mut pairs);
    consume_token(Rule::t_equal, &mut pairs);
    let value = consume_as_range(&mut pairs);
    consume_semicolon(&mut pairs);
    Const {
        range,
        typ,
        name,
        value,
    }
}

#[derive(Debug, PartialEq)]
pub struct EnumValue {
    pub range: Range,
    pub name: Range,
    pub value: Option<Range>,
}

fn into_enum_value(pair: Pair) -> EnumValue {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    let name = consume_as_range(&mut pairs);
    // The next item should be t_equal when it's Some(item).
    if let Some(item) = pairs.next() {
        assert!(item.as_rule() == Rule::t_equal);
    }
    let value = pairs.next().map(|item| item.as_span().into());
    EnumValue { range, name, value }
}

#[derive(Debug, PartialEq)]
pub struct Enum {
    pub range: Range,
    pub name: Range,
    pub values: Vec<EnumValue>,
}

fn into_enum(pair: Pair) -> Enum {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    let name = consume_as_range(&mut pairs);
    let mut values = Vec::new();
    for item in pairs {
        match item.as_rule() {
            Rule::enum_block => {
                let mut pairs = item.into_inner();
                consume_token(Rule::t_lbrace, &mut pairs);
                for item in pairs {
                    let value = match item.as_rule() {
                        Rule::enum_value => into_enum_value(item),
                        Rule::t_comma => continue,
                        Rule::t_rbrace => break,
                        _ => unreachable!(),
                    };
                    values.push(value);
                }
            }
            Rule::t_semicolon => break,
            _ => unreachable!(),
        }
    }
    Enum {
        range,
        name,
        values,
    }
}

#[derive(Debug, PartialEq)]
pub struct StructField {
    pub range: Range,
    pub typ: Range,
    pub name: Range,
    pub ordinal: Option<Range>,
    pub default: Option<Range>,
}

fn into_struct_field(pair: Pair) -> StructField {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    let typ = consume_as_type(&mut pairs);
    let name = consume_as_range(&mut pairs);
    let mut res = StructField {
        range,
        typ,
        name,
        ordinal: None,
        default: None,
    };
    for item in pairs {
        match item.as_rule() {
            Rule::ordinal_value => res.ordinal = Some(item.as_span().into()),
            Rule::default => {
                let mut pairs = item.into_inner();
                consume_token(Rule::t_equal, &mut pairs);
                res.default = Some(pairs.next().unwrap().as_span().into());
            }
            Rule::t_semicolon => break,
            _ => unreachable!(),
        }
    }
    res
}

#[derive(Debug, PartialEq)]
pub enum StructBody {
    Const(Const),
    Enum(Enum),
    Field(StructField),
}

#[derive(Debug, PartialEq)]
pub struct Struct {
    pub range: Range,
    pub name: Range,
    pub members: Vec<StructBody>,
}

fn into_struct_members(pair: Pair) -> Vec<StructBody> {
    let mut pairs = pair.into_inner();
    consume_token(Rule::t_lbrace, &mut pairs);
    let mut members = Vec::new();
    for item in pairs {
        if item.as_rule() == Rule::t_rbrace {
            break;
        }
        // At this point `item` should have only one inner and it should be struct_item.
        let struct_item = item.into_inner().next().unwrap();
        let member = match struct_item.as_rule() {
            Rule::const_stmt => StructBody::Const(into_const(struct_item)),
            Rule::enum_stmt => StructBody::Enum(into_enum(struct_item)),
            Rule::struct_field => StructBody::Field(into_struct_field(struct_item)),
            _ => unreachable!(),
        };
        members.push(member);
    }
    members
}

fn into_struct(pair: Pair) -> Struct {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    consume_token(Rule::t_struct, &mut pairs);
    let name = consume_as_range(&mut pairs);
    let item = pairs.next().unwrap();
    match item.as_rule() {
        Rule::t_semicolon => {
            return Struct {
                range,
                name,
                members: Vec::new(),
            };
        }
        Rule::struct_body => {
            let members = into_struct_members(item);
            consume_semicolon(&mut pairs);
            return Struct {
                range,
                name,
                members,
            };
        }
        _ => unreachable!(),
    }
}

#[derive(Debug, PartialEq)]
pub struct UnionField {
    pub range: Range,
    pub typ: Range,
    pub name: Range,
    pub ordinal: Option<Range>,
}

fn into_union_field(pair: Pair) -> UnionField {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    let typ = consume_as_type(&mut pairs);
    let name = consume_as_range(&mut pairs);
    let mut ordinal = None;
    for item in pairs {
        match item.as_rule() {
            Rule::ordinal_value => ordinal = Some(item.as_span().into()),
            Rule::t_semicolon => break,
            _ => unreachable!(),
        }
    }
    UnionField {
        range,
        typ,
        name,
        ordinal,
    }
}

#[derive(Debug, PartialEq)]
pub struct Union {
    pub range: Range,
    pub name: Range,
    pub fields: Vec<UnionField>,
}

fn into_union(pair: Pair) -> Union {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    consume_token(Rule::t_union, &mut pairs);
    let name = consume_as_range(&mut pairs);
    consume_token(Rule::t_lbrace, &mut pairs);
    let mut fields = Vec::new();
    loop {
        let item = pairs.next().unwrap();
        let item = match item.as_rule() {
            Rule::union_field => into_union_field(item),
            Rule::t_rbrace => break,
            _ => unreachable!(),
        };
        fields.push(item);
    }
    consume_semicolon(&mut pairs);
    Union {
        range,
        name,
        fields,
    }
}

#[derive(Debug, PartialEq)]
pub struct Parameter {
    pub range: Range,
    pub typ: Range,
    pub name: Range,
    pub ordinal: Option<Range>,
}

fn into_parameter(pair: Pair) -> Parameter {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    let typ = consume_as_type(&mut pairs);
    let name = consume_as_range(&mut pairs);
    let ordinal = pairs.next().map(|ord| ord.as_span().into());
    Parameter {
        range,
        typ,
        name,
        ordinal,
    }
}

fn into_parameter_list(pair: Pair) -> Vec<Parameter> {
    let mut pairs = pair.into_inner();
    consume_token(Rule::t_lparen, &mut pairs);
    let mut params = Vec::new();
    for item in pairs {
        let param = match item.as_rule() {
            Rule::parameter => into_parameter(item),
            Rule::t_comma => continue,
            Rule::t_rparen => break,
            _ => unreachable!(),
        };
        params.push(param);
    }
    params
}

#[derive(Debug, PartialEq)]
pub struct Response {
    pub range: Range,
    pub params: Vec<Parameter>,
}

fn into_response(pair: Pair) -> Response {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    consume_token(Rule::t_arrow, &mut pairs);
    let params = into_parameter_list(pairs.next().unwrap());
    Response { range, params }
}

#[derive(Debug, PartialEq)]
pub struct Method {
    pub range: Range,
    pub name: Range,
    pub ordinal: Option<Range>,
    pub params: Vec<Parameter>,
    pub response: Option<Response>,
}

fn into_method(pair: Pair) -> Method {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    let name = consume_as_range(&mut pairs);
    let ordinal = match pairs.peek().unwrap().as_rule() {
        Rule::ordinal_value => pairs.next().map(|ord| ord.as_span().into()),
        _ => None,
    };
    let params = into_parameter_list(pairs.next().unwrap());
    let mut response = None;
    for item in pairs {
        match item.as_rule() {
            Rule::response => response = Some(into_response(item)),
            Rule::t_semicolon => break,
            _ => unreachable!(),
        }
    }
    Method {
        range,
        name,
        ordinal,
        params,
        response,
    }
}

#[derive(Debug, PartialEq)]
pub enum InterfaceMember {
    Const(Const),
    Enum(Enum),
    Method(Method),
}

fn into_interface_member(pair: Pair) -> InterfaceMember {
    let mut pairs = pair.into_inner();
    let member = pairs.next().unwrap();
    match member.as_rule() {
        Rule::const_stmt => InterfaceMember::Const(into_const(member)),
        Rule::enum_stmt => InterfaceMember::Enum(into_enum(member)),
        Rule::method_stmt => InterfaceMember::Method(into_method(member)),
        _ => unreachable!(),
    }
}

#[derive(Debug, PartialEq)]
pub struct Interface {
    pub range: Range,
    pub name: Range,
    pub members: Vec<InterfaceMember>,
}

fn into_interface(pair: Pair) -> Interface {
    let range = pair.as_span().into();
    let mut pairs = pair.into_inner();
    skip_attribute_list(&mut pairs);
    consume_token(Rule::t_interface, &mut pairs);
    let name = consume_as_range(&mut pairs);
    consume_token(Rule::t_lbrace, &mut pairs);
    let mut members = Vec::new();
    // `for` takes the ownership of `pairs`. Use `loop`.
    loop {
        let item = pairs.next().unwrap(); // Should not be None.
        match item.as_rule() {
            Rule::interface_body => {
                let member = into_interface_member(item);
                members.push(member);
            }
            Rule::t_rbrace => break,
            _ => unreachable!(),
        }
    }
    consume_semicolon(&mut pairs);
    Interface {
        range,
        name,
        members,
    }
}

#[derive(Debug, PartialEq)]
pub enum Statement {
    Module(Module),
    Import(Import),
    Interface(Interface),
    Struct(Struct),
    Union(Union),
    Enum(Enum),
    Const(Const),
}

fn into_statement(pair: Pair) -> Statement {
    // We don't need range information for statements.
    let mut pairs = pair.into_inner();
    let stmt = pairs.next().unwrap();
    match stmt.as_rule() {
        Rule::module_stmt => Statement::Module(into_module(stmt)),
        Rule::import_stmt => Statement::Import(into_import(stmt)),
        Rule::interface => Statement::Interface(into_interface(stmt)),
        Rule::struct_stmt => Statement::Struct(into_struct(stmt)),
        Rule::union_stmt => Statement::Union(into_union(stmt)),
        Rule::enum_stmt => Statement::Enum(into_enum(stmt)),
        Rule::const_stmt => Statement::Const(into_const(stmt)),
        _ => unreachable!(),
    }
}

#[derive(Debug, PartialEq)]
pub struct MojomFile {
    pub stmts: Vec<Statement>,
}

fn into_mojom_file(pair: Pair) -> MojomFile {
    let pairs = pair.into_inner();
    let mut stmts = Vec::new();
    for stmt in pairs {
        let stmt = match stmt.as_rule() {
            Rule::statement => into_statement(stmt),
            Rule::EOI => break,
            _ => unreachable!(),
        };
        stmts.push(stmt);
    }
    MojomFile { stmts: stmts }
}

/// Zero-based line/column in a text.
#[derive(Debug)]
pub struct LineCol {
    pub line: usize,
    pub col: usize,
}

type PestError = pest::error::Error<Rule>;

/// Represents a syntax error.
#[derive(Debug)]
pub struct SyntaxError<'a> {
    input: &'a str,
    pest_err: PestError,
    span: (usize, usize),
}

impl<'a> SyntaxError<'a> {
    /// Returns `start` and `end` positions of the error.
    pub fn range(&self) -> (LineCol, LineCol) {
        let (start, end) = self.span;
        let start = line_col(&self.input, start).unwrap();
        let end = line_col(&self.input, end).unwrap();
        (start, end)
    }

    pub fn lsp_range(&self) -> lsp_types::Range {
        let (start, end) = self.range();
        lsp_types::Range {
            start: lsp_types::Position::new(start.line as u32, start.col as u32),
            end: lsp_types::Position::new(end.line as u32, end.col as u32),
        }
    }

    pub fn lsp_diagnostic(&self) -> lsp_types::Diagnostic {
        lsp_types::Diagnostic {
            range: self.lsp_range(),
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            code: Some(lsp_types::NumberOrString::String("mojom".to_string())),
            source: Some("mojom".to_string()),
            message: self.to_string(),
            ..Default::default()
        }
    }
}

impl<'a> std::fmt::Display for SyntaxError<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.pest_err)
    }
}

fn find_token_end_position(input: &str, start: usize) -> usize {
    let mut end = start;
    for ch in input[start..].chars() {
        if ch == ' ' || ch == '\r' || ch == '\n' {
            break;
        }
        end += 1;
    }

    if end > input.len() {
        end = input.len();
    }
    end
}

impl<'a> SyntaxError<'a> {
    fn new(input: &str, err: PestError) -> SyntaxError {
        let span = match &err.location {
            pest::error::InputLocation::Pos(start) => {
                let end = find_token_end_position(&input, *start);
                (*start, end)
            }
            pest::error::InputLocation::Span((start, end)) => (*start, *end),
        };
        SyntaxError {
            input: input,
            pest_err: err,
            span: span,
        }
    }
}

fn parse_input(input: &str) -> Result<Pairs, PestError> {
    MojomParser::parse(Rule::mojom_file, input).map_err(|err| {
        err.renamed_rules(|rule| match rule {
            Rule::EOI => "'End of File'".to_owned(),
            Rule::mojom_file => "statement".to_owned(),
            Rule::t_array => "array".to_owned(),
            Rule::t_associated => "associated".to_owned(),
            Rule::t_const => "const".to_owned(),
            Rule::t_handle => "handle".to_owned(),
            Rule::t_import => "import".to_owned(),
            Rule::t_interface => "interface".to_owned(),
            Rule::t_map => "map".to_owned(),
            Rule::t_module => "module".to_owned(),
            Rule::t_struct => "struct".to_owned(),
            Rule::t_union => "union".to_owned(),
            Rule::t_amp => "'&'".to_owned(),
            Rule::t_arrow => "'=>'".to_owned(),
            Rule::t_comma => "','".to_owned(),
            Rule::t_equal => "'='".to_owned(),
            Rule::t_langlebracket => "'<'".to_owned(),
            Rule::t_lbrace => "'{'".to_owned(),
            Rule::t_lbracket => "'['".to_owned(),
            Rule::t_lparen => "'('".to_owned(),
            Rule::t_nullable => "'?'".to_owned(),
            Rule::t_ranglebracket => "'>'".to_owned(),
            Rule::t_rbrace => "'}'".to_owned(),
            Rule::t_rbracket => "']'".to_owned(),
            Rule::t_rparen => "')'".to_owned(),
            Rule::t_semicolon => "';'".to_owned(),
            _ => format!("{:?}", rule),
        })
    })
}

fn build_syntax_tree(mut pairs: Pairs) -> MojomFile {
    let inner = pairs.next().unwrap();
    into_mojom_file(inner)
}

/// Parses `input` into a syntax tree.
pub fn parse(input: &str) -> Result<MojomFile, SyntaxError> {
    let pairs = parse_input(input).map_err(|err| SyntaxError::new(input, err))?;
    let mojom = build_syntax_tree(pairs);
    Ok(mojom)
}

/// Converts `offset` to LineCol in `text`.
pub fn line_col(text: &str, offset: usize) -> Option<LineCol> {
    Position::new(text, offset)
        .map(|p| p.line_col())
        .map(|(line, col)| LineCol {
            line: line - 1,
            col: col - 1,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial_text<'t>(text: &'t str, range: &Range) -> &'t str {
        &text[range.start..range.end]
    }

    #[test]
    fn test_comment() {
        let input = "/* block comment */";
        let parsed = MojomParser::parse(Rule::mojom_file, &input);
        assert!(parsed.is_ok());

        let input = "// line comment";
        let parsed = MojomParser::parse(Rule::mojom_file, &input);
        assert!(parsed.is_ok());
    }

    fn parse_part(r: Rule, i: &str) -> &str {
        MojomParser::parse(r, i).unwrap().as_str()
    }

    #[test]
    fn test_integer() {
        assert_eq!("0", parse_part(Rule::integer, "0"));
        assert_eq!("123", parse_part(Rule::integer, "123"));
        assert_eq!("-42", parse_part(Rule::integer, "-42"));
        assert_eq!("0xdeadbeef", parse_part(Rule::integer, "0xdeadbeef"));
        assert_eq!("+0X1AB4", parse_part(Rule::integer, "+0X1AB4"));
    }

    #[test]
    fn test_float() {
        assert_eq!("0.0", parse_part(Rule::float, "0.0"));
        assert_eq!("1.0", parse_part(Rule::float, "1.0"));
        assert_eq!("3.141", parse_part(Rule::float, "3.141"));
        assert_eq!("+0.123", parse_part(Rule::float, "+0.123"));
        assert_eq!("-5.67", parse_part(Rule::float, "-5.67"));
        assert_eq!("4e5", parse_part(Rule::float, "4e5"));
        assert_eq!("-7e+15", parse_part(Rule::float, "-7e+15"));
        assert_eq!("+9e-2", parse_part(Rule::float, "+9e-2"));
    }

    #[test]
    fn test_number() {
        assert_eq!("0", parse_part(Rule::number, "0"));
        assert_eq!("123", parse_part(Rule::number, "123"));
        assert_eq!("-42", parse_part(Rule::number, "-42"));
        assert_eq!("0xdeadbeef", parse_part(Rule::number, "0xdeadbeef"));
        assert_eq!("+0X1AB4", parse_part(Rule::number, "+0X1AB4"));

        assert_eq!("0.0", parse_part(Rule::number, "0.0"));
        assert_eq!("1.0", parse_part(Rule::number, "1.0"));
        assert_eq!("3.141", parse_part(Rule::number, "3.141"));
        assert_eq!("+0.123", parse_part(Rule::number, "+0.123"));
        assert_eq!("-5.67", parse_part(Rule::number, "-5.67"));
        assert_eq!("4e5", parse_part(Rule::number, "4e5"));
        assert_eq!("-7e+15", parse_part(Rule::number, "-7e+15"));
        assert_eq!("+9e-2", parse_part(Rule::number, "+9e-2"));
    }

    #[test]
    fn test_string_literal() {
        assert_eq!(r#""hello""#, parse_part(Rule::string_literal, r#""hello""#));
        assert_eq!(
            r#""hell\"o""#,
            parse_part(Rule::string_literal, r#""hell\"o""#)
        );
    }

    #[test]
    fn test_literal() {
        assert_eq!("true", parse_part(Rule::literal, "true"));
        assert_eq!("false", parse_part(Rule::literal, "false"));
        assert_eq!("default", parse_part(Rule::literal, "default"));
        assert_eq!("0x12ab", parse_part(Rule::literal, "0x12ab"));
        assert_eq!(
            r#""string literal \"with\" quote""#,
            parse_part(Rule::literal, r#""string literal \"with\" quote""#)
        );
    }

    #[test]
    fn test_attribute() {
        assert_eq!("[]", parse_part(Rule::attribute_section, "[]"));
        assert_eq!(
            "[Attr1, Attr2=NameVal, Attr3=123]",
            parse_part(Rule::attribute_section, "[Attr1, Attr2=NameVal, Attr3=123]")
        );
        assert_eq!(
            "[Attr=foo.bar.baz]",
            parse_part(Rule::attribute_section, "[Attr=foo.bar.baz]")
        );
    }

    #[test]
    fn test_types() {
        macro_rules! parse_type {
            ($tok:expr) => {{
                assert_eq!($tok, parse_part(Rule::type_spec, $tok));
            }};
        }

        parse_type!("bool");
        parse_type!("int8");
        parse_type!("uint8");
        parse_type!("int16");
        parse_type!("uint16");
        parse_type!("int32");
        parse_type!("uint32");
        parse_type!("int64");
        parse_type!("uint64");
        parse_type!("float");
        parse_type!("double");
        parse_type!("handle");
        parse_type!("handle<message_pipe>");
        parse_type!("pending_receiver<MyInterface>");
        parse_type!("pending_remote<mymodule.MyInterface>");
        parse_type!("pending_associated_remote<FooInterface>?");
        parse_type!("pending_associated_receiver<FooInterface>");
        parse_type!("handle<message_pipe>");
        parse_type!("string");
        parse_type!("array<uint8>");
        parse_type!("array<uint8, 16>");
        parse_type!("map<int32, MyInterface>");
        parse_type!("MyInterface");
        parse_type!("MyInerface&");
        parse_type!("associated MyInterface");
        parse_type!("associated MyInterface&");
        parse_type!("bool?");
    }

    #[test]
    fn test_module_stmt() {
        let input = "module my.mod;";
        let parsed = MojomParser::parse(Rule::module_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_module(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 14 });
        assert_eq!("my.mod", partial_text(&input, &stmt.name));
    }

    #[test]
    fn test_import_stmt() {
        let input = r#"import "my.mod";"#;
        let parsed = MojomParser::parse(Rule::import_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_import(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 16 });
        assert_eq!(r#""my.mod""#, partial_text(&input, &stmt.path));

        let input = r#"[Attr] import "my.mod";"#;
        let parsed = MojomParser::parse(Rule::import_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_import(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 23 });
        assert_eq!(r#""my.mod""#, partial_text(&input, &stmt.path));
    }

    #[test]
    fn test_const_stmt() {
        let input = "const uint32 kTheAnswer = 42;";
        let parsed = MojomParser::parse(Rule::const_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_const(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 29 });
        assert_eq!("uint32", partial_text(&input, &stmt.typ));
        assert_eq!("kTheAnswer", partial_text(&input, &stmt.name));
        assert_eq!("42", partial_text(&input, &stmt.value));
    }

    #[test]
    fn test_enum_stmt() {
        let input = "enum MyEnum { kOne, kTwo=2, kThree=IdentValue, };";
        let parsed = MojomParser::parse(Rule::enum_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_enum(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 49 });
        assert_eq!("MyEnum", partial_text(&input, &stmt.name));
        let values = &stmt.values;
        assert_eq!(3, values.len());
        assert_eq!("kOne", partial_text(&input, &values[0].name));
        assert_eq!("kTwo", partial_text(&input, &values[1].name));
        assert_eq!("2", partial_text(&input, values[1].value.as_ref().unwrap()));
        assert_eq!("kThree", partial_text(&input, &values[2].name));
        assert_eq!(
            "IdentValue",
            partial_text(&input, values[2].value.as_ref().unwrap())
        );

        let input = "enum MyEnum {};";
        let parsed = MojomParser::parse(Rule::enum_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_enum(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 15 });
        assert_eq!("MyEnum", partial_text(&input, &stmt.name));
        assert_eq!(0, stmt.values.len());

        let input = "[Native] enum MyEnum;";
        let parsed = MojomParser::parse(Rule::enum_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_enum(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 21 });
        assert_eq!("MyEnum", partial_text(&input, &stmt.name));
        assert_eq!(0, stmt.values.len());
    }

    #[test]
    fn test_method_stmt() {
        let input = "MyMethod(string str_arg, int8 int8_arg) => (uint32 result);";
        let parsed = MojomParser::parse(Rule::method_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_method(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 59 });
        assert_eq!("MyMethod", partial_text(&input, &stmt.name));
        let params = &stmt.params;
        assert_eq!(2, params.len());
        assert_eq!("string", partial_text(&input, &params[0].typ));
        assert_eq!("str_arg", partial_text(&input, &params[0].name));
        assert_eq!("int8", partial_text(&input, &params[1].typ));
        assert_eq!("int8_arg", partial_text(&input, &params[1].name));
        let response = stmt.response.as_ref().unwrap();
        assert_eq!(1, response.params.len());
        assert_eq!("uint32", partial_text(&input, &response.params[0].typ));
        assert_eq!("result", partial_text(&input, &response.params[0].name));

        let input = "MyMethod2();";
        let parsed = MojomParser::parse(Rule::method_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_method(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 12 });
        assert_eq!("MyMethod2", partial_text(&input, &stmt.name));
        assert_eq!(0, stmt.params.len());
        assert!(stmt.response.is_none());

        let input = "MyMethod3(int8 default_int8_arg) => ();";
        let parsed = MojomParser::parse(Rule::method_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_method(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 39 });
        assert_eq!("MyMethod3", partial_text(&input, &stmt.name));
        assert_eq!(1, stmt.params.len());
        let params = &stmt.params;
        assert_eq!("int8", partial_text(&input, &params[0].typ));
        assert_eq!("default_int8_arg", partial_text(&input, &params[0].name));
        let response = stmt.response.as_ref().unwrap();
        assert_eq!(0, response.params.len());
    }

    #[test]
    fn test_struct_stmt() {
        let input = "struct MyStruct {
            const int64 kInvalidId = -1;
            int64 my_id;
            MyInterface? my_interface;
            float my_float_value = 0.1;
        };";
        let parsed = MojomParser::parse(Rule::struct_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_struct(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 173 });
        assert_eq!("MyStruct", partial_text(&input, &stmt.name));
        let members = &stmt.members;
        assert_eq!(4, members.len());

        let item = match &members[0] {
            StructBody::Const(item) => item,
            _ => unreachable!(),
        };
        assert_eq!("kInvalidId", partial_text(&input, &item.name));

        let item = match &members[1] {
            StructBody::Field(item) => item,
            _ => unreachable!(),
        };
        assert_eq!("my_id", partial_text(&input, &item.name));

        let item = match &members[2] {
            StructBody::Field(item) => item,
            _ => unreachable!(),
        };
        assert_eq!("my_interface", partial_text(&input, &item.name));

        let item = match &members[3] {
            StructBody::Field(item) => item,
            _ => unreachable!(),
        };
        assert_eq!("my_float_value", partial_text(&input, &item.name));

        let input = "[Native] struct MyStruct;";
        let parsed = MojomParser::parse(Rule::struct_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_struct(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 25 });
        assert_eq!("MyStruct", partial_text(&input, &stmt.name));
        assert_eq!(0, stmt.members.len());
    }

    #[test]
    fn test_interface() {
        let input = "interface MyInterface {
            MyMethod();
            enum MyEnum { kMyEnumVal1, kMyEnumVal2 };
        };";
        let parsed = MojomParser::parse(Rule::interface, &input)
            .unwrap()
            .next()
            .unwrap();
        let intr = into_interface(parsed);
        assert_eq!(intr.range, Range { start: 0, end: 112 });
        assert_eq!("MyInterface", partial_text(&input, &intr.name));
        let members = &intr.members;
        assert_eq!(2, members.len());

        let member = match &members[0] {
            InterfaceMember::Method(member) => member,
            _ => unreachable!(),
        };
        assert_eq!("MyMethod", partial_text(&input, &member.name));

        let member = match &members[1] {
            InterfaceMember::Enum(member) => member,
            _ => unreachable!(),
        };
        assert_eq!("MyEnum", partial_text(&input, &member.name));
    }

    #[test]
    fn test_union_stmt() {
        let input = "union MyUnion {
            string str_field;
            StringPair pair_field;
            int64 int64_field;
        };";
        let parsed = MojomParser::parse(Rule::union_stmt, &input)
            .unwrap()
            .next()
            .unwrap();
        let stmt = into_union(parsed);
        assert_eq!(stmt.range, Range { start: 0, end: 122 });
        assert_eq!("MyUnion", partial_text(&input, &stmt.name));
        let fields = &stmt.fields;
        assert_eq!(3, fields.len());
        assert_eq!("str_field", partial_text(&input, &fields[0].name));
        assert_eq!("pair_field", partial_text(&input, &fields[1].name));
        assert_eq!("int64_field", partial_text(&input, &fields[2].name));
    }

    #[test]
    fn test_parse() {
        let input = r#"
        module test.mod;
        import "a.b.c";
        import "a.c.d";

        enum MyEnum;
        enum MyEnum2 { kFoo, kBar, kBaz };

        const string kMyConst = "const_value";
        const int32 kMyConst2 = -1;

        struct MyStruct {};
        struct MyStruct2 {
            uint8 my_uint8_value;
            float my_float_value = 0.1;
        };

        union MyUnion {
            string str_field;
            uint8 uint8_field;
        };
        union MyUnion2 {
            MyInterface myinterface_field;
            uint32 uint32_field;
        };

        // This is MyInterface
        interface MyInterface {
            MyMethod() => (/* empty */);
        };

        interface InterfaceA {};

        // This is comment.
        interface InterfaceB {};

        [Attr]
        interface InterfaceC {};

        interface InterfaceD {
            const string kMessage = "message";
            enum SomeEnum { Foo, Bar, Baz, };
            MethodA(string message, string? optional_message) => ();
            MethodB() => (int32 result, MyStruct? optional_result);
            [Attr2] MethodC(associated InterfaceA assoc) => (map<string, int8> result);
        };
        "#;
        let res = parse(input).unwrap();
        assert_eq!(16, res.stmts.len());
    }
}
