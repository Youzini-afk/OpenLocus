//! AST-bounded chunk extraction.
//!
//! Parses source files with Tree-sitter and produces chunks bounded by
//! language-specific AST node types (functions, classes, structs, etc.).
//! Unsupported languages or parse errors fall back to line-window chunking.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Node, Parser, Tree};

// ── Types ─────────────────────────────────────────────────────────────

/// Supported AST-aware languages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AstLanguage {
    Rust,
    Python,
    JavaScript,
    TypeScript,
}

impl AstLanguage {
    /// Try to map a language string (from scan) to an AstLanguage.
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "rust" => Some(Self::Rust),
            "python" => Some(Self::Python),
            "javascript" | "jsx" => Some(Self::JavaScript),
            "typescript" | "tsx" => Some(Self::TypeScript),
            _ => None,
        }
    }
}

/// Kind of AST chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AstChunkKind {
    /// An AST node matching a language-specific definition node.
    AstNode,
    /// Fallback line window for gaps or oversized nodes.
    FallbackLineWindow,
}

/// A single chunk extracted from a file by the AST chunker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstChunk {
    /// 1-based start line.
    pub start_line: u64,
    /// 1-based end line (inclusive).
    pub end_line: u64,
    /// Kind of chunk.
    pub kind: AstChunkKind,
    /// The AST node type name (e.g. "function_item", "struct_item"), if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
}

/// Status of AST extraction for a single file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AstStatus {
    /// AST parsing succeeded and chunks were extracted.
    Supported,
    /// Language is not supported by the AST chunker; fell back to line windows.
    FallbackUnsupported,
    /// Parse error; fell back to line windows.
    FallbackParseError,
}

/// Stats about AST chunking for an entire index build.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AstStats {
    /// Number of files successfully parsed with AST.
    pub supported_files: u64,
    /// Number of files that fell back due to unsupported language.
    pub fallback_files: u64,
    /// Number of files that fell back due to parse errors.
    pub parser_error_files: u64,
    /// Number of chunks from AST nodes.
    pub ast_chunks: u64,
    /// Number of chunks from fallback line windows.
    pub fallback_chunks: u64,
}

/// Result of extracting AST chunks from a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstFileChunks {
    /// File path (passed through from caller).
    pub path: String,
    /// Language used for parsing.
    pub language: String,
    /// Status of AST extraction.
    pub status: AstStatus,
    /// Extracted chunks.
    pub chunks: Vec<AstChunk>,
}

/// Default maximum chunk size in lines for AST chunking.
pub const DEFAULT_MAX_CHUNK_LINES: u64 = 30;

/// Extract AST-bounded chunks from a source file.
///
/// - `path`: file path (metadata only; source is read from `source` param).
/// - `language`: language string from scan.
/// - `source`: file content (caller reads from filesystem).
/// - `max_lines`: maximum lines per chunk; oversized nodes are split.
///
/// Returns `AstFileChunks` with chunks covering the entire file (1..total_lines)
/// with no overlaps and no gaps.
pub fn extract_ast_chunks(
    path: &str,
    language: &str,
    source: &str,
    max_lines: u64,
) -> AstFileChunks {
    let ast_lang = AstLanguage::from_str_loose(language);

    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len() as u64;

    if total_lines == 0 {
        return AstFileChunks {
            path: path.to_string(),
            language: language.to_string(),
            status: AstStatus::FallbackUnsupported,
            chunks: vec![],
        };
    }

    match ast_lang {
        Some(lang) => {
            let (tree, ts_lang) = match parse_source(source, &lang) {
                Ok(t) => t,
                Err(_) => {
                    return AstFileChunks {
                        path: path.to_string(),
                        language: language.to_string(),
                        status: AstStatus::FallbackParseError,
                        chunks: fallback_line_chunks(total_lines, max_lines),
                    };
                }
            };

            let root = tree.root_node();
            if root.has_error() {
                return AstFileChunks {
                    path: path.to_string(),
                    language: language.to_string(),
                    status: AstStatus::FallbackParseError,
                    chunks: fallback_line_chunks(total_lines, max_lines),
                };
            }
            let ast_chunks = collect_ast_nodes(&root, &lang, max_lines, total_lines);

            let chunks = fill_gaps(ast_chunks, total_lines, max_lines);

            let ast_count = chunks
                .iter()
                .filter(|c| c.kind == AstChunkKind::AstNode)
                .count() as u64;
            let _fallback_count = chunks
                .iter()
                .filter(|c| c.kind == AstChunkKind::FallbackLineWindow)
                .count() as u64;

            let status = if ast_count > 0 {
                AstStatus::Supported
            } else {
                AstStatus::FallbackParseError
            };

            let _ = ts_lang; // keep reference alive

            AstFileChunks {
                path: path.to_string(),
                language: language.to_string(),
                status,
                chunks,
            }
        }
        None => AstFileChunks {
            path: path.to_string(),
            language: language.to_string(),
            status: AstStatus::FallbackUnsupported,
            chunks: fallback_line_chunks(total_lines, max_lines),
        },
    }
}

