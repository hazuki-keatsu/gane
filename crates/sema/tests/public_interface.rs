use gane_parser::{
    ast::{Decl, Expr, Spec, Stmt},
    parser::Mode,
    parser::parse_file,
    token::FileSet,
};
use gane_sema::{
    BasicType, ConstValue, FileId, GlobalInitializer, IntegerValue, ObjectKind, PackageInput,
    SelectionKind, TypeKind, analyze_package,
};

#[test]
fn exposes_semantic_facts_without_exposing_storage() {
    let mut files = FileSet::new();
    let (ast, errors) = parse_file(
        &mut files,
        "main.go",
        b"package main\nconst base = 1\nvar global int = base\ntype UserID int\ntype Pair struct { value int }\nfunc main() { var pair Pair; pair.value = global }\n",
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

    let Decl::GenDecl(base_decl) = &ast.decls[0] else {
        panic!("first declaration should be const");
    };
    let Spec::ValueSpec(base_spec) = &base_decl.specs[0] else {
        panic!("const declaration should contain a value spec");
    };
    let Expr::BasicLit(base_value) = &base_spec.values[0] else {
        panic!("const initializer should be a literal");
    };
    let base = result
        .definition(base_spec.names[0].node_id())
        .expect("const definition should be available");
    assert!(matches!(
        result.type_and_value(base_value.node_id()),
        Some(type_and_value)
            if type_and_value.constant == Some(ConstValue::Int(IntegerValue::from_u64(1)))
    ));

    let user_id = result
        .package_member("UserID")
        .expect("named type is declared");
    let ObjectKind::TypeName { named: user_id, .. } = result.object(user_id).kind else {
        panic!("UserID should be a named type");
    };
    assert_eq!(result.underlying_type(user_id), result.predeclared.int);
    assert!(!result.identical_types(user_id, result.predeclared.int));

    let Decl::FuncDecl(main_decl) = &ast.decls[4] else {
        panic!("fifth declaration should be main");
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
    let Decl::GenDecl(global_decl) = &ast.decls[1] else {
        panic!("second declaration should be global");
    };
    let Spec::ValueSpec(global_spec) = &global_decl.specs[0] else {
        panic!("global declaration should contain a value spec");
    };
    let Expr::Ident(base_use) = &global_spec.values[0] else {
        panic!("global initializer should use the const");
    };
    assert_eq!(result.use_of(base_use.node_id()), Some(base));
    assert!(matches!(
        result.type_and_value(base_use.node_id()),
        Some(type_and_value)
            if type_and_value.constant == Some(ConstValue::Int(IntegerValue::from_u64(1)))
    ));
    assert_eq!(result.use_of(global_use.node_id()), Some(global));
    assert!(matches!(
        result.type_and_value(global_use.node_id()),
        Some(type_and_value) if type_and_value.typ == result.predeclared.int && type_and_value.constant.is_none()
    ));
    let selection = result
        .selection(selection.node_id())
        .expect("field selection facts should be available");
    assert_eq!(selection.kind, SelectionKind::Field);
    assert_eq!(selection.index, [0]);
    assert!(!selection.indirect);
    assert!(matches!(
        result.global_initializer(global),
        Some(GlobalInitializer::Scalar(ConstValue::Int(value))) if value == &IntegerValue::from_u64(1)
    ));
}
