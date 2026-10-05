//! Regression tests for XSD schema composition (`xs:import`, `xs:include`,
//! `xs:redefine`) interacting with content models.
//!
//! Each test that needs sibling schema files writes them to a unique
//! tempdir and passes the schema path to `from_schema_with_base_path` so
//! `schemaLocation` resolution works. No external test fixture files.

mod common;
use common::parse;

use std::fs;
use std::path::PathBuf;

use uppsala::XsdValidator;

fn mkdir_unique(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "uppsala-test-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn validate(schema: &str, schema_path: &std::path::Path, instance: &str) -> Vec<String> {
    let schema_doc = parse(schema).expect("parse schema");
    let validator = XsdValidator::from_schema_with_base_path(&schema_doc, Some(schema_path))
        .expect("build validator");
    let doc = parse(instance).expect("parse instance");
    validator
        .validate(&doc)
        .into_iter()
        .map(|e| format!("{}", e))
        .collect()
}

/// Control case: same-namespace `xs:element ref="..."` inside an unbounded
/// choice in mixed content. This works correctly today and is included so
/// the cross-namespace regression below can be compared against a known-good
/// baseline.
#[test]
fn same_namespace_ref_in_unbounded_choice_mixed_content() {
    let dir = mkdir_unique("same-ns-choice");
    let schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:m="urn:test:m"
           targetNamespace="urn:test:m"
           elementFormDefault="qualified">
  <xs:element name="ref">
    <xs:complexType><xs:attribute name="term" type="xs:string"/></xs:complexType>
  </xs:element>
  <xs:element name="p">
    <xs:complexType mixed="true">
      <xs:choice minOccurs="0" maxOccurs="unbounded">
        <xs:element ref="m:ref"/>
        <xs:element name="b" type="xs:string"/>
      </xs:choice>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let schema_path = dir.join("schema.xsd");
    fs::write(&schema_path, schema).unwrap();

    let instance = r#"<m:p xmlns:m="urn:test:m">
Text <m:ref term="x"/> and <m:b>bold</m:b> and <m:ref term="y"/> more.
</m:p>"#;

    let errors = validate(schema, &schema_path, instance);
    fs::remove_dir_all(&dir).ok();

    assert!(
        errors.is_empty(),
        "same-namespace ref in unbounded choice should validate, got: {:?}",
        errors
    );
}

/// Regression: when an `xs:element ref="foreign:name"` (resolved across an
/// `xs:import` boundary) appears inside an unbounded choice in mixed content,
/// validation incorrectly reports `Unexpected element ... after choice` for
/// the second and subsequent occurrences. This is the "cross-namespace ref
/// in unbounded choice" bug.
///
/// Schema layout:
///   inner.xsd — defines a global element `i:ref` in namespace `urn:test:inner`
///   outer.xsd — imports inner, declares `o:p` whose content model is
///               mixed + unbounded choice over `i:ref` and a local `b`.
///
/// Instance: `<o:p>` containing two `<i:ref/>` interleaved with text and a
/// `<o:b>`. By spec this is valid (choice is unbounded; mixed allows text).
#[test]
fn cross_namespace_ref_in_unbounded_choice_mixed_content() {
    let dir = mkdir_unique("cross-ns-choice");

    let inner = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           targetNamespace="urn:test:inner"
           elementFormDefault="qualified">
  <xs:element name="ref">
    <xs:complexType><xs:attribute name="term" type="xs:string"/></xs:complexType>
  </xs:element>
</xs:schema>"#;
    fs::write(dir.join("inner.xsd"), inner).unwrap();

    let outer = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           xmlns:o="urn:test:outer"
           targetNamespace="urn:test:outer"
           elementFormDefault="qualified">
  <xs:import namespace="urn:test:inner" schemaLocation="inner.xsd"/>
  <xs:element name="p">
    <xs:complexType mixed="true">
      <xs:choice minOccurs="0" maxOccurs="unbounded">
        <xs:element ref="i:ref"/>
        <xs:element name="b" type="xs:string"/>
      </xs:choice>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let outer_path = dir.join("outer.xsd");
    fs::write(&outer_path, outer).unwrap();

    let instance = r#"<o:p xmlns:o="urn:test:outer" xmlns:i="urn:test:inner">
Text with <i:ref term="x"/> and <o:b>bold</o:b> and <i:ref term="y"/>.
</o:p>"#;

    let errors = validate(outer, &outer_path, instance);
    fs::remove_dir_all(&dir).ok();

    assert!(
        errors.is_empty(),
        "cross-namespace ref in unbounded choice should validate, got: {:?}",
        errors
    );
}

