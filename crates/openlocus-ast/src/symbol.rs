//! AST symbol extraction.
//!
//! Extracts narrow header/signature spans for definitions in supported
//! languages using Tree-sitter. Unsupported languages or parse errors
//! return empty symbols; callers can fall back to regex-based search.

use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Node, Parser, Tree};

use crate::chunk::AstLanguage;

// ── Types ─────────────────────────────────────────────────────────────

/// Kind of AST symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AstSymbolKind {
    Function,
    Method,
    Class,
    Interface,
    Type,
    Enum,
    Trait,
    Module,
    Variable,
    Constant,
    Macro,
    Decorator,
    Unknown,
}

/// A single symbol extracted from a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstSymbol {
    /// Symbol name.
    pub name: String,
    /// Kind of symbol.
    pub kind: AstSymbolKind,
    /// 1-based start line of the symbol's header/signature.
    pub start_line: u64,
    /// 1-based end line of the symbol's header/signature (inclusive, max 10 lines).
    pub end_line: u64,
    /// AST node type name.
    pub node_type: String,
}

/// Status of AST symbol extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AstSymbolStatus {
    /// AST parsing succeeded.
    Supported,
    /// Language not supported; returned empty symbols.
    FallbackUnsupported,
    /// Parse error; returned empty symbols.
    FallbackParseError,
}

/// Result of extracting AST symbols from a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstFileSymbols {
    /// File path (passed through from caller).
    pub path: String,
    /// Language used for parsing.
    pub language: String,
    /// Status of extraction.
    pub status: AstSymbolStatus,
    /// Extracted symbols.
    pub symbols: Vec<AstSymbol>,
}

/// Maximum header/signature span in lines.
const MAX_SYMBOL_HEADER_LINES: u64 = 10;

/// Extract AST symbols from a source file.
///
/// Returns narrow header/signature spans (max 10 lines) for definitions.
/// Unsupported languages or parse errors return empty + fallback status;
/// callers can use regex fallback.
pub fn extract_ast_symbols(path: &str, language: &str, source: &str) -> AstFileSymbols {
    let ast_lang = AstLanguage::from_str_loose(language);

    let total_lines = source.lines().count() as u64;

    match ast_lang {
        Some(lang) => {
            let (tree, ts_lang) = match parse_source(source, &lang) {
                Ok(t) => t,
                Err(_) => {
                    return AstFileSymbols {
                        path: path.to_string(),
                        language: language.to_string(),
                        status: AstSymbolStatus::FallbackParseError,
                        symbols: vec![],
                    };
                }
            };

            let root = tree.root_node();
            if root.has_error() {
                return AstFileSymbols {
                    path: path.to_string(),
                    language: language.to_string(),
                    status: AstSymbolStatus::FallbackParseError,
                    symbols: vec![],
                };
            }
            let mut symbols = Vec::new();
            collect_symbols_with_names(&root, &lang, total_lines, source.as_bytes(), &mut symbols);

            let _ = ts_lang; // keep alive

            AstFileSymbols {
                path: path.to_string(),
                language: language.to_string(),
                status: AstSymbolStatus::Supported,
                symbols,
            }
        }
        None => AstFileSymbols {
            path: path.to_string(),
            language: language.to_string(),
            status: AstSymbolStatus::FallbackUnsupported,
            symbols: vec![],
        },
    }
}

// ── Parse ─────────────────────────────────────────────────────────────

fn parse_source(source: &str, lang: &AstLanguage) -> anyhow::Result<(Tree, Language)> {
    let ts_lang: Language = match lang {
        AstLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
        AstLanguage::Python => tree_sitter_python::LANGUAGE.into(),
        AstLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        AstLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    };

    let mut parser = Parser::new();
    parser.set_language(&ts_lang)?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter parse returned None"))?;

    Ok((tree, ts_lang))
}

// ── Symbol collection ─────────────────────────────────────────────────

/// Try to extract a symbol kind from a node type for the given language.
/// Returns None if the node is not a definition node.
fn symbol_info_for_node(kind: &str, lang: &AstLanguage) -> Option<(Option<String>, AstSymbolKind)> {
    match lang {
        AstLanguage::Rust => rust_symbol_info(kind),
        AstLanguage::Python => python_symbol_info(kind),
        AstLanguage::JavaScript => js_symbol_info(kind),
        AstLanguage::TypeScript => ts_symbol_info(kind),
    }
}

fn rust_symbol_info(kind: &str) -> Option<(Option<String>, AstSymbolKind)> {
    match kind {
        "function_item" => Some((None, AstSymbolKind::Function)),
        "struct_item" => Some((None, AstSymbolKind::Class)),
        "enum_item" => Some((None, AstSymbolKind::Enum)),
        "trait_item" => Some((None, AstSymbolKind::Trait)),
        "impl_item" => Some((None, AstSymbolKind::Class)),
        "mod_item" => Some((None, AstSymbolKind::Module)),
        "type_item" => Some((None, AstSymbolKind::Type)),
        "macro_definition" => Some((None, AstSymbolKind::Macro)),
        _ => None,
    }
}

