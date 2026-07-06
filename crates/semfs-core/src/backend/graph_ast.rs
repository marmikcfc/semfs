//! AST knowledge-graph code lane — graphify parity (`tickets/ast-kg-code-lane`).
//!
//! A **deterministic, local, free** alternative to the LLM entity extractor
//! ([`super::graph::extract_graph`]) for *source code*. tree-sitter parses each
//! file; per-language queries (the tree-sitter `tags.scm` capture convention)
//! surface class/function/method/module definitions, imports, inheritance, and
//! call/type references. From those we emit the graphify relation ontology:
//!
//! | edge       | lane            | confidence | weight |
//! |------------|-----------------|------------|--------|
//! | `contains` | AST (nesting)   | EXTRACTED  | 1.0    |
//! | `method`   | AST (nesting)   | EXTRACTED  | 1.0    |
//! | `imports`  | AST             | EXTRACTED  | 1.0    |
//! | `inherits` | AST + resolve   | EXTRACTED  | 1.0    |
//! | `calls`    | AST + resolve   | INFERRED   | 0.8    |
//! | `uses`     | AST + resolve   | INFERRED   | 0.8    |
//!
//! Pure: no network, no DB, no Modal. [`parse_file`] handles everything
//! intra-file; [`resolve`] does the cross-file symbol-resolution pass that
//! turns `calls`/`uses`/`inherits` references into entity→entity edges (edges
//! to unknown symbols are dropped — graphify's "edges between known nodes only"
//! rule, mirroring [`super::graph::clean_relations`]).
//!
//! The extractor is **language-agnostic**: it understands only capture *names*
//! (`def.class`, `call`, `inherit`, …); each [`Lang`] supplies a query mapping
//! its native node types onto them. Adding a language = one enum arm + one
//! query string.

use std::collections::HashMap;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

/// graphify's 14 supported languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Rust,
    Java,
    C,
    Cpp,
    Ruby,
    CSharp,
    Kotlin,
    Scala,
    Php,
}

/// Code entity kind (graphify node typing) — distinct from the doc-lane
/// `ONTOLOGY` (Person/Org/…).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeKind {
    Class,
    Interface,
    Function,
    Method,
    Module,
}

impl CodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CodeKind::Class => "class",
            CodeKind::Interface => "interface",
            CodeKind::Function => "function",
            CodeKind::Method => "method",
            CodeKind::Module => "module",
        }
    }
    fn is_type(self) -> bool {
        matches!(self, CodeKind::Class | CodeKind::Interface)
    }
    fn is_callable(self) -> bool {
        matches!(self, CodeKind::Function | CodeKind::Method)
    }
    /// Parse back a kind label as persisted by [`Self::as_str`] (e.g.
    /// `graph_entity.kind` in the DB) — used by [`resolve_refs`]'s DB-backed
    /// lookup to interpret a candidate's kind without a second column.
    pub fn from_label(s: &str) -> Option<CodeKind> {
        Some(match s {
            "class" => CodeKind::Class,
            "interface" => CodeKind::Interface,
            "function" => CodeKind::Function,
            "method" => CodeKind::Method,
            "module" => CodeKind::Module,
            _ => return None,
        })
    }
}

/// One extracted code entity. `qualified` is the module-qualified node id
/// (`pkg.mod.Class.method`) used as a graph node path; `line` is 1-based.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeEntity {
    pub name: String,
    pub qualified: String,
    pub kind: CodeKind,
    pub line: usize,
    // byte range of the definition node — used for intra-file nesting only.
    start: usize,
    end: usize,
}

