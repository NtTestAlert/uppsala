//! Regression tests for simple types derived from user-defined simple types.
//!
//! XSD 1.0 Part 2: a value of a derived simple type must be valid for the
//! built-in type at the root of its derivation chain and satisfy the facets
//! of every restriction step (patterns ORed within a step, ANDed across
//! steps).

mod common;
use common::parse;

use std::fs;
use std::path::{Path, PathBuf};

use uppsala::XsdValidator;

const XS: &str = r#"xmlns:xs="http://www.w3.org/2001/XMLSchema""#;

fn schema(body: &str) -> String {
    format!(r#"<xs:schema {}>{}</xs:schema>"#, XS, body)
}

fn build(xsd: &str) -> Result<XsdValidator, String> {
    let doc = parse(xsd).map_err(|e| format!("schema parse error: {}", e))?;
    XsdValidator::from_schema(&doc).map_err(|e| e.to_string())
}

fn errors(validator: &XsdValidator, xml: &str) -> Vec<String> {
    let doc = parse(xml).expect("parse instance");
    validator
        .validate(&doc)
        .iter()
        .map(|e| e.to_string())
        .collect()
}

/// Assert the verdict for each `(value, valid)` pair on element `<e>`.
fn check_element_values(xsd: &str, cases: &[(&str, bool)]) {
    let validator = build(xsd).expect("schema builds");
    for (value, valid) in cases {
        let errs = errors(&validator, &format!("<e>{}</e>", value));
        assert_eq!(
            errs.is_empty(),
            *valid,
            "value {:?}: expected {}, got errors {:?}",
            value,
            if *valid { "valid" } else { "invalid" },
            errs
        );
    }
}

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

fn build_with_base(main: &str, main_path: &Path) -> Result<XsdValidator, String> {
    let doc = parse(main).map_err(|e| format!("schema parse error: {}", e))?;
    XsdValidator::from_schema_with_base_path(&doc, Some(main_path)).map_err(|e| e.to_string())
}

// ─── Derivation chains ─────────────────────────────────────

#[test]
fn base_pattern_applies_to_derived_type() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:pattern value="\d{3}"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:maxLength value="5"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="B"/>"#,
    );
    check_element_values(&xsd, &[("123", true), ("abcd", false), ("123456", false)]);
}

#[test]
fn three_step_chain_applies_every_step() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:minLength value="2"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:pattern value="[a-z]+"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="C"><xs:restriction base="B"><xs:maxLength value="4"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="C"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("abc", true),
            ("a", false),
            ("ab1", false),
            ("abcde", false),
        ],
    );
}

#[test]
fn base_built_in_lexical_check_applies_to_derived_date() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:date"/></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:maxInclusive value="2050-01-01"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="B"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("2020-01-31", true),
            ("2020-99-99", false),
            ("2020-01-01x", false),
            ("2051-01-01", false),
        ],
    );
}

#[test]
fn derived_date_range_compares_by_value_not_as_string() {
    // 2000-01-01-01:00 starts at 2000-01-01T01:00Z, after the bound, although
    // it sorts before "2000-01-01Z" as a string. 2000-01-01+01:00 starts at
    // 1999-12-31T23:00Z, inside the bound.
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:date"/></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:maxInclusive value="2000-01-01Z"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="B"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("1999-12-31Z", true),
            ("2000-01-01+01:00", true),
            ("2000-01-01-01:00", false),
        ],
    );
}

#[test]
fn derived_decimal_range_and_digits_compare_by_value() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:decimal"><xs:totalDigits value="5"/><xs:fractionDigits value="2"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:maxInclusive value="100"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="B"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("99.99", true),
            ("100.00", true),
            ("0100", true),
            ("100.01", false),
            ("9", true),
            ("1.234", false),
            ("abc", false),
            ("1e2", false),
        ],
    );
}

#[test]
fn derived_integer_keeps_base_lexical_and_digit_checks() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:nonNegativeInteger"><xs:totalDigits value="14"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:minExclusive value="0"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="B"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("7", true),
            ("10", true),
            ("abc", false),
            ("1.5", false),
            ("0", false),
            ("123456789012345", false),
        ],
    );
}

#[test]
fn base_length_facets_and_token_whitespace_apply() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:token"><xs:minLength value="1"/><xs:maxLength value="256"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="B"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("x", true),
            ("  abc  ", true),
            ("", false),
            ("   ", false),
            ("abcd", false),
        ],
    );
}

