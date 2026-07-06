//! Unit tests for the AST code lane. Pure: no network, no DB, fully
//! deterministic — runnable in CI. Each test parses a fixture snippet and
//! asserts the exact entity/relation set (kinds + confidences).

use super::*;

/// Parse one snippet and resolve it as a single-file set.
fn graph(path: &str, src: &str) -> (FileAst, Vec<CodeRelation>) {
    let ast = parse_file(path, src).expect("supported language");
    let rels = resolve(std::slice::from_ref(&ast));
    (ast, rels)
}

fn rel<'a>(rels: &'a [CodeRelation], relation: &str) -> Vec<&'a CodeRelation> {
    rels.iter().filter(|r| r.relation == relation).collect()
}

fn has_edge(rels: &[CodeRelation], relation: &str, src_suffix: &str, tgt_suffix: &str) -> bool {
    rels.iter().any(|r| {
        r.relation == relation && r.source.ends_with(src_suffix) && r.target.ends_with(tgt_suffix)
    })
}

#[test]
fn all_queries_compile() {
    // A bad node type / field name makes Query::new fail — assert every
    // language's query is valid against its grammar.
    for lang in [
        Lang::Python,
        Lang::JavaScript,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::Go,
        Lang::Rust,
        Lang::Java,
        Lang::C,
        Lang::Cpp,
        Lang::Ruby,
        Lang::CSharp,
        Lang::Kotlin,
        Lang::Scala,
        Lang::Php,
    ] {
        let language = lang.language();
        Query::new(&language, lang.query_src())
            .unwrap_or_else(|e| panic!("query for {lang:?} failed to compile: {e}"));
    }
}

#[test]
fn ext_routing() {
    assert_eq!(Lang::from_path("a/b/c.py"), Some(Lang::Python));
    assert_eq!(Lang::from_path("x.tsx"), Some(Lang::Tsx));
    assert_eq!(Lang::from_path("x.ts"), Some(Lang::TypeScript));
    assert_eq!(Lang::from_path("main.go"), Some(Lang::Go));
    // Unsupported → None (caller routes to the LLM doc lane).
    assert_eq!(Lang::from_path("schema.proto"), None);
    assert_eq!(Lang::from_path("README.md"), None);
}

#[test]
fn python_full_ontology() {
    let src = r#"
import os
from app.db import Session

class Animal:
    def speak(self):
        return "..."

class Dog(Animal):
    def speak(self):
        return bark()

def bark():
    return "woof"
"#;
    let (ast, rels) = graph("app/models.py", src);

    // entities: 2 classes, their methods, 1 top-level function.
    let kinds: Vec<_> = ast
        .entities
        .iter()
        .map(|e| (e.name.as_str(), e.kind))
        .collect();
    assert!(kinds.contains(&("Animal", CodeKind::Class)));
    assert!(kinds.contains(&("Dog", CodeKind::Class)));
    assert!(kinds.contains(&("bark", CodeKind::Function)));
    // speak appears twice (Animal.speak, Dog.speak) reclassified to Method.
    assert_eq!(
        ast.entities
            .iter()
            .filter(|e| e.name == "speak" && e.kind == CodeKind::Method)
            .count(),
        2
    );

    // contains: file → Animal/Dog/bark
    assert!(has_edge(
        &rels,
        "contains",
        "models.py",
        "app.models.Animal"
    ));
    assert!(has_edge(&rels, "contains", "models.py", "app.models.bark"));
    // method: class → method
    assert!(has_edge(
        &rels,
        "method",
        "app.models.Dog",
        "app.models.Dog.speak"
    ));
    // inherits: Dog → Animal (EXTRACTED)
    assert!(has_edge(
        &rels,
        "inherits",
        "app.models.Dog",
        "app.models.Animal"
    ));
    assert!(rel(&rels, "inherits")
        .iter()
        .all(|r| r.confidence == "EXTRACTED"));
    // imports: file → module
    assert!(rel(&rels, "imports").iter().any(|r| r.target == "os"));
    assert!(rel(&rels, "imports").iter().any(|r| r.target == "app.db"));
    // calls: Dog.speak → bark (INFERRED, weight 0.8)
    assert!(has_edge(
        &rels,
        "calls",
        "app.models.Dog.speak",
        "app.models.bark"
    ));
    let calls = rel(&rels, "calls");
    assert!(calls
        .iter()
        .all(|r| r.confidence == "INFERRED" && (r.weight - 0.8).abs() < 1e-9));
    // source_location is populated.
    assert!(rels
        .iter()
        .all(|r| r.source_location.starts_with("app/models.py:")));
}