/// A typed entity→entity (or file→entity/module) relation, graphify parity.
/// `source`/`target` are qualified entity names, a file path (for file-level
/// `contains`/`imports`), or a module string (the `imports` target).
#[derive(Debug, Clone, PartialEq)]
pub struct CodeRelation {
    pub source: String,
    pub target: String,
    pub relation: String,
    pub confidence: String,
    pub confidence_score: f64,
    pub weight: f64,
    pub source_file: String,
    pub source_location: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RefKind {
    Inherits,
    Calls,
    Uses,
}

/// An unresolved reference (target is a bare symbol name resolved in [`resolve`]).
#[derive(Debug, Clone)]
struct Ref {
    from: String, // qualified name of the enclosing entity (or file path)
    name: String, // target symbol (simple name)
    kind: RefKind,
    line: usize,
}

/// A single file's parsed AST graph: entities, ready EXTRACTED relations
/// (`contains`/`method`/`imports`), and unresolved cross-file refs.
#[derive(Debug, Clone)]
pub struct FileAst {
    pub path: String,
    pub entities: Vec<CodeEntity>,
    pub extracted: Vec<CodeRelation>,
    refs: Vec<Ref>,
}

impl Lang {
    /// Map a file path's extension to a grammar. `None` ⇒ unsupported ⇒ caller
    /// routes the file to the LLM doc lane instead.
    pub fn from_path(path: &str) -> Option<Lang> {
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        Some(match ext.as_str() {
            "py" | "pyi" => Lang::Python,
            "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "go" => Lang::Go,
            "rs" => Lang::Rust,
            "java" => Lang::Java,
            "c" | "h" => Lang::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Lang::Cpp,
            "rb" => Lang::Ruby,
            "cs" => Lang::CSharp,
            "kt" | "kts" => Lang::Kotlin,
            "scala" | "sc" => Lang::Scala,
            "php" => Lang::Php,
            _ => return None,
        })
    }

    fn language(self) -> Language {
        match self {
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::C => tree_sitter_c::LANGUAGE.into(),
            Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Lang::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Lang::Scala => tree_sitter_scala::LANGUAGE.into(),
            Lang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        }
    }

    fn query_src(self) -> &'static str {
        match self {
            Lang::Python => Q_PYTHON,
            Lang::JavaScript => Q_JAVASCRIPT,
            Lang::TypeScript | Lang::Tsx => Q_TYPESCRIPT,
            Lang::Go => Q_GO,
            Lang::Rust => Q_RUST,
            Lang::Java => Q_JAVA,
            Lang::C => Q_C,
            Lang::Cpp => Q_CPP,
            Lang::Ruby => Q_RUBY,
            Lang::CSharp => Q_CSHARP,
            Lang::Kotlin => Q_KOTLIN,
            Lang::Scala => Q_SCALA,
            Lang::Php => Q_PHP,
        }
    }
}

/// A top-level definition's byte span + label, for AST-aware chunking
/// (`chunk::code_chunks`). Distinct from `CodeEntity` (the KG node): this is
/// just "what to cut on," not a graph node.
#[derive(Debug, Clone, PartialEq)]
pub struct DefSpan {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub kind: CodeKind,
    pub name: String,
}

/// Top-level class/function/method definitions (byte spans, source order) plus
/// the file's import lines — for AST-aware chunking. Reuses the same tree-sitter
/// query as [`parse_file`]. `None` if the extension is unsupported or the grammar
/// fails to load (caller falls back to the word/char chunker). Nested defs are
/// dropped: the chunker takes each OUTERMOST unit and recursively splits it only
/// if it exceeds the size budget.
pub fn def_spans(path: &str, src: &str) -> Option<(Vec<DefSpan>, Vec<String>)> {
    let lang = Lang::from_path(path)?;
    let language = lang.language();
    let query = Query::new(&language, lang.query_src()).ok()?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(src, None)?;
    let bytes = src.as_bytes();
    let names = query.capture_names();

    let mut defs: Vec<DefSpan> = Vec::new();
    let mut imports: Vec<String> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, tree.root_node(), bytes);
    while let Some(m) = it.next() {
        let mut kind: Option<CodeKind> = None;
        let mut range: Option<(usize, usize, usize)> = None; // start, end, line
        let mut name: Option<String> = None;
        for cap in m.captures {
            let node = cap.node;
            let r = (node.start_byte(), node.end_byte(), node.start_position().row + 1);
            match names[cap.index as usize] {
                "name" => name = Some(node.utf8_text(bytes).unwrap_or("").to_string()),
                "def.class" => (kind, range) = (Some(CodeKind::Class), Some(r)),
                "def.interface" => (kind, range) = (Some(CodeKind::Interface), Some(r)),
                "def.function" => (kind, range) = (Some(CodeKind::Function), Some(r)),
                "def.method" => (kind, range) = (Some(CodeKind::Method), Some(r)),
                "def.module" => (kind, range) = (Some(CodeKind::Module), Some(r)),
                "import" => imports.push(node.utf8_text(bytes).unwrap_or("").trim().to_string()),
                _ => {}
            }
        }
        if let (Some(kind), Some((start, end, line)), Some(name)) = (kind, range, name) {
            if !name.trim().is_empty() {
                defs.push(DefSpan { start, end, line, kind, name });
            }
        }
    }
    // Keep only OUTERMOST defs (a def strictly contained in another is nested).
    let mut top: Vec<DefSpan> = defs
        .iter()
        .filter(|d| {
            !defs.iter().any(|o| {
                o.start <= d.start && o.end >= d.end && (o.start, o.end) != (d.start, d.end)
            })
        })
        .cloned()
        .collect();
    top.sort_by_key(|d| d.start);
    Some((top, imports))
}