#[test]
fn most_derived_white_space_facet_applies() {
    // A preserves whitespace, so "  a   b " fails its pattern; B collapses it
    // to "a b" before A's pattern is checked.
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:pattern value="a b"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:whiteSpace value="collapse"/></xs:restriction></xs:simpleType>
           <xs:element name="a" type="A"/>
           <xs:element name="b" type="B"/>"#,
    );
    let validator = build(&xsd).expect("schema builds");
    assert!(!errors(&validator, "<a>  a   b </a>").is_empty());
    assert!(errors(&validator, "<b>  a   b </b>").is_empty());
    assert!(!errors(&validator, "<b>ab</b>").is_empty());
}

#[test]
fn base_white_space_facet_applies_to_derived_type() {
    // A collapses whitespace, so B's own pattern sees "a b" for "  a\t  b ".
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:whiteSpace value="collapse"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:pattern value="a b"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="B"/>"#,
    );
    check_element_values(&xsd, &[("a b", true), ("  a\t  b ", true), ("ab", false)]);
}

#[test]
fn patterns_are_ored_within_a_step_and_anded_across_steps() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:pattern value="[0-9]+"/><xs:pattern value="[a-z]+"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:pattern value=".{3}"/></xs:restriction></xs:simpleType>
           <xs:element name="a" type="A"/>
           <xs:element name="e" type="B"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("123", true),
            ("abc", true),
            ("12a", false),
            ("1234", false),
        ],
    );
    let validator = build(&xsd).expect("schema builds");
    assert!(errors(&validator, "<a>12345</a>").is_empty());
    assert!(errors(&validator, "<a>abcde</a>").is_empty());
    assert!(!errors(&validator, "<a>ab1</a>").is_empty());
}

#[test]
fn enumerations_apply_per_step() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:enumeration value="a"/><xs:enumeration value="b"/><xs:enumeration value="c"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:enumeration value="b"/><xs:enumeration value="c"/><xs:enumeration value="d"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="B"/>"#,
    );
    check_element_values(
        &xsd,
        &[("b", true), ("c", true), ("a", false), ("d", false)],
    );
}

#[test]
fn anonymous_element_type_restricting_named_type_keeps_base_facets() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:minLength value="1"/><xs:maxLength value="5"/></xs:restriction></xs:simpleType>
           <xs:element name="e"><xs:simpleType><xs:restriction base="A"/></xs:simpleType></xs:element>"#,
    );
    check_element_values(&xsd, &[("abc", true), ("", false), ("abcdef", false)]);
}

#[test]
fn anonymous_base_of_restriction_keeps_its_facets() {
    let xsd = schema(
        r#"<xs:element name="e"><xs:simpleType><xs:restriction>
             <xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="\d+"/></xs:restriction></xs:simpleType>
             <xs:maxLength value="3"/>
           </xs:restriction></xs:simpleType></xs:element>"#,
    );
    check_element_values(&xsd, &[("12", true), ("ab", false), ("1234", false)]);
}

#[test]
fn derived_type_in_element_with_complex_type_particles() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:int"/></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:minInclusive value="1"/></xs:restriction></xs:simpleType>
           <xs:element name="r"><xs:complexType><xs:sequence><xs:element name="c" type="B"/></xs:sequence></xs:complexType></xs:element>"#,
    );
    let validator = build(&xsd).expect("schema builds");
    assert!(errors(&validator, "<r><c>5</c></r>").is_empty());
    assert!(!errors(&validator, "<r><c>x</c></r>").is_empty());
    assert!(!errors(&validator, "<r><c>0</c></r>").is_empty());
}