#[test]
fn go_struct_method_call() {
    let src = r#"
package main

import "fmt"

type Server struct{}

func (s *Server) Start() {
    helper()
}

func helper() {
    fmt.Println("hi")
}
"#;
    let (ast, rels) = graph("srv/main.go", src);
    assert!(ast
        .entities
        .iter()
        .any(|e| e.name == "Server" && e.kind == CodeKind::Class));
    assert!(ast
        .entities
        .iter()
        .any(|e| e.name == "Start" && e.kind == CodeKind::Method));
    assert!(ast
        .entities
        .iter()
        .any(|e| e.name == "helper" && e.kind == CodeKind::Function));
    // Start calls helper (resolves; fmt.Println does not → dropped).
    assert!(has_edge(&rels, "calls", "Start", "helper"));
    assert!(!rel(&rels, "calls")
        .iter()
        .any(|r| r.target.ends_with("Println")));
    assert!(rel(&rels, "imports")
        .iter()
        .any(|r| r.target.contains("fmt")));
}

#[test]
fn typescript_class_interface_inherit() {
    let src = r#"
interface Greeter {
  greet(): string;
}

class Base {
  hello() { return "hi"; }
}

class Service extends Base implements Greeter {
  greet() { return this.hello(); }
}
"#;
    let (ast, rels) = graph("src/svc.ts", src);
    assert!(ast
        .entities
        .iter()
        .any(|e| e.name == "Greeter" && e.kind == CodeKind::Interface));
    assert!(ast
        .entities
        .iter()
        .any(|e| e.name == "Service" && e.kind == CodeKind::Class));
    // extends Base + implements Greeter both → inherits (EXTRACTED).
    assert!(has_edge(&rels, "inherits", "Service", "Base"));
    assert!(has_edge(&rels, "inherits", "Service", "Greeter"));
}

#[test]
fn java_inherit_and_call() {
    let src = r#"
package com.acme;

import java.util.List;

class Base {
    void run() {}
}

class Worker extends Base {
    void go() {
        run();
        new Base();
    }
}
"#;
    let (_ast, rels) = graph("com/acme/Worker.java", src);
    assert!(has_edge(&rels, "inherits", "Worker", "Base"));
    // `run()` resolves to Base.run by name (the only callable named `run`).
    assert!(has_edge(&rels, "calls", "Worker.go", "Base.run"));
    // `new Base()` → uses Base (INFERRED).
    assert!(has_edge(&rels, "uses", "Worker.go", "Base"));
    assert!(rel(&rels, "imports")
        .iter()
        .any(|r| r.target.contains("List")));
}

#[test]
fn rust_struct_fn_call() {
    let src = r#"
pub struct Engine;

fn boot() {
    start();
}

fn start() {}
"#;
    let (ast, rels) = graph("src/engine.rs", src);
    assert!(ast
        .entities
        .iter()
        .any(|e| e.name == "Engine" && e.kind == CodeKind::Class));
    assert!(ast
        .entities
        .iter()
        .any(|e| e.name == "boot" && e.kind == CodeKind::Function));
    assert!(has_edge(&rels, "calls", "boot", "start"));
}

#[test]
fn cross_file_resolution() {
    // calls/uses/inherits resolve ACROSS files via the global symbol table.
    let a = parse_file(
        "pkg/base.py",
        "class Base:\n    def run(self):\n        return 1\n",
    )
    .unwrap();
    let b = parse_file(
        "pkg/worker.py",
        "from pkg.base import Base\n\nclass Worker(Base):\n    def go(self):\n        return run()\n",
    )
    .unwrap();
    let rels = resolve(&[a, b]);

    // Worker (in worker.py) inherits Base (defined in base.py).
    assert!(has_edge(
        &rels,
        "inherits",
        "pkg.worker.Worker",
        "pkg.base.Base"
    ));
    // Worker.go calls run (defined in base.py as Base.run).
    assert!(has_edge(
        &rels,
        "calls",
        "pkg.worker.Worker.go",
        "pkg.base.Base.run"
    ));
}