/// Module path from a file path: `pkg/sub/mod.py` → `pkg.sub.mod`.
fn module_of(path: &str) -> String {
    let p = path.trim_start_matches("./").trim_start_matches('/');
    let no_ext = p.rsplit_once('.').map(|(a, _)| a).unwrap_or(p);
    no_ext.replace(['/', '\\'], ".")
}

/// Parse one source file into its AST graph. `None` if the extension is not one
/// of the 14 supported languages, or if the query/grammar fails to load (the
/// caller treats `None` as "route to the doc lane").
pub fn parse_file(path: &str, src: &str) -> Option<FileAst> {
    let lang = Lang::from_path(path)?;
    let language = lang.language();
    let query = Query::new(&language, lang.query_src()).ok()?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(src, None)?;
    let bytes = src.as_bytes();
    let names = query.capture_names();

    // First pass: collect raw definitions (kind + name + byte range) and raw
    // references (callee/inherit/use symbols with their source byte offset).
    struct RawDef {
        kind: CodeKind,
        name: String,
        line: usize,
        start: usize,
        end: usize,
    }
    struct RawRef {
        name: String,
        kind: RefKind,
        offset: usize,
        line: usize,
    }
    let mut defs: Vec<RawDef> = Vec::new();
    let mut raw_refs: Vec<RawRef> = Vec::new();
    let module = module_of(path);
    let mut imports: Vec<(String, usize)> = Vec::new();

    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, tree.root_node(), bytes);
    while let Some(m) = it.next() {
        // Within a match, a definition pattern carries both `@name` and a
        // `@def.<kind>` capture; reference patterns carry a single capture.
        let mut def_kind: Option<CodeKind> = None;
        let mut def_range: Option<(usize, usize, usize)> = None; // start,end,line
        let mut def_name: Option<String> = None;
        for cap in m.captures {
            let cname = names[cap.index as usize];
            let node = cap.node;
            let text = node.utf8_text(bytes).unwrap_or("").to_string();
            match cname {
                "name" => def_name = Some(text),
                "def.class" => {
                    def_kind = Some(CodeKind::Class);
                    def_range = Some((
                        node.start_byte(),
                        node.end_byte(),
                        node.start_position().row + 1,
                    ));
                }
                "def.interface" => {
                    def_kind = Some(CodeKind::Interface);
                    def_range = Some((
                        node.start_byte(),
                        node.end_byte(),
                        node.start_position().row + 1,
                    ));
                }
                "def.function" => {
                    def_kind = Some(CodeKind::Function);
                    def_range = Some((
                        node.start_byte(),
                        node.end_byte(),
                        node.start_position().row + 1,
                    ));
                }
                "def.method" => {
                    def_kind = Some(CodeKind::Method);
                    def_range = Some((
                        node.start_byte(),
                        node.end_byte(),
                        node.start_position().row + 1,
                    ));
                }
                "def.module" => {
                    def_kind = Some(CodeKind::Module);
                    def_range = Some((
                        node.start_byte(),
                        node.end_byte(),
                        node.start_position().row + 1,
                    ));
                }
                "call" => raw_refs.push(RawRef {
                    name: last_ident(&text),
                    kind: RefKind::Calls,
                    offset: node.start_byte(),
                    line: node.start_position().row + 1,
                }),
                "use" => raw_refs.push(RawRef {
                    name: last_ident(&text),
                    kind: RefKind::Uses,
                    offset: node.start_byte(),
                    line: node.start_position().row + 1,
                }),
                "inherit" => raw_refs.push(RawRef {
                    name: last_ident(&text),
                    kind: RefKind::Inherits,
                    offset: node.start_byte(),
                    line: node.start_position().row + 1,
                }),
                "import" => imports.push((clean_import(&text), node.start_position().row + 1)),
                _ => {}
            }
        }
        if let (Some(kind), Some((s, e, line)), Some(name)) = (def_kind, def_range, def_name) {
            if !name.trim().is_empty() {
                defs.push(RawDef {
                    kind,
                    name,
                    line,
                    start: s,
                    end: e,
                });
            }
        }
    }

    // Second pass: assign qualified names + reclassify functions nested in a
    // class as methods, and emit `contains`/`method` from byte-range nesting.
    // Sort by start offset (widest-first on ties) so parents precede children.
    defs.sort_by_key(|d| (d.start, std::cmp::Reverse(d.end)));

    // Nearest strictly-enclosing definition index for each def (its parent).
    let parent_of: Vec<Option<usize>> = (0..defs.len())
        .map(|i| {
            (0..defs.len())
                .filter(|&j| j != i)
                .filter(|&j| defs[j].start <= defs[i].start && defs[j].end >= defs[i].end)
                .filter(|&j| !(defs[j].start == defs[i].start && defs[j].end == defs[i].end))
                .min_by_key(|&j| defs[j].end - defs[j].start)
        })
        .collect();

    // Qualified name = module + the chain of enclosing def names.
    let qualified_of: Vec<String> = (0..defs.len())
        .map(|i| {
            let mut chain = vec![defs[i].name.as_str()];
            let mut p = parent_of[i];
            while let Some(j) = p {
                chain.push(defs[j].name.as_str());
                p = parent_of[j];
            }
            chain.reverse();
            format!("{module}.{}", chain.join("."))
        })
        .collect();

    let mut entities: Vec<CodeEntity> = Vec::new();
    let mut extracted: Vec<CodeRelation> = Vec::new();

    for i in 0..defs.len() {
        let parent = parent_of[i];
        let parent_is_type = parent.map(|j| defs[j].kind.is_type()).unwrap_or(false);
        // A function directly inside a class is a method.
        let kind = if defs[i].kind == CodeKind::Function && parent_is_type {
            CodeKind::Method
        } else {
            defs[i].kind
        };

        let parent_qual = parent.map(|j| qualified_of[j].clone());
        let qualified = qualified_of[i].clone();
        let loc = format!("{path}:{}", defs[i].line);

        // contains (file/class → entity) or method (class → method).
        let (source, relation) = match (&parent_qual, kind) {
            (Some(pq), CodeKind::Method) => (pq.clone(), "method"),
            (Some(pq), _) => (pq.clone(), "contains"),
            (None, _) => (path.to_string(), "contains"),
        };
        extracted.push(CodeRelation {
            source,
            target: qualified.clone(),
            relation: relation.to_string(),
            confidence: "EXTRACTED".into(),
            confidence_score: 1.0,
            weight: 1.0,
            source_file: path.to_string(),
            source_location: loc.clone(),
        });

        entities.push(CodeEntity {
            name: defs[i].name.clone(),
            qualified,
            kind,
            line: defs[i].line,
            start: defs[i].start,
            end: defs[i].end,
        });
    }

    // file → module imports (EXTRACTED).
    for (m, line) in imports {
        if m.trim().is_empty() {
            continue;
        }
        extracted.push(CodeRelation {
            source: path.to_string(),
            target: m,
            relation: "imports".into(),
            confidence: "EXTRACTED".into(),
            confidence_score: 1.0,
            weight: 1.0,
            source_file: path.to_string(),
            source_location: format!("{path}:{line}"),
        });
    }

    // Attribute each reference to its innermost enclosing entity (the caller /
    // user). References outside any entity are attributed to the file.
    let refs: Vec<Ref> = raw_refs
        .into_iter()
        .map(|r| {
            let from = entities
                .iter()
                .filter(|e| e.start <= r.offset && e.end >= r.offset)
                .min_by_key(|e| e.end - e.start)
                .map(|e| e.qualified.clone())
                .unwrap_or_else(|| path.to_string());
            Ref {
                from,
                name: r.name,
                kind: r.kind,
                line: r.line,
            }
        })
        .filter(|r| !r.name.trim().is_empty())
        .collect();

    Some(FileAst {
        path: path.to_string(),
        entities,
        extracted,
        refs,
    })
}