fn python_symbol_info(kind: &str) -> Option<(Option<String>, AstSymbolKind)> {
    match kind {
        "function_definition" => Some((None, AstSymbolKind::Function)),
        "class_definition" => Some((None, AstSymbolKind::Class)),
        "decorated_definition" => Some((None, AstSymbolKind::Decorator)),
        _ => None,
    }
}

fn js_symbol_info(kind: &str) -> Option<(Option<String>, AstSymbolKind)> {
    match kind {
        "function_declaration" => Some((None, AstSymbolKind::Function)),
        "class_declaration" => Some((None, AstSymbolKind::Class)),
        "method_definition" => Some((None, AstSymbolKind::Method)),
        "lexical_declaration" => Some((None, AstSymbolKind::Variable)),
        "variable_declaration" => Some((None, AstSymbolKind::Variable)),
        "export_statement" => Some((None, AstSymbolKind::Unknown)),
        _ => None,
    }
}

fn ts_symbol_info(kind: &str) -> Option<(Option<String>, AstSymbolKind)> {
    match kind {
        "function_declaration" => Some((None, AstSymbolKind::Function)),
        "class_declaration" => Some((None, AstSymbolKind::Class)),
        "method_definition" => Some((None, AstSymbolKind::Method)),
        "lexical_declaration" => Some((None, AstSymbolKind::Variable)),
        "variable_declaration" => Some((None, AstSymbolKind::Variable)),
        "export_statement" => Some((None, AstSymbolKind::Unknown)),
        "interface_declaration" => Some((None, AstSymbolKind::Interface)),
        "type_alias_declaration" => Some((None, AstSymbolKind::Type)),
        "enum_declaration" => Some((None, AstSymbolKind::Enum)),
        _ => None,
    }
}

/// Extract the name from a definition node by looking for the "name" or
/// "identifier" child field. Requires the source bytes for utf8_text.
fn extract_name(node: &Node, source: &[u8]) -> Option<String> {
    // Try "name" field first (standard for most nodes)
    if let Some(name_node) = node.child_by_field_name("name")
        && let Ok(text) = name_node.utf8_text(source)
    {
        return Some(text.to_string());
    }
    // For impl_item, try "type" field
    if let Some(type_node) = node.child_by_field_name("type")
        && let Ok(text) = type_node.utf8_text(source)
    {
        return Some(text.to_string());
    }
    // Fallback: look for an "identifier" child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if (child.kind() == "identifier"
            || child.kind() == "type_identifier"
            || child.kind() == "property_identifier")
            && let Ok(text) = child.utf8_text(source)
        {
            return Some(text.to_string());
        }
    }
    None
}

fn collect_symbols_with_names(
    node: &Node,
    lang: &AstLanguage,
    total_lines: u64,
    source: &[u8],
    out: &mut Vec<AstSymbol>,
) {
    let kind = node.kind();
    let symbol_info = symbol_info_for_node(kind, lang);

    if let Some((_, sym_kind)) = symbol_info {
        let (name, resolved_kind) =
            if kind == "decorated_definition" && *lang == AstLanguage::Python {
                decorated_python_symbol(node, source)
                    .unwrap_or_else(|| ("<anonymous>".to_string(), sym_kind.clone()))
            } else {
                (
                    extract_name(node, source).unwrap_or_else(|| "<anonymous>".to_string()),
                    sym_kind.clone(),
                )
            };
        let start_line = node.start_position().row as u64 + 1;
        let raw_end_line = node.end_position().row as u64 + 1;
        let end_line =
            symbol_header_end_line(node, lang, source, start_line, raw_end_line, total_lines);

        out.push(AstSymbol {
            name,
            kind: resolved_kind,
            start_line,
            end_line,
            node_type: kind.to_string(),
        });
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_symbols_with_names(&child, lang, total_lines, source, out);
    }
}

fn decorated_python_symbol(node: &Node, source: &[u8]) -> Option<(String, AstSymbolKind)> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = match child.kind() {
            "function_definition" => AstSymbolKind::Function,
            "class_definition" => AstSymbolKind::Class,
            _ => continue,
        };
        let name = extract_name(&child, source)?;
        return Some((name, kind));
    }
    None
}