#[test]
fn unresolved_refs_dropped() {
    // A call to a symbol that is defined nowhere yields NO calls edge.
    let (_ast, rels) = graph("x.py", "def f():\n    return totally_unknown_fn()\n");
    assert!(rel(&rels, "calls").is_empty());
}

#[test]
fn from_label_round_trips_as_str() {
    for k in [
        CodeKind::Class,
        CodeKind::Interface,
        CodeKind::Function,
        CodeKind::Method,
        CodeKind::Module,
    ] {
        assert_eq!(CodeKind::from_label(k.as_str()), Some(k));
    }
    assert_eq!(CodeKind::from_label("bogus"), None);
}

#[test]
fn resolve_refs_matches_lookup_incrementally() {
    // The live per-file path: worker.go's `helper()` call resolves against an
    // EXTERNAL lookup (standing in for a `graph_entity` DB query) instead of a
    // global `resolve()` symbol table built from all files at once.
    let worker = parse_file(
        "srv/worker.go",
        "package main\n\nfunc caller() {\n    helper()\n}\n",
    )
    .unwrap();

    // Nothing known yet (helper.go not indexed): no calls edge.
    let none: Vec<CodeRelation> = resolve_refs(&worker, |_| Vec::new());
    assert!(none.is_empty());

    // helper.go has since been indexed and `helper` is now a known Function.
    let resolved = resolve_refs(&worker, |name| {
        if name == "helper" {
            vec![("srv.helper.helper".to_string(), CodeKind::Function)]
        } else {
            Vec::new()
        }
    });
    assert!(has_edge(&resolved, "calls", "caller", "helper.helper"));
    // Kind-incompatible candidates (e.g. a Class named `helper`) don't match a
    // `calls` ref.
    let wrong_kind = resolve_refs(&worker, |name| {
        if name == "helper" {
            vec![("srv.helper.Helper".to_string(), CodeKind::Class)]
        } else {
            Vec::new()
        }
    });
    assert!(rel(&wrong_kind, "calls").is_empty());
}

#[test]
fn settle_time_resolve_recovers_forward_reference_incremental_misses() {
    // SEM-57 root cause: the live per-file path (`index_graph_ast`) resolves
    // via `resolve_refs` against only ALREADY-INDEXED entities. If `a.go` (the
    // caller) is indexed BEFORE `b.go` (the callee's home file), the
    // incremental pass never learns about `b.go`'s `Helper` and drops the
    // `calls` edge — permanently, unless `b.go` happens to be re-written
    // later. The settle-time fix (`graph_file::resolve_code_calls`) instead
    // re-runs the real batch `resolve()` over every file's CURRENT content
    // once the queue has settled, recovering exactly this edge.
    let a = parse_file(
        "pkg/a.go",
        "package pkg\n\nfunc Caller() {\n\tHelper()\n}\n",
    )
    .unwrap();
    let b = parse_file("pkg/b.go", "package pkg\n\nfunc Helper() {}\n").unwrap();

    // 1. Simulate the incremental live path: `a.go` is indexed first, so its
    //    `resolve_refs` lookup only knows about entities `a.go` itself has
    //    defined so far (none relevant to `Helper`) — `b.go` doesn't exist yet
    //    from the indexer's point of view.
    let incremental = resolve_refs(&a, |_name| Vec::new());
    assert!(
        incremental.iter().all(|r| r.relation != "calls"),
        "a purely order-dependent incremental pass must miss the forward reference: {incremental:?}"
    );

    // 2. Settle-time fix: `graph_ast::resolve` (the same fn the batch builder
    //    uses) sees BOTH files' full symbol tables at once and recovers it.
    let settled = resolve(&[a, b]);
    assert!(
        has_edge(&settled, "calls", "pkg.a.Caller", "pkg.b.Helper"),
        "global resolve at settle must recover the A→B calls edge: {settled:?}"
    );
}