/// Regression: `xs:import` must resolve `schemaLocation` when the caller passes
/// the schema *directory* as `base_path`, not just the schema *file*.
///
/// The public entry points hand a directory to `from_schema_with_base_path`:
/// the schema is supplied as a string (no file of its own), and the pyuppsala
/// `XsdValidator.from_file(schema_xml, base_path)` / etree
/// `XMLSchema(file=...)` facade passes `os.path.dirname(file)`. Composition
/// used to do `base_path.parent()` unconditionally, treating that directory as
/// a file and stripping one level, so every `xs:import`/`xs:include` silently
/// failed to resolve and *all* imported declarations (types **and** the global
/// element used to validate the instance root) went missing -- surfacing as
/// "No element declaration found for '<root>'". This test passes the directory
/// (as the real callers do) and asserts the imported global element resolves.
///
/// The sibling tests above pass the schema *file* path, whose `.parent()` is
/// the directory, so they validated correctly even with the bug and never
/// exercised this path.
#[test]
fn import_resolves_with_directory_base_path() {
    let dir = mkdir_unique("import-dir-base");

    // Imported schema declares a global element (and its type) in its own
    // namespace -- this is the element used to validate the instance root.
    let inner = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           targetNamespace="urn:test:inner"
           elementFormDefault="qualified">
  <xs:element name="Thing" type="i:ThingType"/>
  <xs:complexType name="ThingType">
    <xs:attribute name="id" type="xs:string"/>
  </xs:complexType>
</xs:schema>"#;
    fs::write(dir.join("inner.xsd"), inner).unwrap();

    // Entry schema (different targetNamespace) only imports the inner one.
    let composite = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:test:aggregate" version="1.0">
  <xs:import namespace="urn:test:inner" schemaLocation="inner.xsd"/>
</xs:schema>"#;

    let instance = r#"<i:Thing xmlns:i="urn:test:inner" id="x"/>"#;

    // Pass the DIRECTORY as base_path (what the public callers do), NOT the
    // schema file path.
    let schema_doc = parse(composite).expect("parse composite schema");
    let validator = XsdValidator::from_schema_with_base_path(&schema_doc, Some(dir.as_path()))
        .expect("build validator from directory base_path");
    let doc = parse(instance).expect("parse instance");
    let errors: Vec<String> = validator
        .validate(&doc)
        .into_iter()
        .map(|e| format!("{e}"))
        .collect();
    fs::remove_dir_all(&dir).ok();

    assert!(
        errors.is_empty(),
        "imported global element must resolve when base_path is the schema \
         directory, got: {errors:?}",
    );
}

/// Regression: a `base_path` directory that does not exist must fail closed,
/// not silently resolve `schemaLocation` against its (existing) parent.
///
/// The effective base directory is computed by reducing only a *known regular
/// file* to its parent; a missing/unreadable path is kept as the directory so
/// the canonicalize-or-reject guard fires. A `!is_dir()` test would instead
/// treat the missing directory as a file and fall back to its parent, which may
/// canonicalize successfully and re-open the contained-resolution hole.
#[test]
fn missing_base_directory_fails_closed() {
    let parent = mkdir_unique("missing-base-parent");
    let missing = parent.join("does-not-exist");

    let schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:test:agg" version="1.0">
  <xs:import namespace="urn:test:inner" schemaLocation="inner.xsd"/>
</xs:schema>"#;

    let schema_doc = parse(schema).expect("parse schema");
    let built = XsdValidator::from_schema_with_base_path(&schema_doc, Some(missing.as_path()));
    fs::remove_dir_all(&parent).ok();

    assert!(
        built.is_err(),
        "a missing base directory must fail closed (canonicalize error), not \
         resolve imports against its parent",
    );
}