// ── Parse ─────────────────────────────────────────────────────────────

fn parse_source(source: &str, lang: &AstLanguage) -> Result<(Tree, Language)> {
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

// ── Collect AST nodes ─────────────────────────────────────────────────

/// Collect AST definition nodes for the given language.
fn collect_ast_nodes(
    root: &Node,
    lang: &AstLanguage,
    max_lines: u64,
    total_lines: u64,
) -> Vec<AstChunk> {
    let target_kinds = definition_kinds(lang);
    let mut chunks = Vec::new();
    collect_recursive(root, &target_kinds, max_lines, total_lines, &mut chunks);
    chunks.sort_by_key(|c| c.start_line);
    // Remove overlapping chunks (keep first/outer)
    deduplicate_overlaps(&mut chunks);
    chunks
}

fn collect_recursive(
    node: &Node,
    target_kinds: &[&str],
    max_lines: u64,
    total_lines: u64,
    out: &mut Vec<AstChunk>,
) {
    let kind = node.kind();

    if target_kinds.contains(&kind) {
        let start = node.start_position().row as u64 + 1; // 1-based
        let _end = node.end_position().row as u64; // tree-sitter end is exclusive row, but we want inclusive
        // tree-sitter end_position().row is the row of the last character + 1 if it's on a new line
        // Actually: end_position is the byte after the last character.
        // If a node ends at the end of line N, end_position.row = N (0-based) => 1-based = N+1
        // But if it's exactly at the start of the next line, it could be N+1 (0-based).
        // We want inclusive end line: use end_position.row (0-based) + 1 for 1-based.
        let end_line = node.end_position().row as u64 + 1;
        let start_line = start;

        // Clamp to file bounds
        let start_line = start_line.max(1).min(total_lines);
        let end_line = end_line.max(start_line).min(total_lines);

        if end_line - start_line + 1 > max_lines {
            // Oversized node: split into line windows
            let mut window_start = start_line;
            while window_start <= end_line {
                let window_end = (window_start + max_lines - 1).min(end_line);
                out.push(AstChunk {
                    start_line: window_start,
                    end_line: window_end,
                    kind: AstChunkKind::FallbackLineWindow,
                    node_type: Some(format!("{}_oversized", kind)),
                });
                window_start = window_end + 1;
            }
        } else {
            out.push(AstChunk {
                start_line,
                end_line,
                kind: AstChunkKind::AstNode,
                node_type: Some(kind.to_string()),
            });
        }
        // Don't recurse into children of a definition node — the whole node is one chunk
        return;
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_recursive(&child, target_kinds, max_lines, total_lines, out);
    }
}

/// Language-specific definition node kinds.
fn definition_kinds(lang: &AstLanguage) -> Vec<&'static str> {
    match lang {
        AstLanguage::Rust => vec![
            "function_item",
            "struct_item",
            "enum_item",
            "trait_item",
            "impl_item",
            "mod_item",
            "type_item",
            "macro_definition",
        ],
        AstLanguage::Python => vec![
            "function_definition",
            "class_definition",
            "decorated_definition",
        ],
        AstLanguage::JavaScript => vec![
            "function_declaration",
            "class_declaration",
            "method_definition",
            "lexical_declaration",
            "variable_declaration",
            "export_statement",
        ],
        AstLanguage::TypeScript => vec![
            "function_declaration",
            "class_declaration",
            "method_definition",
            "lexical_declaration",
            "variable_declaration",
            "export_statement",
            "interface_declaration",
            "type_alias_declaration",
            "enum_declaration",
        ],
    }
}

/// Remove overlapping chunks. When chunks overlap, keep the one that
/// starts earlier (which is typically the outer/containing node).
fn deduplicate_overlaps(chunks: &mut Vec<AstChunk>) {
    if chunks.len() <= 1 {
        return;
    }
    let mut result: Vec<AstChunk> = Vec::with_capacity(chunks.len());
    result.push(chunks[0].clone());
    for chunk in chunks.iter().skip(1) {
        let last = result.last().unwrap();
        if chunk.start_line <= last.end_line {
            // Overlap: skip the later chunk (it's likely a child node)
            continue;
        }
        result.push(chunk.clone());
    }
    *chunks = result;
}

// ── Gap filling ───────────────────────────────────────────────────────