/// Cross-file resolution: turn each file's unresolved `inherits`/`calls`/`uses`
/// refs into entity→entity edges against a global symbol table, then return
/// every relation (resolved + the ready EXTRACTED ones). Edges to unknown
/// symbols are dropped.
pub fn resolve(files: &[FileAst]) -> Vec<CodeRelation> {
    // simple name → [(qualified, kind)]
    let mut symbols: HashMap<&str, Vec<(&str, CodeKind)>> = HashMap::new();
    for f in files {
        for e in &f.entities {
            symbols
                .entry(e.name.as_str())
                .or_default()
                .push((e.qualified.as_str(), e.kind));
        }
    }

    let mut out: Vec<CodeRelation> = Vec::new();
    for f in files {
        out.extend(f.extracted.iter().cloned());
        for r in &f.refs {
            let Some(cands) = symbols.get(r.name.as_str()) else {
                continue;
            };
            let (relation, want): (&str, fn(CodeKind) -> bool) = match r.kind {
                RefKind::Calls => ("calls", CodeKind::is_callable),
                RefKind::Inherits => ("inherits", CodeKind::is_type),
                RefKind::Uses => ("uses", CodeKind::is_type),
            };
            // Distinct targets matching the expected kind, excluding self-loops.
            let mut seen: Vec<&str> = Vec::new();
            for (qual, kind) in cands {
                if !want(*kind) || *qual == r.from || seen.contains(qual) {
                    continue;
                }
                seen.push(qual);
                let (conf, score, weight) = match r.kind {
                    RefKind::Inherits => ("EXTRACTED", 1.0, 1.0),
                    _ => ("INFERRED", 0.8, 0.8),
                };
                out.push(CodeRelation {
                    source: r.from.clone(),
                    target: qual.to_string(),
                    relation: relation.to_string(),
                    confidence: conf.into(),
                    confidence_score: score,
                    weight,
                    source_file: f.path.clone(),
                    source_location: format!("{}:{}", f.path, r.line),
                });
            }
        }
    }
    out
}