/// Regression: `<xs:attribute ref="foreign:attr"/>` across an `xs:import`
/// boundary. Before the fix, the prefix was stripped and the lookup keyed
/// against the outer schema's targetNamespace, so the imported global
/// attribute was never found and the `use="required"` constraint wasn't
/// enforced.
#[test]
fn cross_namespace_attribute_ref_required() {
    let dir = mkdir_unique("cross-ns-attr");

    let inner = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           targetNamespace="urn:test:inner"
           elementFormDefault="qualified"
           attributeFormDefault="qualified">
  <xs:attribute name="lang" type="xs:string"/>
</xs:schema>"#;
    fs::write(dir.join("inner.xsd"), inner).unwrap();

    let outer = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           xmlns:o="urn:test:outer"
           targetNamespace="urn:test:outer"
           elementFormDefault="qualified">
  <xs:import namespace="urn:test:inner" schemaLocation="inner.xsd"/>
  <xs:element name="p">
    <xs:complexType>
      <xs:attribute ref="i:lang" use="required"/>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let outer_path = dir.join("outer.xsd");
    fs::write(&outer_path, outer).unwrap();

    // Valid — attribute present.
    let ok_instance = r#"<o:p xmlns:o="urn:test:outer" xmlns:i="urn:test:inner" i:lang="en"/>"#;
    let errors = validate(outer, &outer_path, ok_instance);
    assert!(
        errors.is_empty(),
        "cross-namespace attribute ref should resolve, got: {:?}",
        errors
    );

    // Invalid — required foreign attribute missing. Pre-fix this would
    // ALSO have produced errors, but for the wrong reason (unresolved
    // local-namespace decl, not the real `use="required"` violation).
    let bad_instance = r#"<o:p xmlns:o="urn:test:outer"/>"#;
    let errors = validate(outer, &outer_path, bad_instance);
    fs::remove_dir_all(&dir).ok();
    assert!(
        !errors.is_empty(),
        "missing required cross-namespace attribute should fail validation"
    );
}

/// Regression: `<xs:attributeGroup ref="foreign:group"/>` across an
/// `xs:import` boundary. Pre-fix, the prefix was ignored and the lookup
/// keyed against the outer schema's targetNamespace, so the imported
/// group's attributes were silently dropped from the effective attribute
/// list — any required attributes declared in the group went unenforced.
#[test]
fn cross_namespace_attribute_group_ref() {
    let dir = mkdir_unique("cross-ns-ag");

    let inner = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           targetNamespace="urn:test:inner"
           elementFormDefault="qualified"
           attributeFormDefault="qualified">
  <xs:attributeGroup name="meta">
    <xs:attribute name="id" type="xs:string" use="required"/>
  </xs:attributeGroup>
</xs:schema>"#;
    fs::write(dir.join("inner.xsd"), inner).unwrap();

    let outer = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:i="urn:test:inner"
           xmlns:o="urn:test:outer"
           targetNamespace="urn:test:outer"
           elementFormDefault="qualified">
  <xs:import namespace="urn:test:inner" schemaLocation="inner.xsd"/>
  <xs:element name="p">
    <xs:complexType>
      <xs:attributeGroup ref="i:meta"/>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let outer_path = dir.join("outer.xsd");
    fs::write(&outer_path, outer).unwrap();

    // Missing imported required attribute must fail.
    let bad_instance = r#"<o:p xmlns:o="urn:test:outer"/>"#;
    let errors = validate(outer, &outer_path, bad_instance);
    fs::remove_dir_all(&dir).ok();
    assert!(
        !errors.is_empty(),
        "cross-namespace attributeGroup ref should contribute its required \
         attributes to the effective attribute list; got no errors which \
         means the group was silently dropped"
    );
}