/// Fill gaps between AST chunks with fallback line windows.
/// Ensures complete coverage of [1, total_lines] with no overlaps.
fn fill_gaps(ast_chunks: Vec<AstChunk>, total_lines: u64, max_lines: u64) -> Vec<AstChunk> {
    if ast_chunks.is_empty() {
        return fallback_line_chunks(total_lines, max_lines);
    }

    let mut result: Vec<AstChunk> = Vec::new();
    let mut cursor: u64 = 1; // next uncovered line (1-based)

    for chunk in &ast_chunks {
        if chunk.start_line > cursor {
            // Gap: fill with fallback line windows
            fill_gap(&mut result, cursor, chunk.start_line - 1, max_lines);
        }
        if chunk.start_line > cursor || chunk.end_line >= cursor {
            result.push(chunk.clone());
            cursor = chunk.end_line + 1;
        }
    }

    // Fill trailing gap
    if cursor <= total_lines {
        fill_gap(&mut result, cursor, total_lines, max_lines);
    }

    result
}

/// Fill a gap [gap_start, gap_end] with fallback line windows.
fn fill_gap(out: &mut Vec<AstChunk>, gap_start: u64, gap_end: u64, max_lines: u64) {
    let mut start = gap_start;
    while start <= gap_end {
        let end = (start + max_lines - 1).min(gap_end);
        out.push(AstChunk {
            start_line: start,
            end_line: end,
            kind: AstChunkKind::FallbackLineWindow,
            node_type: None,
        });
        start = end + 1;
    }
}