#[test]
fn derived_types_apply_to_attributes() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:pattern value="\d{3}"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:maxLength value="5"/></xs:restriction></xs:simpleType>
           <xs:element name="e"><xs:complexType>
             <xs:attribute name="named" type="B"/>
             <xs:attribute name="inline"><xs:simpleType><xs:restriction base="A"/></xs:simpleType></xs:attribute>
           </xs:complexType></xs:element>"#,
    );
    let validator = build(&xsd).expect("schema builds");
    assert!(errors(&validator, r#"<e named="123" inline="456"/>"#).is_empty());
    assert!(!errors(&validator, r#"<e named="abcd"/>"#).is_empty());
    assert!(!errors(&validator, r#"<e inline="abcd"/>"#).is_empty());
}

#[test]
fn simple_content_extension_of_derived_type_keeps_base_facets() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:date"/></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:maxInclusive value="2050-01-01"/></xs:restriction></xs:simpleType>
           <xs:element name="e"><xs:complexType><xs:simpleContent><xs:extension base="B">
             <xs:attribute name="a" type="xs:string"/>
           </xs:extension></xs:simpleContent></xs:complexType></xs:element>"#,
    );
    check_element_values(&xsd, &[("2020-01-31", true), ("2020-99-99", false)]);
}

#[test]
fn list_of_derived_item_type_checks_whole_item_chain() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:pattern value="\d+"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="L"><xs:list itemType="B"/></xs:simpleType>
           <xs:simpleType name="L2"><xs:restriction base="L"><xs:maxLength value="2"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="L2"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("12 345", true),
            ("12 abc", false),
            ("12 3456", false),
            ("1 2 3", false),
        ],
    );
}

#[test]
fn list_with_anonymous_item_type_checks_items() {
    let xsd = schema(
        r#"<xs:element name="e"><xs:simpleType><xs:list>
             <xs:simpleType><xs:restriction base="xs:int"><xs:maxInclusive value="10"/></xs:restriction></xs:simpleType>
           </xs:list></xs:simpleType></xs:element>"#,
    );
    check_element_values(&xsd, &[("1 2 10", true), ("1 x", false), ("1 11", false)]);
}

// ─── Schema refusals ───────────────────────────────────────

#[test]
fn circular_derivation_is_refused() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="B"/></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="A"/></xs:simpleType>
           <xs:element name="e" type="A"/>"#,
    );
    let err = build(&xsd).err().expect("circular schema is refused");
    assert!(err.contains("Circular"), "{}", err);

    let self_ref = schema(r#"<xs:simpleType name="A"><xs:restriction base="A"/></xs:simpleType>"#);
    assert!(build(&self_ref).is_err());
}

#[test]
fn missing_base_type_is_refused() {
    let xsd = schema(
        r#"<xs:simpleType name="B"><xs:restriction base="Missing"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="B"/>"#,
    );
    let err = build(&xsd)
        .err()
        .expect("schema with a missing base is refused");
    assert!(err.contains("Missing"), "{}", err);
}