/// Negative: an undeclared prefix in a `ref=` attribute must no longer
/// silently rebind to the schema's targetNamespace. Pre-fix, a typo like
/// `ref="nobdy:foo"` would quietly resolve against the outer schema; this
/// test pins the new fail-closed behaviour (lookup misses, particle does
/// not match anything in the instance).
#[test]
fn undeclared_prefix_in_ref_fails_closed() {
    let dir = mkdir_unique("undeclared-prefix");
    let schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           xmlns:m="urn:test:m"
           targetNamespace="urn:test:m"
           elementFormDefault="qualified">
  <xs:element name="p">
    <xs:complexType>
      <xs:sequence>
        <xs:element ref="nobdy:foo"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
    let schema_path = dir.join("schema.xsd");
    fs::write(&schema_path, schema).unwrap();

    // Instance that would have accidentally matched pre-fix (an element
    // `<m:foo/>` in targetNamespace) must now NOT match, because the ref
    // resolves to no-namespace.
    let instance = r#"<m:p xmlns:m="urn:test:m"><m:foo/></m:p>"#;
    let errors = validate(schema, &schema_path, instance);
    fs::remove_dir_all(&dir).ok();
    assert!(
        !errors.is_empty(),
        "undeclared-prefix ref must fail closed; instead the particle \
         silently matched an element in the wrong namespace"
    );
}

/// F-10: a schema that uses `xs:include schemaLocation="/etc/passwd"` or
/// any absolute path outside its own base directory must be rejected.
/// Before the fix, the loader `std::fs::read_to_string`d the path verbatim.
#[test]
fn absolute_schema_location_is_rejected() {
    let dir = std::env::temp_dir().join(format!(
        "uppsala-f10-abs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    fs::create_dir_all(&dir).unwrap();

    // Put the "secret" schema OUTSIDE the schema's base directory.
    let outside =
        std::env::temp_dir().join(format!("uppsala-f10-outside-{}.xsd", std::process::id()));
    fs::write(
        &outside,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="leaked" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();

    // The schema we hand the validator tries to include it by absolute path.
    let schema_path = dir.join("evil.xsd");
    let schema = format!(
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="{}"/>
  <xs:element name="x" type="xs:string"/>
</xs:schema>"#,
        outside.display()
    );
    fs::write(&schema_path, &schema).unwrap();

    let schema_doc = parse(&schema).unwrap();
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&schema_path));

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_file(&outside);

    let err = built
        .err()
        .expect("absolute schemaLocation must be rejected");
    let msg = format!("{}", err);
    assert!(
        msg.contains("escapes the schema's base directory")
            || msg.contains("absolute URI not supported"),
        "expected containment error, got: {}",
        msg
    );
}

/// F-10: `schemaLocation="../../../../etc/passwd"` that canonicalizes to
/// a path outside the schema's directory must be rejected.
#[test]
fn parent_traversal_schema_location_is_rejected() {
    let dir = mkdir_unique("f10-traversal");
    let nested = dir.join("sub");
    fs::create_dir_all(&nested).unwrap();

    // "Secret" file lives in `dir` (one level up from `nested`).
    let secret = dir.join("secret.xsd");
    fs::write(
        &secret,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="secret" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();

    // The schema's base dir is `nested/`; the include escapes upward.
    let schema_path = nested.join("evil.xsd");
    let schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="../secret.xsd"/>
  <xs:element name="x" type="xs:string"/>
</xs:schema>"#;
    fs::write(&schema_path, schema).unwrap();

    let schema_doc = parse(schema).unwrap();
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&schema_path));
    fs::remove_dir_all(&dir).ok();

    assert!(
        built.is_err(),
        "`../` traversal out of schema base dir must be rejected"
    );
}

/// F-10 positive control: an include within the same directory works fine.
#[test]
fn same_directory_include_is_allowed() {
    let dir = mkdir_unique("f10-same-dir");

    let inner_path = dir.join("inner.xsd");
    fs::write(
        &inner_path,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="inner_leaf" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();

    let schema_path = dir.join("outer.xsd");
    let schema = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="inner.xsd"/>
  <xs:element name="x" type="xs:string"/>
</xs:schema>"#;
    fs::write(&schema_path, schema).unwrap();

    let schema_doc = parse(schema).unwrap();
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&schema_path));
    fs::remove_dir_all(&dir).ok();

    let _validator = built.expect("same-dir include must be allowed");
}