/// Generate fallback line-window chunks covering [1, total_lines].
fn fallback_line_chunks(total_lines: u64, max_lines: u64) -> Vec<AstChunk> {
    let mut chunks = Vec::new();
    let mut start: u64 = 1;
    while start <= total_lines {
        let end = (start + max_lines - 1).min(total_lines);
        chunks.push(AstChunk {
            start_line: start,
            end_line: end,
            kind: AstChunkKind::FallbackLineWindow,
            node_type: None,
        });
        start = end + 1;
    }
    chunks
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_function_and_impl() {
        let source = r#"fn helper() -> i32 { 1 }

pub fn authenticate() -> bool {
    let x = helper();
    x > 0
}

impl Auth {
    fn check(&self) -> bool { true }
    fn login(&self) -> bool { false }
}
"#;
        let result = extract_ast_chunks("src/auth.rs", "rust", source, 30);
        assert_eq!(result.status, AstStatus::Supported);
        // Should have: helper fn, authenticate fn, impl block
        let ast_kinds: Vec<&str> = result
            .chunks
            .iter()
            .filter(|c| c.kind == AstChunkKind::AstNode)
            .map(|c| c.node_type.as_deref().unwrap_or(""))
            .collect();
        assert!(
            ast_kinds.contains(&"function_item"),
            "should find function_item, got {:?}",
            ast_kinds
        );
        assert!(
            ast_kinds.contains(&"impl_item"),
            "should find impl_item, got {:?}",
            ast_kinds
        );
        // No overlap
        assert_no_overlap(&result.chunks);
        // Full coverage
        assert_full_coverage(&result.chunks, source);
    }

    #[test]
    fn rust_struct_item() {
        let source = "struct Config {\n    name: String,\n    value: i32,\n}\n";
        let result = extract_ast_chunks("types.rs", "rust", source, 30);
        assert_eq!(result.status, AstStatus::Supported);
        let struct_chunks: Vec<_> = result
            .chunks
            .iter()
            .filter(|c| c.node_type.as_deref() == Some("struct_item"))
            .collect();
        assert_eq!(struct_chunks.len(), 1);
        assert_eq!(struct_chunks[0].start_line, 1);
        assert_eq!(struct_chunks[0].end_line, 4);
    }

    #[test]
    fn python_decorator() {
        let source =
            "@dataclass\nclass User:\n    name: str\n    age: int\n\ndef greet(user):\n    pass\n";
        let result = extract_ast_chunks("models.py", "python", source, 30);
        assert_eq!(result.status, AstStatus::Supported);
        // Should have decorated_definition for the class and function_definition for greet
        let ast_chunks: Vec<_> = result
            .chunks
            .iter()
            .filter(|c| c.kind == AstChunkKind::AstNode)
            .collect();
        assert!(
            ast_chunks.len() >= 2,
            "should find at least 2 AST chunks, got {:?}",
            ast_chunks
        );
        assert_no_overlap(&result.chunks);
        assert_full_coverage(&result.chunks, source);
    }

    #[test]
    fn typescript_interface_and_arrow() {
        let source = r#"interface Config {
  name: string;
  retries: number;
}

export const handler = async (req: Request): Promise<Response> => {
  return new Response("ok");
};
"#;
        let result = extract_ast_chunks("api.ts", "typescript", source, 30);
        assert_eq!(result.status, AstStatus::Supported);
        let node_types: Vec<&str> = result
            .chunks
            .iter()
            .filter(|c| c.kind == AstChunkKind::AstNode)
            .map(|c| c.node_type.as_deref().unwrap_or(""))
            .collect();
        assert!(
            node_types.iter().any(|t| t.contains("interface")),
            "should find interface, got {:?}",
            node_types
        );
        assert_no_overlap(&result.chunks);
        assert_full_coverage(&result.chunks, source);
    }

    #[test]
    fn unsupported_language_fallback() {
        let source = "func main() {\n    fmt.Println(\"hello\")\n}\n";
        let result = extract_ast_chunks("main.go", "go", source, 30);
        assert_eq!(result.status, AstStatus::FallbackUnsupported);
        assert!(
            result
                .chunks
                .iter()
                .all(|c| c.kind == AstChunkKind::FallbackLineWindow)
        );
        assert_full_coverage(&result.chunks, source);
    }

    #[test]
    fn parse_error_fallback() {
        // Invalid Rust syntax
        let source = "fn incomplete(\n";
        let result = extract_ast_chunks("bad.rs", "rust", source, 30);
        assert_eq!(result.status, AstStatus::FallbackParseError);
        assert!(
            result
                .chunks
                .iter()
                .all(|c| c.kind == AstChunkKind::FallbackLineWindow)
        );
        assert_full_coverage(&result.chunks, source);
    }

    #[test]
    fn unicode_line_range() {
        let source = "// 注释\nfn 你好() -> bool {\n    true\n}\n";
        let result = extract_ast_chunks("cn.rs", "rust", source, 30);
        assert_eq!(result.status, AstStatus::Supported);
        assert_full_coverage(&result.chunks, source);
        // Line ranges should be valid 1-based
        for chunk in &result.chunks {
            assert!(chunk.start_line >= 1);
            assert!(chunk.end_line >= chunk.start_line);
        }
    }

    #[test]
    fn oversized_node_split() {
        // Create a function that exceeds max_lines
        let mut source = String::from("fn big_function() {\n");
        for i in 0..50 {
            source.push_str(&format!("    let x{} = {};\n", i, i));
        }
        source.push_str("}\n");

        let result = extract_ast_chunks("big.rs", "rust", &source, 20);
        // The oversized function should be split into fallback_line_window chunks
        let oversized: Vec<_> = result
            .chunks
            .iter()
            .filter(|c| c.node_type.as_deref().unwrap_or("").contains("oversized"))
            .collect();
        assert!(!oversized.is_empty(), "oversized node should be split");
        for chunk in &oversized {
            assert!(chunk.end_line - chunk.start_line < 20);
        }
        assert_no_overlap(&result.chunks);
        assert_full_coverage(&result.chunks, &source);
    }

    #[test]
    fn no_overlap_in_chunks() {
        let source = r#"mod foo;

struct A { x: i32 }

fn main() {
    let a = A { x: 1 };
}
"#;
        let result = extract_ast_chunks("test.rs", "rust", source, 30);
        assert_no_overlap(&result.chunks);
        assert_full_coverage(&result.chunks, source);
    }

    // ── Helpers ────────────────────────────────────────────────────────

    fn assert_no_overlap(chunks: &[AstChunk]) {
        for i in 0..chunks.len() {
            for j in (i + 1)..chunks.len() {
                let a = &chunks[i];
                let b = &chunks[j];
                let overlap = a.start_line <= b.end_line && b.start_line <= a.end_line;
                assert!(
                    !overlap,
                    "chunks overlap: [{},{}] ({:?}) and [{},{}] ({:?})",
                    a.start_line, a.end_line, a.node_type, b.start_line, b.end_line, b.node_type
                );
            }
        }
    }

    fn assert_full_coverage(chunks: &[AstChunk], source: &str) {
        let total_lines = source.lines().count() as u64;
        if total_lines == 0 {
            assert!(chunks.is_empty());
            return;
        }
        // Sort by start_line
        let mut sorted = chunks.to_vec();
        sorted.sort_by_key(|c| c.start_line);

        // First chunk starts at 1
        assert_eq!(
            sorted.first().map(|c| c.start_line),
            Some(1),
            "coverage should start at line 1"
        );

        // Last chunk ends at total_lines
        assert_eq!(
            sorted.last().map(|c| c.end_line),
            Some(total_lines),
            "coverage should end at line {}",
            total_lines
        );

        // No gaps between chunks
        for w in sorted.windows(2) {
            assert_eq!(
                w[0].end_line + 1,
                w[1].start_line,
                "gap between [{},{}] and [{},{}]",
                w[0].start_line,
                w[0].end_line,
                w[1].start_line,
                w[1].end_line
            );
        }
    }
}