/// Incremental, single-file variant of [`resolve`] for the live per-file index
/// path (`sqlite_vec::index_graph_ast`, SEM-55). [`resolve`] builds its symbol
/// table from ALL files at once — fine for the batch builder (one shot, every
/// file known up front), but the live path indexes one file per write, and
/// re-running a full-corpus `resolve()` on every write would be O(N²) over the
/// whole tree. Here the caller supplies `lookup`: given a bare symbol name,
/// return candidate `(qualified name, kind)` pairs from wherever it tracks
/// already-known entities (in practice, a `graph_entity` query). Matching
/// rules (kind-compatibility, no self-loops, EXTRACTED/INFERRED confidence)
/// mirror `resolve` exactly. Does NOT include `file.extracted` — that ready
/// EXTRACTED half (`contains`/`method`/`imports`) is written by the caller
/// directly. This is inherently order-dependent (a callee not yet indexed is
/// missed on this pass) but self-heals: every file re-indexes on write, so the
/// edge appears once the callee exists.
pub fn resolve_refs(
    file: &FileAst,
    lookup: impl Fn(&str) -> Vec<(String, CodeKind)>,
) -> Vec<CodeRelation> {
    let mut out: Vec<CodeRelation> = Vec::new();
    for r in &file.refs {
        let cands = lookup(&r.name);
        if cands.is_empty() {
            continue;
        }
        let (relation, want): (&str, fn(CodeKind) -> bool) = match r.kind {
            RefKind::Calls => ("calls", CodeKind::is_callable),
            RefKind::Inherits => ("inherits", CodeKind::is_type),
            RefKind::Uses => ("uses", CodeKind::is_type),
        };
        // Distinct targets matching the expected kind, excluding self-loops.
        let mut seen: Vec<String> = Vec::new();
        for (qual, kind) in cands {
            if !want(kind) || qual == r.from || seen.contains(&qual) {
                continue;
            }
            seen.push(qual.clone());
            let (conf, score, weight) = match r.kind {
                RefKind::Inherits => ("EXTRACTED", 1.0, 1.0),
                _ => ("INFERRED", 0.8, 0.8),
            };
            out.push(CodeRelation {
                source: r.from.clone(),
                target: qual,
                relation: relation.to_string(),
                confidence: conf.into(),
                confidence_score: score,
                weight,
                source_file: file.path.clone(),
                source_location: format!("{}:{}", file.path, r.line),
            });
        }
    }
    out
}

