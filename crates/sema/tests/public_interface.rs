use gane_parser::{parser::parse_file, parser::Mode, token::FileSet};
use gane_sema::{analyze_package, BasicType, FileId, ObjectKind, PackageInput, TypeKind};

#[test]
fn exposes_semantic_facts_without_exposing_storage() {
    let mut files = FileSet::new();
    let (ast, errors) = parse_file(
        &mut files,
        "main.go",
        b"package main\nfunc main() {}\n",
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
}