fn symbol_header_end_line(
    node: &Node,
    lang: &AstLanguage,
    source: &[u8],
    start_line: u64,
    raw_end_line: u64,
    total_lines: u64,
) -> u64 {
    let max_end = raw_end_line
        .min(start_line + MAX_SYMBOL_HEADER_LINES - 1)
        .min(total_lines);

    let Ok(text) = node.utf8_text(source) else {
        return start_line.min(total_lines);
    };

    let mut python_header_started = false;

    for (line_no, raw_line) in
        (start_line..).zip(text.lines().take(MAX_SYMBOL_HEADER_LINES as usize))
    {
        let trimmed = raw_line.trim();
        let header_ends_here = match lang {
            AstLanguage::Python => {
                if trimmed.starts_with('@') {
                    false
                } else if trimmed.starts_with("def ")
                    || trimmed.starts_with("async def ")
                    || trimmed.starts_with("class ")
                {
                    python_header_started = true;
                    trimmed.ends_with(':')
                } else {
                    python_header_started && trimmed.ends_with(':')
                }
            }
            AstLanguage::Rust => trimmed.contains('{') || trimmed.ends_with(';'),
            AstLanguage::JavaScript | AstLanguage::TypeScript => {
                trimmed.contains('{') || trimmed.ends_with(';') || trimmed.contains("=>")
            }
        };

        if header_ends_here {
            return line_no.min(max_end);
        }

        if line_no >= max_end {
            break;
        }
    }

    // If the parser gave us a broad body node but we could not find a clear
    // signature delimiter, keep the result conservative instead of returning a
    // full body span by default.
    start_line.min(max_end)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_fn_symbol() {
        let source = "fn authenticate_user() -> bool {\n    true\n}\n";
        let result = extract_ast_symbols("auth.rs", "rust", source);
        assert_eq!(result.status, AstSymbolStatus::Supported);
        assert!(!result.symbols.is_empty());
        let fn_sym = result
            .symbols
            .iter()
            .find(|s| s.kind == AstSymbolKind::Function);
        assert!(fn_sym.is_some(), "should find function symbol");
        let fn_sym = fn_sym.unwrap();
        assert_eq!(fn_sym.name, "authenticate_user");
        assert_eq!(fn_sym.start_line, 1);
        assert_eq!(
            fn_sym.end_line, 1,
            "function symbol should return header only"
        );
    }

    #[test]
    fn rust_impl_symbol() {
        let source = "impl Auth {\n    fn check(&self) -> bool { true }\n}\n";
        let result = extract_ast_symbols("auth.rs", "rust", source);
        assert_eq!(result.status, AstSymbolStatus::Supported);
        let impl_sym = result
            .symbols
            .iter()
            .find(|s| s.kind == AstSymbolKind::Class && s.node_type == "impl_item");
        assert!(impl_sym.is_some(), "should find impl symbol");
    }

    #[test]
    fn python_class_symbol() {
        let source = "class User:\n    def __init__(self):\n        pass\n";
        let result = extract_ast_symbols("models.py", "python", source);
        assert_eq!(result.status, AstSymbolStatus::Supported);
        let class_sym = result
            .symbols
            .iter()
            .find(|s| s.kind == AstSymbolKind::Class);
        assert!(class_sym.is_some(), "should find class symbol");
        assert_eq!(class_sym.unwrap().name, "User");
    }

    #[test]
    fn ts_interface_symbol() {
        let source = "interface Config {\n    name: string;\n}\n";
        let result = extract_ast_symbols("types.ts", "typescript", source);
        assert_eq!(result.status, AstSymbolStatus::Supported);
        let iface = result
            .symbols
            .iter()
            .find(|s| s.kind == AstSymbolKind::Interface);
        assert!(iface.is_some(), "should find interface symbol");
        assert_eq!(iface.unwrap().name, "Config");
    }

    #[test]
    fn unsupported_returns_empty() {
        let source = "func main() {}\n";
        let result = extract_ast_symbols("main.go", "go", source);
        assert_eq!(result.status, AstSymbolStatus::FallbackUnsupported);
        assert!(result.symbols.is_empty());
    }

    #[test]
    fn parse_error_returns_empty() {
        // Garbage input
        let source = "{{{{{\n";
        let result = extract_ast_symbols("bad.rs", "rust", source);
        assert_eq!(result.status, AstSymbolStatus::FallbackParseError);
        assert!(result.symbols.is_empty());
    }

    #[test]
    fn multiline_python_header_includes_decorator_and_signature_only() {
        let source =
            "@route('/login')\ndef authenticate_user(\n    request,\n):\n    return True\n";
        let result = extract_ast_symbols("auth.py", "python", source);
        assert_eq!(result.status, AstSymbolStatus::Supported);
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "authenticate_user")
            .expect("should find decorated python function");
        assert_eq!(sym.start_line, 1);
        assert_eq!(sym.end_line, 4);
    }

    #[test]
    fn multiline_rust_signature_stops_at_open_brace() {
        let source = "pub fn authenticate_user(\n    user: &User,\n) -> bool {\n    true\n}\n";
        let result = extract_ast_symbols("auth.rs", "rust", source);
        assert_eq!(result.status, AstSymbolStatus::Supported);
        let sym = result
            .symbols
            .iter()
            .find(|s| s.name == "authenticate_user")
            .expect("should find rust function");
        assert_eq!(sym.start_line, 1);
        assert_eq!(sym.end_line, 3);
    }
}