/// F-11: a.xsd includes b.xsd which includes a.xsd. Before the fix this
/// recursed until the thread stack overflowed. With the visited-paths
/// set the second `xs:include` is short-circuited and the build succeeds.
#[test]
fn circular_include_terminates() {
    let dir = mkdir_unique("f11-circular");

    let a_path = dir.join("a.xsd");
    let b_path = dir.join("b.xsd");
    fs::write(
        &a_path,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="b.xsd"/>
  <xs:element name="a_leaf" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();
    fs::write(
        &b_path,
        r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="a.xsd"/>
  <xs:element name="b_leaf" type="xs:string"/>
</xs:schema>"#,
    )
    .unwrap();

    let schema_src = fs::read_to_string(&a_path).unwrap();
    let schema_doc = parse(&schema_src).unwrap();
    // Without the visited set this call recurses until SIGABRT.
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&a_path));
    fs::remove_dir_all(&dir).ok();
    assert!(
        built.is_ok(),
        "circular include must terminate cleanly, got: {:?}",
        built.err()
    );
}

/// F-11: include-nesting past `MAX_INCLUDE_DEPTH` errors with a clear
/// message instead of exhausting the stack.
#[test]
fn deep_include_chain_rejected() {
    let dir = mkdir_unique("f11-deep");
    // 20 schemas each including the next one. Exceeds MAX_INCLUDE_DEPTH = 16.
    let n = 20usize;
    for i in 0..n {
        let next = i + 1;
        let body = if next < n {
            format!(r#"<xs:include schemaLocation="s{}.xsd"/>"#, next)
        } else {
            String::new()
        };
        let path = dir.join(format!("s{}.xsd", i));
        fs::write(
            &path,
            format!(
                r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  {}
  <xs:element name="e{}" type="xs:string"/>
</xs:schema>"#,
                body, i
            ),
        )
        .unwrap();
    }

    let entry = dir.join("s0.xsd");
    let schema_src = fs::read_to_string(&entry).unwrap();
    let schema_doc = parse(&schema_src).unwrap();
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&entry));
    fs::remove_dir_all(&dir).ok();

    let err = built.err().expect("20-deep include chain must be rejected");
    assert!(
        format!("{}", err).contains("nesting exceeds maximum depth"),
        "expected include-depth error, got: {}",
        err
    );
}