/// Last dotted/`::`/`->` segment of a reference (`a.b.c` → `c`, `Foo::bar` → `bar`).
fn last_ident(s: &str) -> String {
    s.trim()
        .rsplit(['.', ':', '>', '\\'])
        .next()
        .unwrap_or(s)
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
        .to_string()
}

/// Normalize an import capture to a module string (strip quotes/keywords).
fn clean_import(s: &str) -> String {
    s.trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == ';')
        .trim()
        .to_string()
}

// ───────────────────────────── per-language queries ─────────────────────────
// Capture convention (tree-sitter `tags.scm` style):
//   @name           the identifier of a definition (paired with a @def.* below)
//   @def.class      class / struct / enum / trait / object definition node
//   @def.interface  interface / protocol definition node
//   @def.function   free function / function declaration node
//   @def.method     method definition node (also derived from class nesting)
//   @def.module     module / namespace / package node
//   @call           callee identifier of a call expression           → calls
//   @use            a class/type name referenced (e.g. `new Foo`)     → uses
//   @inherit        a base class / implemented interface identifier   → inherits
//   @import         an import statement (text → module string)        → imports

const Q_PYTHON: &str = r#"
(class_definition name: (identifier) @name) @def.class
(function_definition name: (identifier) @name) @def.function
(call function: (identifier) @call)
(call function: (attribute attribute: (identifier) @call))
(class_definition superclasses: (argument_list (identifier) @inherit))
(class_definition superclasses: (argument_list (attribute attribute: (identifier) @inherit)))
(import_statement name: (dotted_name) @import)
(import_from_statement module_name: (dotted_name) @import)
"#;

const Q_JAVASCRIPT: &str = r#"
(class_declaration name: (identifier) @name) @def.class
(function_declaration name: (identifier) @name) @def.function
(method_definition name: (property_identifier) @name) @def.method
(call_expression function: (identifier) @call)
(call_expression function: (member_expression property: (property_identifier) @call))
(class_heritage (identifier) @inherit)
(new_expression constructor: (identifier) @use)
(import_statement source: (string (string_fragment) @import))
"#;

const Q_TYPESCRIPT: &str = r#"
(class_declaration name: (type_identifier) @name) @def.class
(abstract_class_declaration name: (type_identifier) @name) @def.class
(interface_declaration name: (type_identifier) @name) @def.interface
(function_declaration name: (identifier) @name) @def.function
(method_definition name: (property_identifier) @name) @def.method
(call_expression function: (identifier) @call)
(call_expression function: (member_expression property: (property_identifier) @call))
(extends_clause (identifier) @inherit)
(extends_type_clause (type_identifier) @inherit)
(implements_clause (type_identifier) @inherit)
(new_expression constructor: (identifier) @use)
(import_statement source: (string (string_fragment) @import))
"#;

