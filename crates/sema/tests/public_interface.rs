use gane_parser::{
    ast::{Decl, Expr, Stmt},
    parser::Mode,
    parser::parse_file,
    token::FileSet,
};
use gane_sema::{
    BasicType, FileId, GlobalInitializer, ObjectKind, PackageInput, TypeKind, analyze_package,
};

#[test]
fn exposes_semantic_facts_without_exposing_storage() {
    let mut files = FileSet::new();
    let (ast, errors) = parse_file(
        &mut files,
        "main.go",
        b"package main\nvar global int = 1\ntype Pair struct { value int }\nfunc main() { var pair Pair; pair.value = global }\n",
        Mode::default(),
    );
    assert!(errors.is_none(), "fixture should parse: {errors:?}");

    let result = analyze_package(PackageInput::single("main", FileId::from_raw(1), &ast));

    assert!(!result.has_errors());
    assert!(result.file_scope(FileId::from_raw(1)).is_some());
    assert!(result.file_scope(FileId::from_raw(2)).is_none());
    assert_eq!(result.name(result.package.name), Some("main"));
    assert!(matches!(
        result.type_of(result.predeclared.int).kind,
        TypeKind::Basic(BasicType::Int)
    ));

    let main = result.package_member("main").expect("main is declared");
    let ObjectKind::Func { signature, .. } = &result.object(main).kind else {
        panic!("main should be a function");
    };
    let TypeKind::Signature {
        params, results, ..
    } = &result.type_of(*signature).kind
    else {
        panic!("main should have a signature");
    };
    assert!(result.tuple(*params).unwrap().vars.is_empty());
    assert!(result.tuple(*results).unwrap().vars.is_empty());

    let Decl::FuncDecl(main_decl) = &ast.decls[2] else {
        panic!("third declaration should be main");
    };
    assert_eq!(result.definition(main_decl.name.node_id()), Some(main));

    let body = main_decl.body.as_ref().expect("main has a body");
    let Stmt::AssignStmt(assign) = &body.list[1] else {
        panic!("main should assign the field");
    };
    let Expr::SelectorExpr(selection) = &assign.lhs[0] else {
        panic!("assignment target should be a selector");
    };
    let Expr::Ident(global_use) = &assign.rhs[0] else {
        panic!("assignment source should be global");
    };
    let global = result.package_member("global").expect("global is declared");
    assert_eq!(result.use_of(global_use.node_id()), Some(global));
    assert!(result.type_and_value(global_use.node_id()).is_some());
    assert!(result.selection(selection.node_id()).is_some());
    assert!(matches!(
        result.global_initializer(global),
        Some(GlobalInitializer::Scalar(_))
    ));
}