#[test]
fn undeclared_base_prefix_is_refused() {
    let xsd = schema(r#"<xs:simpleType name="B"><xs:restriction base="nope:A"/></xs:simpleType>"#);
    assert!(build(&xsd).is_err());
}

#[test]
fn complex_type_as_simple_base_is_refused() {
    let xsd = schema(
        r#"<xs:complexType name="C"><xs:sequence/></xs:complexType>
           <xs:simpleType name="B"><xs:restriction base="C"/></xs:simpleType>"#,
    );
    assert!(build(&xsd).is_err());
}

// ─── Composition ───────────────────────────────────────────

/// `m:TData` restricts `b:TData`, a type of the same local name in an
/// imported namespace. The base must be looked up in `urn:b`, not in the
/// derived type's own namespace.
#[test]
fn cross_namespace_base_is_resolved_through_its_prefix() {
    let dir = mkdir_unique("cross-ns-base");
    fs::write(
        dir.join("base.xsd"),
        format!(
            r#"<xs:schema {} targetNamespace="urn:b">
                 <xs:simpleType name="TData"><xs:restriction base="xs:date"><xs:pattern value="\d{{4}}-\d{{2}}-\d{{2}}"/></xs:restriction></xs:simpleType>
               </xs:schema>"#,
            XS
        ),
    )
    .unwrap();
    let main = format!(
        r#"<xs:schema {} xmlns:b="urn:b" xmlns:m="urn:m" targetNamespace="urn:m">
             <xs:import namespace="urn:b" schemaLocation="base.xsd"/>
             <xs:simpleType name="TData"><xs:restriction base="b:TData"><xs:maxInclusive value="2050-01-01"/></xs:restriction></xs:simpleType>
             <xs:element name="e" type="m:TData"/>
           </xs:schema>"#,
        XS
    );
    let main_path = dir.join("main.xsd");
    fs::write(&main_path, &main).unwrap();
    let validator = build_with_base(&main, &main_path).expect("schema builds");
    for (value, valid) in [
        ("2020-01-31", true),
        ("2020-99-99", false),
        ("2026-09-29Z", false),
        ("2051-01-01", false),
    ] {
        let errs = errors(&validator, &format!(r#"<e xmlns="urn:m">{}</e>"#, value));
        assert_eq!(errs.is_empty(), valid, "{}: {:?}", value, errs);
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn chameleon_include_resolves_unprefixed_bases() {
    let dir = mkdir_unique("chameleon-base");
    fs::write(
        dir.join("types.xsd"),
        schema(
            r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:pattern value="\d{3}"/></xs:restriction></xs:simpleType>
               <xs:simpleType name="B"><xs:restriction base="A"><xs:maxLength value="5"/></xs:restriction></xs:simpleType>"#,
        ),
    )
    .unwrap();
    let main = format!(
        r#"<xs:schema {} xmlns:m="urn:m" targetNamespace="urn:m">
             <xs:include schemaLocation="types.xsd"/>
             <xs:element name="e" type="m:B"/>
           </xs:schema>"#,
        XS
    );
    let main_path = dir.join("main.xsd");
    fs::write(&main_path, &main).unwrap();
    let validator = build_with_base(&main, &main_path).expect("schema builds");
    assert!(errors(&validator, r#"<e xmlns="urn:m">123</e>"#).is_empty());
    assert!(!errors(&validator, r#"<e xmlns="urn:m">abcd</e>"#).is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn redefined_simple_type_restricts_the_original() {
    let dir = mkdir_unique("redefine-simple");
    fs::write(
        dir.join("orig.xsd"),
        format!(
            r#"<xs:schema {} targetNamespace="urn:m">
                 <xs:simpleType name="T"><xs:restriction base="xs:string"><xs:maxLength value="5"/></xs:restriction></xs:simpleType>
               </xs:schema>"#,
            XS
        ),
    )
    .unwrap();
    let main = format!(
        r#"<xs:schema {} xmlns:m="urn:m" targetNamespace="urn:m">
             <xs:redefine schemaLocation="orig.xsd">
               <xs:simpleType name="T"><xs:restriction base="m:T"><xs:pattern value="[a-z]+"/></xs:restriction></xs:simpleType>
             </xs:redefine>
             <xs:element name="e" type="m:T"/>
           </xs:schema>"#,
        XS
    );
    let main_path = dir.join("main.xsd");
    fs::write(&main_path, &main).unwrap();
    let validator = build_with_base(&main, &main_path).expect("schema builds");
    assert!(errors(&validator, r#"<e xmlns="urn:m">abc</e>"#).is_empty());
    assert!(!errors(&validator, r#"<e xmlns="urn:m">abcdef</e>"#).is_empty());
    assert!(!errors(&validator, r#"<e xmlns="urn:m">ab1</e>"#).is_empty());
    let _ = fs::remove_dir_all(&dir);
}

// ─── Enumerations compare values after whitespace normalization ─────

#[test]
fn white_space_set_on_two_steps_uses_the_most_derived() {
    // A replaces whitespace, D collapses it; D's facet wins, so " a\tb "
    // becomes "a b" (length 3), not " a b " (length 5).
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:whiteSpace value="replace"/><xs:length value="3"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="D"><xs:restriction base="A"><xs:whiteSpace value="collapse"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="D"/>"#,
    );
    check_element_values(&xsd, &[(" a\tb ", true), ("a  b", true), ("ab", false)]);
}

// ─── Type name resolution ──────────────────────────────────

/// QName attribute values are whitespace-collapsed.
#[test]
fn whitespace_around_type_qnames_is_ignored() {
    let xsd = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="    xs:string "><xs:maxLength value="3"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="L"><xs:list itemType=" A&#9;"/></xs:simpleType>
           <xs:element name="e" type="&#10;L "/>"#,
    );
    check_element_values(&xsd, &[("ab cd", true), ("abcd", false)]);
}

/// Without a default namespace declaration, an unprefixed name is
/// `{absent}local`: a no-namespace schema's own type of that name, even when
/// it is named like a built-in type; otherwise the built-in type.
#[test]
fn unprefixed_name_of_a_defined_type_is_not_the_built_in() {
    let xsd = schema(
        r#"<xs:simpleType name="Name"><xs:restriction base="xs:string"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>
           <xs:element name="root"><xs:complexType><xs:sequence>
             <xs:element name="n" type="Name" minOccurs="0"/>
             <xs:element name="s" type="string" minOccurs="0"/>
           </xs:sequence></xs:complexType></xs:element>"#,
    );
    let validator = build(&xsd).expect("schema builds");
    assert!(errors(&validator, "<root><n>ABC</n><s>any text</s></root>").is_empty());
    assert!(!errors(&validator, "<root><n>ABCDE</n></root>").is_empty());
}

/// `base="Code"` with a default namespace `urn:o` means `{urn:o}Code`, even
/// when the target namespace has a type of the same local name.
#[test]
fn unprefixed_base_resolves_to_a_foreign_default_namespace() {
    let dir = mkdir_unique("default-ns-base");
    fs::write(
        dir.join("other.xsd"),
        format!(
            r#"<xs:schema {} targetNamespace="urn:o">
                 <xs:simpleType name="Code"><xs:restriction base="xs:string"><xs:pattern value="[A-Z]{{2}}"/></xs:restriction></xs:simpleType>
               </xs:schema>"#,
            XS
        ),
    )
    .unwrap();
    let main = format!(
        r#"<xs:schema {} xmlns="urn:o" xmlns:t="urn:t" targetNamespace="urn:t">
             <xs:import namespace="urn:o" schemaLocation="other.xsd"/>
             <xs:simpleType name="Code"><xs:restriction base="xs:string"/></xs:simpleType>
             <xs:simpleType name="D"><xs:restriction base="Code"/></xs:simpleType>
             <xs:element name="e" type="t:D"/>
           </xs:schema>"#,
        XS
    );
    let main_path = dir.join("main.xsd");
    fs::write(&main_path, &main).unwrap();
    let validator = build_with_base(&main, &main_path).expect("schema builds");
    assert!(errors(&validator, r#"<e xmlns="urn:t">AB</e>"#).is_empty());
    assert!(!errors(&validator, r#"<e xmlns="urn:t">abc</e>"#).is_empty());
    let _ = fs::remove_dir_all(&dir);
}

/// A redefined schema that itself redefines the same type: each level keeps
/// the definition it redefines, and every level's facets apply.
#[test]
fn chained_redefinition_of_a_simple_type() {
    let dir = mkdir_unique("redefine-chain");
    let ns = r#"xmlns:m="urn:m" targetNamespace="urn:m""#;
    fs::write(
        dir.join("c.xsd"),
        format!(
            r#"<xs:schema {} {}>
                 <xs:simpleType name="B"><xs:restriction base="xs:string"><xs:pattern value="[a-z]+"/></xs:restriction></xs:simpleType>
               </xs:schema>"#,
            XS, ns
        ),
    )
    .unwrap();
    fs::write(
        dir.join("b.xsd"),
        format!(
            r#"<xs:schema {} {}>
                 <xs:redefine schemaLocation="c.xsd">
                   <xs:simpleType name="B"><xs:restriction base="m:B"><xs:maxLength value="5"/></xs:restriction></xs:simpleType>
                 </xs:redefine>
               </xs:schema>"#,
            XS, ns
        ),
    )
    .unwrap();
    let main = format!(
        r#"<xs:schema {} {}>
             <xs:redefine schemaLocation="b.xsd">
               <xs:simpleType name="B"><xs:restriction base="m:B"><xs:minLength value="2"/></xs:restriction></xs:simpleType>
             </xs:redefine>
             <xs:element name="e" type="m:B"/>
           </xs:schema>"#,
        XS, ns
    );
    let main_path = dir.join("main.xsd");
    fs::write(&main_path, &main).unwrap();
    let validator = build_with_base(&main, &main_path).expect("schema builds");
    for (value, valid) in [
        ("abc", true),
        ("a", false),
        ("abcdef", false),
        ("AB", false),
    ] {
        let errs = errors(&validator, &format!(r#"<e xmlns="urn:m">{}</e>"#, value));
        assert_eq!(errs.is_empty(), valid, "{}: {:?}", value, errs);
    }
    let _ = fs::remove_dir_all(&dir);
}

// ─── Limits ────────────────────────────────────────────────

/// A derivation chain of 64 steps (the type and 63 bases) builds; 65 steps
/// are refused.
#[test]
fn derivation_chain_limit() {
    let chain = |steps: usize| {
        let mut body = String::new();
        for i in 0..steps - 1 {
            body.push_str(&format!(
                r#"<xs:simpleType name="S{}"><xs:restriction base="S{}"/></xs:simpleType>"#,
                i,
                i + 1
            ));
        }
        body.push_str(&format!(
            r#"<xs:simpleType name="S{}"><xs:restriction base="xs:int"/></xs:simpleType><xs:element name="e" type="S0"/>"#,
            steps - 1
        ));
        schema(&body)
    };
    let validator = build(&chain(64)).expect("64 steps build");
    assert!(errors(&validator, "<e>5</e>").is_empty());
    assert!(!errors(&validator, "<e>x</e>").is_empty());
    assert!(build(&chain(65)).is_err());
}

// ─── Schema errors in derivations ──────────────────────────

/// `from_schema` has no base path, so it does not load imports: a derived
/// simple type whose base is in an imported schema is refused instead of
/// being validated as a string.
#[test]
fn from_schema_without_base_path_refuses_an_imported_base() {
    let dir = mkdir_unique("no-base-path");
    fs::write(
        dir.join("base.xsd"),
        format!(
            r#"<xs:schema {} targetNamespace="urn:b">
                 <xs:simpleType name="T"><xs:restriction base="xs:int"/></xs:simpleType>
               </xs:schema>"#,
            XS
        ),
    )
    .unwrap();
    let main = format!(
        r#"<xs:schema {} xmlns:b="urn:b">
             <xs:import namespace="urn:b" schemaLocation="base.xsd"/>
             <xs:simpleType name="D"><xs:restriction base="b:T"><xs:maxInclusive value="5"/></xs:restriction></xs:simpleType>
             <xs:element name="e" type="D"/>
           </xs:schema>"#,
        XS
    );
    let main_path = dir.join("main.xsd");
    fs::write(&main_path, &main).unwrap();
    let err = build(&main).err().expect("imported base is not available");
    assert!(err.contains("urn:b"), "{}", err);
    let validator = build_with_base(&main, &main_path).expect("builds with a base path");
    assert!(errors(&validator, "<e>5</e>").is_empty());
    assert!(!errors(&validator, "<e>x</e>").is_empty());
    let _ = fs::remove_dir_all(&dir);
}

// ─── Unprefixed type names without a default namespace ─────

/// Write `files` into a fresh directory and build `files[0]` with that
/// directory as its base.
fn build_files(label: &str, files: &[(&str, String)]) -> (XsdValidator, PathBuf) {
    let dir = mkdir_unique(label);
    for (name, text) in files {
        fs::write(dir.join(name), text).unwrap();
    }
    let main_path = dir.join(files[0].0);
    let validator = build_with_base(&files[0].1, &main_path).expect("schema builds");
    (validator, dir)
}

fn assert_instances(validator: &XsdValidator, cases: &[(&str, bool)]) {
    for (xml, valid) in cases {
        let errs = errors(validator, xml);
        assert_eq!(errs.is_empty(), *valid, "{}: {:?}", xml, errs);
    }
}

/// Without a default namespace, an unprefixed QName is `{absent}local`. That
/// type may be defined in another document of the schema: an included one,
/// a chameleon-included one (which moves it into the including target
/// namespace) or an imported no-namespace one. It must not be taken for the
/// built-in type of the same name.
#[test]
fn unprefixed_names_without_a_default_namespace_see_the_composed_schema() {
    let name_type = schema(
        r#"<xs:simpleType name="Name"><xs:restriction base="xs:string"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>"#,
    );

    // No target namespace; the type is in an included document.
    let (validator, dir) = build_files(
        "unqualified-include",
        &[
            (
                "main.xsd",
                schema(
                    r#"<xs:include schemaLocation="types.xsd"/><xs:element name="e" type="Name"/>"#,
                ),
            ),
            ("types.xsd", name_type.clone()),
        ],
    );
    assert_instances(&validator, &[("<e>AB</e>", true), ("<e>ABCDE</e>", false)]);
    let _ = fs::remove_dir_all(&dir);

    // Chameleon includes: the reference and the type are in two different
    // no-namespace documents, both moved into urn:t.
    let (validator, dir) = build_files(
        "unqualified-chameleon",
        &[
            (
                "main.xsd",
                format!(
                    r#"<xs:schema {} targetNamespace="urn:t"><xs:include schemaLocation="elems.xsd"/><xs:include schemaLocation="types.xsd"/></xs:schema>"#,
                    XS
                ),
            ),
            ("types.xsd", name_type.clone()),
            (
                "elems.xsd",
                schema(r#"<xs:element name="e" type="Name"/><xs:element name="s" type="string"/>"#),
            ),
        ],
    );
    assert_instances(
        &validator,
        &[
            (r#"<t:e xmlns:t="urn:t">AB</t:e>"#, true),
            (r#"<t:e xmlns:t="urn:t">ABCDE</t:e>"#, false),
            // No type `string` anywhere: the built-in, as before.
            (r#"<t:s xmlns:t="urn:t">any text</t:s>"#, true),
        ],
    );
    let _ = fs::remove_dir_all(&dir);

    // A target namespace and an imported no-namespace document defining
    // `date`: the reference is that type, not xs:date.
    let (validator, dir) = build_files(
        "unqualified-import",
        &[
            (
                "main.xsd",
                format!(
                    r#"<xs:schema {} targetNamespace="urn:t"><xs:import schemaLocation="n.xsd"/><xs:element name="e" type="date"/></xs:schema>"#,
                    XS
                ),
            ),
            (
                "n.xsd",
                schema(
                    r#"<xs:simpleType name="date"><xs:restriction base="xs:string"><xs:pattern value="\d{2}\.\d{2}\.\d{4}"/></xs:restriction></xs:simpleType>"#,
                ),
            ),
        ],
    );
    assert_instances(
        &validator,
        &[
            (r#"<t:e xmlns:t="urn:t">01.01.2020</t:e>"#, true),
            (r#"<t:e xmlns:t="urn:t">2020-01-01</t:e>"#, false),
        ],
    );
    let _ = fs::remove_dir_all(&dir);

    // `{absent}Code` wins over a target-namespace type of the same name.
    let (validator, dir) = build_files(
        "unqualified-absent-first",
        &[
            (
                "main.xsd",
                format!(
                    r#"<xs:schema {} targetNamespace="urn:t"><xs:import schemaLocation="n.xsd"/>
                         <xs:simpleType name="Code"><xs:restriction base="xs:string"/></xs:simpleType>
                         <xs:element name="e" type="Code"/></xs:schema>"#,
                    XS
                ),
            ),
            (
                "n.xsd",
                schema(
                    r#"<xs:simpleType name="Code"><xs:restriction base="xs:string"><xs:pattern value="[A-Z]{2}"/></xs:restriction></xs:simpleType>"#,
                ),
            ),
        ],
    );
    assert_instances(
        &validator,
        &[
            (r#"<t:e xmlns:t="urn:t">AB</t:e>"#, true),
            (r#"<t:e xmlns:t="urn:t">abc</t:e>"#, false),
        ],
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The legacy readings of an unprefixed name without a default namespace
/// stay for schemas that are invalid under the `{absent}local` rule: a type
/// of the document's own target namespace, else a built-in type.
#[test]
fn unprefixed_names_without_a_default_namespace_keep_the_legacy_fallbacks() {
    let xsd = format!(
        r#"<xs:schema {} targetNamespace="urn:t">
             <xs:simpleType name="Name"><xs:restriction base="xs:string"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>
             <xs:simpleType name="Small"><xs:restriction base="int"><xs:maxInclusive value="5"/></xs:restriction></xs:simpleType>
             <xs:element name="n" type="Name"/>
             <xs:element name="s" type="Small"/>
             <xs:element name="l"><xs:simpleType><xs:list itemType="int"/></xs:simpleType></xs:element>
           </xs:schema>"#,
        XS
    );
    let validator = build(&xsd).expect("schema builds");
    assert_instances(
        &validator,
        &[
            (r#"<t:n xmlns:t="urn:t">ABC</t:n>"#, true),
            (r#"<t:n xmlns:t="urn:t">ABCDE</t:n>"#, false),
            (r#"<t:s xmlns:t="urn:t">5</t:s>"#, true),
            (r#"<t:s xmlns:t="urn:t">6</t:s>"#, false),
            (r#"<t:s xmlns:t="urn:t">x</t:s>"#, false),
            (r#"<t:l xmlns:t="urn:t">1 2</t:l>"#, true),
            (r#"<t:l xmlns:t="urn:t">1 x</t:l>"#, false),
        ],
    );
}

/// `count` simple types restricting the built-in `base` (written as given).
fn many_restrictions_of(base: &str, count: usize) -> String {
    let mut body = String::new();
    for i in 0..count {
        body.push_str(&format!(
            r#"<xs:simpleType name="T{i}"><xs:restriction base="{base}"><xs:maxLength value="5"/></xs:restriction></xs:simpleType>"#
        ));
    }
    body.push_str(r#"<xs:element name="e" type="T0"/>"#);
    schema(&body)
}

/// The fastest of three builds of `xsd`.
fn fastest_build(xsd: &str) -> std::time::Duration {
    (0..3)
        .map(|_| {
            let started = std::time::Instant::now();
            build(xsd).expect("schema builds");
            started.elapsed()
        })
        .min()
        .unwrap()
}

/// Benchmark: schema building stays linear in the number of unprefixed
/// built-in names, within a small factor of the same schema with prefixed
/// names. A relative bound only, and ignored by default, so that a loaded
/// machine cannot fail the suite; run it with
/// `cargo test --release --test xsd_derived_simple_types -- --ignored`.
/// The quadratic version was over 40 times slower at this size.
#[test]
#[ignore = "benchmark; run with --release -- --ignored"]
fn many_unprefixed_built_in_names_build_in_linear_time() {
    let unprefixed = many_restrictions_of("string", 8000);
    let validator = build(&unprefixed).expect("schema builds");
    assert!(errors(&validator, "<e>abc</e>").is_empty());
    assert!(!errors(&validator, "<e>abcdef</e>").is_empty());
    let unprefixed_time = fastest_build(&unprefixed);
    let prefixed_time = fastest_build(&many_restrictions_of("xs:string", 8000));
    assert!(
        unprefixed_time < prefixed_time * 8 + std::time::Duration::from_millis(50),
        "8000 unprefixed built-in bases took {:?}, prefixed {:?}",
        unprefixed_time,
        prefixed_time
    );
}

// ─── Positions: LF lines, 1-based byte columns ─────────────

/// Positions follow one convention on every input: only LF ends a line (a
/// lone CR does not, and CRLF counts once), and a column is the 1-based byte
/// offset in its line, so a multi-byte character counts once per byte.
#[test]
fn positions_count_lf_lines_and_byte_columns() {
    let validator = build(&schema(
        r#"<xs:element name="r"><xs:complexType><xs:sequence>
             <xs:element name="e" type="xs:int" maxOccurs="unbounded"/>
           </xs:sequence></xs:complexType></xs:element>"#,
    ))
    .expect("schema builds");
    let cases: &[(&str, &str, (usize, usize))] = &[
        ("LF", "<r>\n<e>1</e>\n<e>zz</e></r>", (3, 1)),
        ("CRLF", "<r>\r\n<e>1</e>\r\n  <e>zz</e></r>", (3, 3)),
        ("CR only", "<r>\r<e>1</e>\r<e>zz</e></r>", (1, 14)),
        // "zażółć" is 6 characters and 10 bytes.
        ("two-byte", "<r><!--zażółć--><e>zz</e></r>", (1, 21)),
        ("four-byte", "<r><!--\u{1F600}--><e>zz</e></r>", (1, 15)),
        (
            "two-byte after CRLF",
            "<r>\r\n<!--ą--><e>zz</e></r>",
            (2, 10),
        ),
        ("CR then LF later", "<r>\r<!--ą-->\n <e>zz</e></r>", (2, 2)),
    ];
    for (label, xml, (line, column)) in cases {
        let doc = parse(xml).expect("parse instance");
        let errs = validator.validate(&doc);
        assert_eq!(errs.len(), 1, "{}: {:?}", label, errs);
        assert_eq!(
            (errs[0].line, errs[0].column),
            (Some(*line), Some(*column)),
            "{}: {:?}",
            label,
            errs[0]
        );
        // The document reports the same position for the element.
        let root = doc.document_element().expect("root");
        let last = doc
            .children(root)
            .into_iter()
            .filter(|&child| doc.element(child).is_some())
            .last()
            .expect("e");
        assert_eq!(
            (doc.node_line(last), doc.node_column(last)),
            (*line, *column),
            "{}",
            label
        );
    }
}