/// When the caller supplies a `base_path` whose parent directory cannot
/// be canonicalized, the composition layer must fail closed rather than
/// silently dropping the F-10 containment check. Pre-fix, the
/// containment-anchor was `base_dir.canonicalize().ok()`, so any
/// canonicalize failure (missing dir, permission denied, race) collapsed
/// `canonical_base` to `None` and left the include path unchecked.
#[test]
fn uncanonicalizable_base_path_fails_closed() {
    // Construct a base_path whose parent directory does NOT exist.
    let bogus_parent = std::env::temp_dir().join(format!(
        "uppsala-nonexistent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let bogus_schema = bogus_parent.join("schema.xsd");
    assert!(
        !bogus_parent.exists(),
        "test precondition: parent must not exist"
    );

    // Schema body itself is benign; it just has to reach
    // process_schema_composition (which fires on any xs:include /
    // xs:redefine / xs:import — even one with a missing target).
    let schema_src = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:include schemaLocation="other.xsd"/>
  <xs:element name="x" type="xs:string"/>
</xs:schema>"#;
    let schema_doc = parse(schema_src).expect("parse");
    let built = uppsala::XsdValidator::from_schema_with_base_path(&schema_doc, Some(&bogus_schema));

    let err = built
        .err()
        .expect("uncanonicalizable base directory must fail closed");
    assert!(
        format!("{}", err).contains("canonicalize schema base directory"),
        "expected canonicalize error, got: {}",
        err
    );
}

/// Write `files` to a fresh directory and build from the first one.
fn build_files(label: &str, files: &[(&str, &str)]) -> XsdValidator {
    let dir = mkdir_unique(label);
    for (name, text) in files {
        fs::write(dir.join(name), text).unwrap();
    }
    let entry = dir.join(files[0].0);
    let schema_doc = parse(files[0].1).expect("parse schema");
    let built = XsdValidator::from_schema_with_base_path(&schema_doc, Some(&entry));
    fs::remove_dir_all(&dir).ok();
    built.expect("build validator")
}

fn is_valid(validator: &XsdValidator, instance: &str) -> bool {
    let doc = parse(instance).expect("parse instance");
    validator.validate(&doc).is_empty()
}

/// A `group` reference that is a complex type's content keeps its
/// `minOccurs`/`maxOccurs` when `xs:redefine` replaces the group: the
/// redefined group is re-resolved with the reference's occurrence.
#[test]
fn redefined_group_reference_keeps_its_occurrence() {
    let base = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:m" xmlns:m="urn:m" elementFormDefault="qualified">
<xs:group name="g"><xs:sequence><xs:element name="a" type="xs:int"/></xs:sequence></xs:group>
<xs:complexType name="T"><xs:group ref="m:g" minOccurs="0" maxOccurs="2"/></xs:complexType>
<xs:element name="r" type="m:T"/>
</xs:schema>"#;
    let main = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:m" xmlns:m="urn:m" elementFormDefault="qualified">
<xs:redefine schemaLocation="base.xsd">
<xs:group name="g"><xs:sequence><xs:group ref="m:g"/><xs:element name="b" type="xs:int" minOccurs="0"/></xs:sequence></xs:group>
</xs:redefine>
</xs:schema>"#;
    let validator = build_files(
        "redefine-group-occurs",
        &[("main.xsd", main), ("base.xsd", base)],
    );
    let r = |content: &str| format!(r#"<m:r xmlns:m="urn:m">{}</m:r>"#, content);
    // The redefined group, twice.
    assert!(is_valid(
        &validator,
        &r("<m:a>1</m:a><m:b>1</m:b><m:a>2</m:a>")
    ));
    assert!(is_valid(&validator, &r("<m:a>1</m:a><m:a>2</m:a>")));
    // A third occurrence exceeds maxOccurs="2".
    assert!(!is_valid(
        &validator,
        &r("<m:a>1</m:a><m:a>2</m:a><m:a>3</m:a>")
    ));
    // The redefinition applies: `c` is in neither group.
    assert!(!is_valid(&validator, &r("<m:a>1</m:a><m:c>1</m:c>")));
}

/// A prefixed `substitutionGroup` is resolved with the namespace bindings in
/// scope at the declaration, including those of `xs:schema`.
#[test]
fn substitution_group_prefix_resolves_with_in_scope_bindings() {
    let main = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:b" xmlns:b="urn:b" xmlns:a="urn:a" elementFormDefault="qualified">
<xs:import namespace="urn:a" schemaLocation="a.xsd"/>
<xs:element name="m" type="xs:string" substitutionGroup="a:h"/>
<xs:element name="m2" type="xs:string" substitutionGroup="q:h" xmlns:q="urn:a"/>
<xs:element name="h" type="xs:string"/>
<xs:element name="t"><xs:complexType><xs:sequence><xs:element ref="b:h"/></xs:sequence></xs:complexType></xs:element>
</xs:schema>"#;
    let a = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:a" xmlns:a="urn:a" elementFormDefault="qualified">
<xs:element name="h" type="xs:string" abstract="true"/>
<xs:element name="s"><xs:complexType><xs:sequence><xs:element ref="a:h"/></xs:sequence></xs:complexType></xs:element>
</xs:schema>"#;
    let validator = build_files(
        "substitution-group-prefix",
        &[("main.xsd", main), ("a.xsd", a)],
    );
    let ns = r#"xmlns:a="urn:a" xmlns:b="urn:b""#;
    // Members of a:h, the head declared on xs:schema and on the element.
    assert!(is_valid(
        &validator,
        &format!("<a:s {ns}><b:m>x</b:m></a:s>")
    ));
    assert!(is_valid(
        &validator,
        &format!("<a:s {ns}><b:m2>x</b:m2></a:s>")
    ));
    assert!(!is_valid(&validator, &format!("<a:s {ns}/>")));
    // Not members of the target namespace's own `h`.
    assert!(is_valid(
        &validator,
        &format!("<b:t {ns}><b:h>x</b:h></b:t>")
    ));
    assert!(!is_valid(
        &validator,
        &format!("<b:t {ns}><b:m>x</b:m></b:t>")
    ));
}

/// Only a reference to a global element declaration admits the members of
/// its substitution group; a local declaration with the head's expanded name
/// is another declaration (XSTS elemZ021f shape).
#[test]
fn local_element_named_like_a_head_admits_no_substitution() {
    let schema = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:m" xmlns:m="urn:m" elementFormDefault="qualified">
<xs:element name="e"/>
<xs:element name="e1" type="xs:int" substitutionGroup="m:e"/>
<xs:element name="local"><xs:complexType><xs:sequence><xs:element name="e" type="xs:string"/></xs:sequence></xs:complexType></xs:element>
<xs:element name="global"><xs:complexType><xs:sequence><xs:element ref="m:e"/></xs:sequence></xs:complexType></xs:element>
</xs:schema>"#;
    let validator =
        XsdValidator::from_schema(&parse(schema).expect("parse schema")).expect("build validator");
    let doc = |root: &str, child: &str| format!(r#"<m:{root} xmlns:m="urn:m">{child}</m:{root}>"#);
    assert!(is_valid(&validator, &doc("local", "<m:e>x</m:e>")));
    assert!(!is_valid(&validator, &doc("local", "<m:e1>1</m:e1>")));
    assert!(is_valid(&validator, &doc("global", "<m:e1>1</m:e1>")));
    assert!(!is_valid(&validator, &doc("global", "<m:e1>x</m:e1>")));
}

/// A `substitutionGroup` whose prefix has no binding names no head: the
/// build is refused, naming the prefix, in a no-namespace schema as in a
/// namespaced one (src-resolve).
#[test]
fn substitution_group_with_an_unbound_prefix_is_refused_at_build() {
    let schemas = [
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
<xs:element name="h" type="xs:string"/>
<xs:element name="m" type="xs:string" substitutionGroup="q:h"/>
<xs:element name="r"><xs:complexType><xs:sequence><xs:element ref="h"/></xs:sequence></xs:complexType></xs:element>
</xs:schema>"#,
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:m" xmlns:m="urn:m" elementFormDefault="qualified">
<xs:element name="h" type="xs:string"/>
<xs:element name="m" type="xs:string" substitutionGroup="q:h"/>
</xs:schema>"#,
    ];
    for schema in schemas {
        let err = XsdValidator::from_schema(&parse(schema).expect("parse schema"))
            .err()
            .expect("the build is refused");
        let message = err.to_string();
        assert!(
            message.contains("'q'") && message.contains("substitutionGroup"),
            "expected the unbound prefix to be named, got: {}",
            message
        );
    }
}

/// A `substitutionGroup` value is an `xs:QName`: its prefix and its local
/// part must each be an NCName. A part that is not, such as one ending in a
/// no-break space or starting with a digit, refuses the build, naming the
/// value. XML white space around the value is collapsed, and a name with
/// non-ASCII letters is an NCName.
#[test]
fn substitution_group_qname_parts_must_be_ncnames() {
    let schema = |head: &str, group: &str| {
        format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:m" xmlns:m="urn:m" elementFormDefault="qualified">
<xs:element name="{head}" type="xs:string"/>
<xs:element name="p" type="xs:string" substitutionGroup="{group}"/>
<xs:element name="r"><xs:complexType><xs:sequence><xs:element ref="m:{head}"/></xs:sequence></xs:complexType></xs:element>
</xs:schema>"#
        )
    };
    for group in ["m:h&#160;", "m:1h", "1m:h", "m:h&#x2003;", "m:h:i", "m:"] {
        let err = XsdValidator::from_schema(&parse(&schema("h", group)).expect("parse schema"))
            .err()
            .unwrap_or_else(|| panic!("the build is refused for {:?}", group));
        let message = err.to_string();
        assert!(
            message.contains("Invalid substitutionGroup QName"),
            "expected the QName {:?} to be refused by name, got: {}",
            group,
            message
        );
    }
    for (head, group) in [("h", " m:h "), ("h\u{e9}", "m:h\u{e9}"), ("h.-1", "m:h.-1")] {
        let validator = XsdValidator::from_schema(&parse(&schema(head, group)).expect("parse"))
            .unwrap_or_else(|e| panic!("{:?} builds: {}", group, e));
        assert!(
            is_valid(&validator, r#"<m:r xmlns:m="urn:m"><m:p>x</m:p></m:r>"#),
            "p is a member of the head named by {:?}",
            group
        );
    }
}

/// The prefix `xmlns` is never bound in a QName: it is not among an
/// element's in-scope namespaces, although it is reserved for declarations.
/// A `substitutionGroup` with that prefix refuses the build, naming the
/// prefix, as any undeclared prefix does.
#[test]
fn substitution_group_with_the_xmlns_prefix_is_refused() {
    let schema = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:m" xmlns:m="urn:m" elementFormDefault="qualified">
<xs:element name="h" type="xs:string"/>
<xs:element name="p" type="xs:string" substitutionGroup="xmlns:h"/>
<xs:element name="r"><xs:complexType><xs:sequence><xs:element ref="m:h"/></xs:sequence></xs:complexType></xs:element>
</xs:schema>"#;
    let err = XsdValidator::from_schema(&parse(schema).expect("parse schema"))
        .err()
        .expect("the build is refused");
    let message = err.to_string();
    assert!(
        message.contains("Undeclared namespace prefix 'xmlns'"),
        "expected the prefix to be refused by name, got: {}",
        message
    );
}

/// An unprefixed `substitutionGroup` is a QName: it takes the default
/// namespace in scope, or no namespace, never the target namespace by
/// default. Here the head is a no-namespace `h`, so the member does not
/// join the target namespace's `h`.
#[test]
fn unprefixed_substitution_group_uses_the_default_namespace() {
    let main = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:t" xmlns:t="urn:t" elementFormDefault="qualified">
<xs:import schemaLocation="n.xsd"/>
<xs:element name="h" type="xs:string"/>
<xs:element name="m" type="xs:string" substitutionGroup="h"/>
<xs:element name="m2" type="xs:string" substitutionGroup="h" xmlns="urn:t"/>
<xs:element name="r"><xs:complexType><xs:sequence><xs:element ref="t:h"/></xs:sequence></xs:complexType></xs:element>
</xs:schema>"#;
    let no_namespace = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
<xs:element name="h" type="xs:string"/>
<xs:element name="s"><xs:complexType><xs:sequence><xs:element ref="h"/></xs:sequence></xs:complexType></xs:element>
</xs:schema>"#;
    let validator = build_files(
        "unprefixed-substitution-group",
        &[("main.xsd", main), ("n.xsd", no_namespace)],
    );
    let t = r#"xmlns:t="urn:t""#;
    // `m` is a member of the no-namespace `h` only.
    assert!(is_valid(&validator, &format!("<s {t}><t:m>x</t:m></s>")));
    assert!(!is_valid(
        &validator,
        &format!("<t:r {t}><t:m>x</t:m></t:r>")
    ));
    // `m2` sees the default namespace `urn:t`: a member of `t:h` only.
    assert!(is_valid(
        &validator,
        &format!("<t:r {t}><t:m2>x</t:m2></t:r>")
    ));
    assert!(!is_valid(&validator, &format!("<s {t}><t:m2>x</t:m2></s>")));
    // The heads themselves.
    assert!(is_valid(
        &validator,
        &format!("<t:r {t}><t:h>x</t:h></t:r>")
    ));
    assert!(is_valid(&validator, "<s><h>x</h></s>"));
}

#[test]
fn redefined_simple_content_uses_original_value_type() {
    let dir = mkdir_unique("redefine-simple-content");
    fs::write(
        dir.join("base.xsd"),
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:complexType name="Amount"><xs:simpleContent>
        <xs:extension base="xs:decimal"/>
      </xs:simpleContent></xs:complexType>
    </xs:schema>"#,
    )
    .unwrap();
    let schema = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:redefine schemaLocation="base.xsd">
        <xs:complexType name="Amount"><xs:simpleContent>
          <xs:extension base="Amount">
            <xs:attribute name="unit" type="xs:string" use="required"/>
          </xs:extension>
        </xs:simpleContent></xs:complexType>
      </xs:redefine>
      <xs:element name="r" type="Amount"/>
    </xs:schema>"#;
    fs::write(dir.join("schema.xsd"), schema).unwrap();
    let validator = XsdValidator::from_schema_with_base_path(
        &parse(schema).unwrap(),
        Some(&dir.join("schema.xsd")),
    )
    .unwrap();
    fs::remove_dir_all(&dir).unwrap();
    for (xml, valid) in [
        (r#"<r unit="USD">12.50</r>"#, true),
        (r#"<r unit="USD">garbage</r>"#, false),
        ("<r>12.50</r>", false),
    ] {
        let errors = validator.validate(&parse(xml).unwrap());
        assert_eq!(errors.is_empty(), valid, "{xml}: {errors:?}");
    }
}