const Q_GO: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(method_declaration name: (field_identifier) @name) @def.method
(type_declaration (type_spec name: (type_identifier) @name type: (struct_type))) @def.class
(type_declaration (type_spec name: (type_identifier) @name type: (interface_type))) @def.interface
(call_expression function: (identifier) @call)
(call_expression function: (selector_expression field: (field_identifier) @call))
(import_spec path: (interpreted_string_literal) @import)
"#;

const Q_RUST: &str = r#"
(struct_item name: (type_identifier) @name) @def.class
(enum_item name: (type_identifier) @name) @def.class
(union_item name: (type_identifier) @name) @def.class
(trait_item name: (type_identifier) @name) @def.interface
(function_item name: (identifier) @name) @def.function
(mod_item name: (identifier) @name) @def.module
(call_expression function: (identifier) @call)
(call_expression function: (field_expression field: (field_identifier) @call))
(macro_invocation macro: (identifier) @call)
(impl_item trait: (type_identifier) @inherit type: (type_identifier) @use)
(use_declaration argument: (scoped_identifier name: (identifier) @import))
"#;

const Q_JAVA: &str = r#"
(class_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(method_declaration name: (identifier) @name) @def.method
(method_invocation name: (identifier) @call)
(object_creation_expression type: (type_identifier) @use)
(superclass (type_identifier) @inherit)
(super_interfaces (type_list (type_identifier) @inherit))
(import_declaration (scoped_identifier) @import)
"#;

const Q_C: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @def.function
(call_expression function: (identifier) @call)
(struct_specifier name: (type_identifier) @name) @def.class
(preproc_include path: (string_literal) @import)
(preproc_include path: (system_lib_string) @import)
"#;

const Q_CPP: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @def.function
(class_specifier name: (type_identifier) @name) @def.class
(struct_specifier name: (type_identifier) @name) @def.class
(call_expression function: (identifier) @call)
(call_expression function: (field_expression field: (field_identifier) @call))
(base_class_clause (type_identifier) @inherit)
(preproc_include path: (string_literal) @import)
(preproc_include path: (system_lib_string) @import)
"#;

const Q_RUBY: &str = r#"
(class name: (constant) @name) @def.class
(module name: (constant) @name) @def.module
(method name: (identifier) @name) @def.method
(singleton_method name: (identifier) @name) @def.method
(call method: (identifier) @call)
(superclass (constant) @inherit)
"#;

const Q_CSHARP: &str = r#"
(class_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(struct_declaration name: (identifier) @name) @def.class
(method_declaration name: (identifier) @name) @def.method
(invocation_expression function: (identifier) @call)
(invocation_expression function: (member_access_expression name: (identifier) @call))
(object_creation_expression type: (identifier) @use)
(base_list (identifier) @inherit)
(using_directive (qualified_name) @import)
(using_directive (identifier) @import)
"#;

const Q_KOTLIN: &str = r#"
(class_declaration name: (identifier) @name) @def.class
(object_declaration name: (identifier) @name) @def.class
(function_declaration name: (identifier) @name) @def.function
(call_expression (identifier) @call)
(delegation_specifier (user_type (identifier) @inherit))
(delegation_specifier (constructor_invocation (user_type (identifier) @inherit)))
(import (qualified_identifier) @import)
"#;

const Q_SCALA: &str = r#"
(class_definition name: (identifier) @name) @def.class
(trait_definition name: (identifier) @name) @def.interface
(object_definition name: (identifier) @name) @def.class
(function_definition name: (identifier) @name) @def.function
(call_expression (identifier) @call)
(import_declaration (stable_identifier) @import)
(import_declaration (identifier) @import)
"#;

const Q_PHP: &str = r#"
(class_declaration name: (name) @name) @def.class
(interface_declaration name: (name) @name) @def.interface
(method_declaration name: (name) @name) @def.method
(function_definition name: (name) @name) @def.function
(function_call_expression function: (name) @call)
(member_call_expression name: (name) @call)
(object_creation_expression (name) @use)
(base_clause (name) @inherit)
(class_interface_clause (name) @inherit)
(namespace_use_clause (qualified_name) @import)
(namespace_use_clause (name) @import)
"#;

#[cfg(test)]
mod tests;
