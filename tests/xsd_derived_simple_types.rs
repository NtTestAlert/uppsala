//! Regression tests for simple types derived from user-defined simple types
//! and for `xs:union`.
//!
//! XSD 1.0 Part 2: a value of a derived simple type must be valid for the
//! built-in type at the root of its derivation chain and satisfy the facets
//! of every restriction step (patterns ORed within a step, ANDed across
//! steps). A union accepts a value if any member type accepts it.

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
fn circular_union_is_refused() {
    let xsd = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int U"/></xs:simpleType>
           <xs:element name="e" type="U"/>"#,
    );
    assert!(build(&xsd).is_err());
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
fn missing_union_member_is_refused() {
    let xsd = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int Missing"/></xs:simpleType>"#,
    );
    assert!(build(&xsd).is_err());
}

/// `memberTypes` is a list of QNames separated by XML white space only
/// (#x20, #x9, #xA, #xD). Any other space character is part of the one
/// item it sits in, which is then not a QName, and the schema is refused.
#[test]
fn member_types_are_split_only_at_xml_white_space() {
    for (written, character) in [
        ("&#160;", '\u{a0}'),
        ("&#x2003;", '\u{2003}'),
        ("&#x3000;", '\u{3000}'),
    ] {
        let xsd = schema(&format!(
            r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int{written}xs:date"/></xs:simpleType>
               <xs:element name="e" type="U"/>"#
        ));
        let err = build(&xsd)
            .err()
            .expect("a memberTypes item that is not a QName is refused");
        assert!(
            err.contains(&format!("xs:int{character}xs:date")),
            "{written}: {err}"
        );
    }
    check_element_values(
        &schema(
            r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int &#9;&#10;&#13;xs:date"/></xs:simpleType>
               <xs:element name="e" type="U"/>"#,
        ),
        &[("3", true), ("2000-01-01", true), ("abc", false)],
    );
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

// ─── Unions ────────────────────────────────────────────────

#[test]
fn union_of_inline_members() {
    let xsd = schema(
        r#"<xs:simpleType name="U"><xs:union>
             <xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="\d{9}"/></xs:restriction></xs:simpleType>
             <xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="\d{14}"/></xs:restriction></xs:simpleType>
           </xs:union></xs:simpleType>
           <xs:element name="e" type="U"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("123456789", true),
            ("12345678901234", true),
            ("x", false),
            ("12345", false),
            ("", false),
        ],
    );
}

#[test]
fn union_of_member_types() {
    let xsd = schema(
        r#"<xs:simpleType name="P9"><xs:restriction base="xs:string"><xs:pattern value="\d{9}"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="P14"><xs:restriction base="xs:string"><xs:pattern value="\d{14}"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="U"><xs:union memberTypes="P9 P14"/></xs:simpleType>
           <xs:element name="e" type="U"/>"#,
    );
    check_element_values(
        &xsd,
        &[("123456789", true), ("12345678901234", true), ("x", false)],
    );
}

#[test]
fn union_of_built_in_and_mixed_members() {
    let xsd = schema(
        r#"<xs:simpleType name="Small"><xs:restriction base="xs:int"><xs:maxInclusive value="10"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="U"><xs:union memberTypes="Small xs:boolean">
             <xs:simpleType><xs:restriction base="xs:string"><xs:enumeration value="none"/></xs:restriction></xs:simpleType>
           </xs:union></xs:simpleType>
           <xs:element name="e" type="U"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("5", true),
            ("true", true),
            ("none", true),
            ("11", false),
            ("x", false),
        ],
    );
}

#[test]
fn restricted_union_applies_its_facets() {
    let xsd = schema(
        r#"<xs:simpleType name="U"><xs:union>
             <xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="\d{9}"/></xs:restriction></xs:simpleType>
             <xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="\d{14}"/></xs:restriction></xs:simpleType>
           </xs:union></xs:simpleType>
           <xs:simpleType name="R"><xs:restriction base="U"><xs:pattern value="1.*"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="E"><xs:restriction base="U"><xs:enumeration value="123456789"/></xs:restriction></xs:simpleType>
           <xs:element name="r" type="R"/>
           <xs:element name="e" type="E"/>"#,
    );
    let validator = build(&xsd).expect("schema builds");
    assert!(errors(&validator, "<r>123456789</r>").is_empty());
    assert!(errors(&validator, "<r>12345678901234</r>").is_empty());
    assert!(!errors(&validator, "<r>223456789</r>").is_empty());
    assert!(!errors(&validator, "<r>1x</r>").is_empty());
    assert!(errors(&validator, "<e>123456789</e>").is_empty());
    assert!(!errors(&validator, "<e>987654321</e>").is_empty());
}

#[test]
fn union_restricted_with_length_facet_is_refused() {
    let xsd = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int xs:boolean"/></xs:simpleType>
           <xs:simpleType name="R"><xs:restriction base="U"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>"#,
    );
    assert!(build(&xsd).is_err());
}

#[test]
fn union_applies_to_attributes_and_list_items() {
    let xsd = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int xs:boolean"/></xs:simpleType>
           <xs:simpleType name="L"><xs:list itemType="U"/></xs:simpleType>
           <xs:element name="e"><xs:complexType>
             <xs:attribute name="u" type="U"/>
             <xs:attribute name="l" type="L"/>
           </xs:complexType></xs:element>"#,
    );
    let validator = build(&xsd).expect("schema builds");
    assert!(errors(&validator, r#"<e u="7" l="1 true 2"/>"#).is_empty());
    assert!(!errors(&validator, r#"<e u="x"/>"#).is_empty());
    assert!(!errors(&validator, r#"<e l="1 x"/>"#).is_empty());
}

#[test]
fn empty_union_is_refused() {
    let xsd = schema(r#"<xs:simpleType name="U"><xs:union/></xs:simpleType>"#);
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

/// `xs:string` preserves whitespace and `xs:normalizedString` only replaces
/// it, so a padded value is a different value and not in the enumeration.
#[test]
fn enumeration_keeps_string_whitespace() {
    let string_enum = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:enumeration value="AB"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="D"><xs:restriction base="A"/></xs:simpleType>
           <xs:element name="e" type="D"/>"#,
    );
    check_element_values(
        &string_enum,
        &[
            ("AB", true),
            (" AB", false),
            ("AB ", false),
            ("AB\n", false),
        ],
    );

    let normalized_enum = schema(
        r#"<xs:simpleType name="C"><xs:restriction base="xs:normalizedString"><xs:enumeration value="PL"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="C"/>"#,
    );
    check_element_values(
        &normalized_enum,
        &[("PL", true), (" PL", false), ("PL\t", false)],
    );

    // A token collapses whitespace, so the padded value is the same value.
    let token_enum = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:token"><xs:enumeration value="PL"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(&token_enum, &[(" PL\n", true), ("P L", false)]);
}

/// XSD 1.0 Part 2 4.3.5: an enumeration's values belong to the **base**
/// type of the step that declares the facet, so a literal is read with the
/// base's whitespace mode, not with a whiteSpace facet of the same or a later
/// step. The instance value is normalized with the most-derived mode and
/// must then be one of those values.
#[test]
fn enumeration_literals_are_read_with_the_base_type_whitespace() {
    // The literal is the xs:string " a  b "; no collapsed value equals it.
    let same_step = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:string">
             <xs:whiteSpace value="collapse"/><xs:enumeration value=" a  b "/>
           </xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(
        &same_step,
        &[
            ("a b", false),
            (" a  b ", false),
            ("  a\tb ", false),
            ("ab", false),
        ],
    );

    // A's value is "a  b"; D collapses, so D's values are never "a  b".
    let later_step = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:enumeration value="a  b"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="D"><xs:restriction base="A"><xs:whiteSpace value="collapse"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="D"/>"#,
    );
    check_element_values(&later_step, &[("a  b", false), ("a b", false)]);
    // A itself still accepts its literal as written.
    let base_only = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:enumeration value="a  b"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="A"/>"#,
    );
    check_element_values(&base_only, &[("a  b", true), ("a b", false)]);

    // A collapsing base reads its literal collapsed; a derived step's
    // enumeration over it is then compared with the collapsed value.
    let collapsed_base = schema(
        r#"<xs:simpleType name="B"><xs:restriction base="xs:string"><xs:whiteSpace value="collapse"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="E"><xs:restriction base="B"><xs:enumeration value=" a  b "/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="E"/>"#,
    );
    check_element_values(
        &collapsed_base,
        &[("a b", true), ("  a\tb ", true), ("ab", false)],
    );

    // Built-in token collapses its enumeration literals too.
    let token = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:token"><xs:enumeration value=" PL "/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(&token, &[("PL", true), (" PL\n", true)]);
}

#[test]
fn enumeration_compares_values_not_lexical_forms() {
    let int_enum = schema(
        r#"<xs:simpleType name="A"><xs:restriction base="xs:int"><xs:enumeration value="1"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="D"><xs:restriction base="A"/></xs:simpleType>
           <xs:element name="e" type="D"/>"#,
    );
    check_element_values(
        &int_enum,
        &[
            ("1", true),
            ("01", true),
            ("+1", true),
            (" 1 ", true),
            ("2", false),
        ],
    );

    let decimal_enum = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:decimal"><xs:enumeration value="1.50"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(
        &decimal_enum,
        &[("1.5", true), ("01.500", true), ("1.51", false)],
    );

    let float_enum = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:float"><xs:enumeration value="1.0"/><xs:enumeration value="NaN"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(
        &float_enum,
        &[("1", true), ("1E0", true), ("NaN", true), ("1.5", false)],
    );

    let boolean_enum = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:boolean"><xs:enumeration value="true"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(
        &boolean_enum,
        &[("true", true), ("1", true), ("false", false)],
    );

    let datetime_enum = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:dateTime"><xs:enumeration value="2020-01-01T00:00:00Z"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(
        &datetime_enum,
        &[
            ("2020-01-01T01:00:00+01:00", true),
            ("2020-01-01T00:00:00.000Z", true),
            ("2020-01-01T00:00:00", false),
        ],
    );
}

/// A union value is compared with the enumeration in the value space of
/// the member that accepts it; the literal is read by the union too.
#[test]
fn union_enumeration_compares_member_values() {
    let whitespace = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:string xs:int"/></xs:simpleType>
           <xs:simpleType name="R"><xs:restriction base="U"><xs:enumeration value="1"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="R"/>"#,
    );
    // "1 " is accepted by xs:string (first member) and is not the string "1".
    check_element_values(&whitespace, &[("1", true), ("1 ", false)]);

    let int_member = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int"/></xs:simpleType>
           <xs:simpleType name="R"><xs:restriction base="U"><xs:enumeration value="1"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="R"/>"#,
    );
    check_element_values(&int_member, &[("1", true), ("01", true), ("2", false)]);

    // The literal "01" is the int 1 (xs:int is the first member to accept it).
    let literal_member = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int xs:string"/></xs:simpleType>
           <xs:simpleType name="R"><xs:restriction base="U"><xs:enumeration value="01"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="R"/>"#,
    );
    check_element_values(&literal_member, &[("1", true), ("01", true), ("x", false)]);
}

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

#[test]
fn union_members_are_tried_in_declaration_order() {
    // Both members accept "a  b"; the first one decides how it is normalized.
    let string_first = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:string xs:token"/></xs:simpleType>
           <xs:simpleType name="R"><xs:restriction base="U"><xs:enumeration value="a b"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="R"/>"#,
    );
    check_element_values(&string_first, &[("a b", true), ("a  b", false)]);

    let token_first = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:token xs:string"/></xs:simpleType>
           <xs:simpleType name="R"><xs:restriction base="U"><xs:enumeration value="a b"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="R"/>"#,
    );
    check_element_values(&token_first, &[("a b", true), ("a  b", true)]);
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

/// An unprefixed QName resolves to the default namespace in scope, so a
/// user type named like a built-in type (`Name`) is that user type.
#[test]
fn unprefixed_type_names_use_the_default_namespace() {
    let xsd = format!(
        r#"<xs:schema {} xmlns="urn:t" targetNamespace="urn:t">
             <xs:simpleType name="Name"><xs:restriction base="xs:string"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>
             <xs:simpleType name="D"><xs:restriction base="Name"/></xs:simpleType>
             <xs:simpleType name="U"><xs:union memberTypes="Name"/></xs:simpleType>
             <xs:element name="d" type="D"/>
             <xs:element name="u" type="U"/>
             <xs:element name="n" type="Name"/>
             <xs:element name="a"><xs:complexType><xs:attribute name="v" type="Name"/></xs:complexType></xs:element>
           </xs:schema>"#,
        XS
    );
    let validator = build(&xsd).expect("schema builds");
    for (xml, valid) in [
        (r#"<d xmlns="urn:t">AB</d>"#, true),
        (r#"<d xmlns="urn:t">ABCDE</d>"#, false),
        (r#"<u xmlns="urn:t">ABCDE</u>"#, false),
        (r#"<n xmlns="urn:t">ABCDE</n>"#, false),
        (r#"<a xmlns="urn:t" v="ABCDE"/>"#, false),
    ] {
        let errs = errors(&validator, xml);
        assert_eq!(errs.is_empty(), valid, "{}: {:?}", xml, errs);
    }

    // With the XSD namespace as the default namespace, unprefixed names are
    // the built-in types.
    let xsd_default = r#"<schema xmlns="http://www.w3.org/2001/XMLSchema">
          <simpleType name="S"><restriction base="int"><maxInclusive value="5"/></restriction></simpleType>
          <element name="e" type="S"/>
        </schema>"#;
    check_element_values(xsd_default, &[("5", true), ("6", false), ("x", false)]);
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

/// XSD 1.0 src-simple-type.4: a union may not reference itself through
/// `memberTypes`, directly or through another union.
#[test]
fn circular_union_through_another_union_is_refused() {
    let xsd = schema(
        r#"<xs:simpleType name="st"><xs:union memberTypes="xs:int xs:string st2"/></xs:simpleType>
           <xs:simpleType name="st2"><xs:union memberTypes="st"/></xs:simpleType>"#,
    );
    let err = build(&xsd).err().expect("circular union is refused");
    assert!(err.contains("Circular"), "{}", err);
}

// ─── Build-time refusal of unresolved references ───────────

#[test]
fn anonymous_union_restriction_with_other_facets_is_refused() {
    let xsd = schema(
        r#"<xs:element name="e"><xs:simpleType><xs:restriction>
             <xs:simpleType><xs:union memberTypes="xs:int xs:string"/></xs:simpleType>
             <xs:maxLength value="3"/>
           </xs:restriction></xs:simpleType></xs:element>"#,
    );
    assert!(build(&xsd).is_err());
}

// ─── Limits ────────────────────────────────────────────────

/// `N0` .. `N{levels-1}` unions, each with the next as its only member,
/// ending in an `xs:int` restriction. `reverse` names them leaf first.
fn nested_unions(levels: usize, reverse: bool) -> String {
    let name = |i: usize| {
        if reverse {
            format!("N{:04}", 5000 - i)
        } else {
            format!("N{:04}", i)
        }
    };
    let mut body = String::new();
    for i in 0..levels {
        body.push_str(&format!(
            r#"<xs:simpleType name="{}"><xs:union memberTypes="{}"/></xs:simpleType>"#,
            name(i),
            name(i + 1)
        ));
    }
    body.push_str(&format!(
        r#"<xs:simpleType name="{}"><xs:restriction base="xs:int"/></xs:simpleType><xs:element name="e" type="{}"/>"#,
        name(levels),
        name(0)
    ));
    schema(&body)
}

/// Nesting deeper than 64 levels is refused at build whatever the order of
/// the type names; 64 levels build and validate.
#[test]
fn nesting_limit_does_not_depend_on_type_names() {
    for reverse in [false, true] {
        let validator = build(&nested_unions(64, reverse)).expect("64 levels build");
        assert!(errors(&validator, "<e>5</e>").is_empty());
        assert!(!errors(&validator, "<e>x</e>").is_empty());
        let err = build(&nested_unions(65, reverse))
            .err()
            .expect("65 levels are refused");
        assert!(err.contains("deeper than 64"), "{}", err);
    }
}

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

/// Unions that share member unions form a DAG: `U{i} = union(U{i+1} W{i+1})`
/// with `W{i}` a restriction of `U{i}`. Each member union is evaluated once
/// per value, so an invalid value is refused quickly instead of after 2^n
/// member checks. The unit test
/// `union_sharing_member_unions_selects_each_member_once` counts the work;
/// this one checks the verdicts.
#[test]
fn union_sharing_member_unions_validates() {
    let levels = 24;
    let mut body = String::new();
    for i in 0..levels {
        body.push_str(&format!(
            r#"<xs:simpleType name="U{i}"><xs:union memberTypes="U{} W{}"/></xs:simpleType>
               <xs:simpleType name="W{}"><xs:restriction base="U{}"/></xs:simpleType>"#,
            i + 1,
            i + 1,
            i + 1,
            i + 1
        ));
    }
    body.push_str(&format!(
        r#"<xs:simpleType name="U{levels}"><xs:restriction base="xs:int"/></xs:simpleType><xs:element name="e" type="U0"/>"#
    ));
    let validator = build(&schema(&body)).expect("schema builds");
    assert!(errors(&validator, "<e>5</e>").is_empty());
    assert!(!errors(&validator, "<e>x</e>").is_empty());
}

// ─── Range facets on float, double and duration ────────────

#[test]
fn float_and_double_ranges_compare_by_value() {
    let float_min = schema(
        r#"<xs:simpleType name="F"><xs:restriction base="xs:float"><xs:minInclusive value="0.5"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="D"><xs:restriction base="F"/></xs:simpleType>
           <xs:element name="e" type="D"/>"#,
    );
    check_element_values(
        &float_min,
        &[
            ("0.75", true),
            ("1E0", true),
            ("INF", true),
            ("1e-5", false),
            ("-INF", false),
            // XSD 1.1 (which the crate follows for float/double values):
            // NaN is not comparable, so it cannot satisfy a bound. XSD 1.0
            // ordered NaN above every value and would accept it here.
            ("NaN", false),
        ],
    );

    let double_max = schema(
        r#"<xs:simpleType name="G"><xs:restriction base="xs:double"><xs:maxInclusive value="100"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="G"/>"#,
    );
    check_element_values(
        &double_max,
        &[
            ("5E1", true),
            ("9e1", true),
            ("1.0E2", true),
            ("1e3", false),
            ("INF", false),
        ],
    );
}

#[test]
fn duration_ranges_use_the_partial_order() {
    let max_one_day = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:duration"><xs:maxInclusive value="P1D"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(
        &max_one_day,
        &[
            ("PT24H", true),
            ("PT23H59M59.5S", true),
            ("-P1Y", true),
            ("P0Y2D", false),
            ("PT24H0.1S", false),
        ],
    );

    // P30D against P1M depends on the month, so it is not comparable.
    let max_one_month = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:duration"><xs:maxInclusive value="P1M"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(
        &max_one_month,
        &[
            ("P27D", true),
            ("P1M", true),
            ("P30D", false),
            ("P32D", false),
            ("P1Y", false),
        ],
    );
}

// ─── Digits ────────────────────────────────────────────────

#[test]
fn total_digits_counts_significant_digits() {
    let xsd = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:decimal"><xs:totalDigits value="3"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="D"><xs:restriction base="T"/></xs:simpleType>
           <xs:element name="e" type="D"/>"#,
    );
    check_element_values(
        &xsd,
        &[
            ("1.5", true),
            ("0001.5", true),
            ("1.500", true),
            ("-00.120", true),
            ("0", true),
            ("100", true),
            ("1234", false),
            ("12.34", false),
        ],
    );
}

// ─── Schema errors in derivations ──────────────────────────

#[test]
fn unknown_built_in_type_is_refused() {
    for body in [
        r#"<xs:simpleType name="A"><xs:restriction base="xs:strin"/></xs:simpleType>"#,
        r#"<xs:simpleType name="L"><xs:list itemType="xs:strin"/></xs:simpleType>"#,
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int xs:strin"/></xs:simpleType>"#,
        r#"<xs:attribute name="a" type="xs:strin"/>"#,
    ] {
        let err = build(&schema(body))
            .err()
            .expect("unknown built-in is refused");
        assert!(err.contains("strin"), "{}", err);
    }
}

#[test]
fn fixed_facet_cannot_be_changed_by_a_derived_type() {
    let fixed = r#"<xs:simpleType name="A"><xs:restriction base="xs:string"><xs:maxLength value="5" fixed="true"/></xs:restriction></xs:simpleType>"#;
    let changed = format!(
        r#"{}<xs:simpleType name="D"><xs:restriction base="A"><xs:maxLength value="10"/></xs:restriction></xs:simpleType>"#,
        fixed
    );
    let err = build(&schema(&changed))
        .err()
        .expect("changing a fixed facet is refused");
    assert!(err.contains("maxLength"), "{}", err);

    let repeated = format!(
        r#"{}<xs:simpleType name="D"><xs:restriction base="A"><xs:maxLength value="5"/><xs:pattern value="[a-z]*"/></xs:restriction></xs:simpleType>"#,
        fixed
    );
    assert!(build(&schema(&repeated)).is_ok());

    // Range facets compare as values: 1.0 is the fixed 1.
    let range = r#"<xs:simpleType name="A"><xs:restriction base="xs:decimal"><xs:minInclusive value="1" fixed="true"/></xs:restriction></xs:simpleType>
                   <xs:simpleType name="D"><xs:restriction base="A"><xs:minInclusive value="1.0"/></xs:restriction></xs:simpleType>
                   <xs:simpleType name="E"><xs:restriction base="A"><xs:minInclusive value="2"/></xs:restriction></xs:simpleType>"#;
    let err = build(&schema(range))
        .err()
        .expect("E changes a fixed facet");
    assert!(err.contains("'E'"), "{}", err);
}

/// A range facet value whose year is too far from zero to place on the
/// timeline is never compared (see
/// `years_too_far_from_zero_to_place_on_the_timeline_are_refused`), so it
/// can neither restate nor change a fixed facet. The schema is refused for
/// that reason, naming the value, and not as a change.
#[test]
fn fixed_range_facet_with_a_year_too_far_from_zero_is_refused_by_name() {
    let huge = format!("1{}", "0".repeat(37));
    let derived = |fixed: &str, restated: &str| {
        schema(&format!(
            r#"<xs:simpleType name="B"><xs:restriction base="xs:gYear"><xs:maxInclusive value="{fixed}" fixed="true"/></xs:restriction></xs:simpleType>
               <xs:simpleType name="D"><xs:restriction base="B"><xs:maxInclusive value="{restated}"/></xs:restriction></xs:simpleType>
               <xs:element name="e" type="D"/>"#
        ))
    };
    for (fixed, restated) in [(&*huge, &*huge), ("2100", &*huge), (&*huge, "2100")] {
        let err = build(&derived(fixed, restated))
            .err()
            .expect("an unplaceable fixed range facet is refused");
        assert!(
            err.contains(&format!("'{huge}' has a year too far from zero"))
                && !err.contains("changes the value"),
            "{fixed} / {restated}: {err}"
        );
    }
    assert!(build(&derived("2100", "2100")).is_ok());
    let err = build(&derived("2100", "2000"))
        .err()
        .expect("changing a fixed facet is refused");
    assert!(
        err.contains("changes the value of facet 'maxInclusive'"),
        "{err}"
    );
}

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

// ─── Float literals round once ─────────────────────────────

/// An `xs:float` literal maps to the nearest single-precision value.
/// `1.0000000596046447753906250001` is just above the midpoint between 1 and
/// the next float, so it is that next float, not 1; rounding through a
/// double first lands on the midpoint and ties to 1.
#[test]
fn float_literals_round_directly_to_single_precision() {
    let above_midpoint = "1.0000000596046447753906250001";
    let below_midpoint = "1.0000000596046447753906249999";
    let enumeration = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:float"><xs:enumeration value="1"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(
        &enumeration,
        &[("1", true), (above_midpoint, false), (below_midpoint, true)],
    );
    let max_one = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:float"><xs:maxInclusive value="1"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(&max_one, &[("1", true), (above_midpoint, false)]);
    // Doubles keep their own precision: the same literal is above 1.
    let double_max = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:double"><xs:maxInclusive value="1"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(&double_max, &[("1", true), (below_midpoint, false)]);
}

/// XSD 1.1 float/double equality for enumerations: `-0` equals `0` and `NaN`
/// is identical to itself.
#[test]
fn float_enumeration_follows_xsd_1_1_equality() {
    let xsd = schema(
        r#"<xs:simpleType name="T"><xs:restriction base="xs:float"><xs:enumeration value="0"/><xs:enumeration value="NaN"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="T"/>"#,
    );
    check_element_values(
        &xsd,
        &[("-0", true), ("0.0", true), ("NaN", true), ("INF", false)],
    );
}

// ─── Union validation stays linear ─────────────────────────

/// The fastest of three `validate` runs of `xml`.
fn fastest_validation(validator: &XsdValidator, xml: &str) -> std::time::Duration {
    let doc = parse(xml).expect("parse instance");
    (0..3)
        .map(|_| {
            let started = std::time::Instant::now();
            let errs = validator.validate(&doc);
            let elapsed = started.elapsed();
            assert!(errs.is_empty(), "{:?}", errs.first());
            elapsed
        })
        .min()
        .unwrap()
}

/// A restricted union of `xs:int` and `xs:string` with ten literals, and the
/// same enumeration on `xs:string`, each for a sequence of `e` elements.
fn union_and_string_enumerations() -> (XsdValidator, XsdValidator) {
    let literals: String = (0..10)
        .map(|i| format!(r#"<xs:enumeration value="v{}"/>"#, i))
        .collect();
    let sequence = r#"<xs:element name="r"><xs:complexType><xs:sequence>
             <xs:element name="e" type="R" maxOccurs="unbounded"/>
           </xs:sequence></xs:complexType></xs:element>"#;
    let union = build(&schema(&format!(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int xs:string"/></xs:simpleType>
           <xs:simpleType name="R"><xs:restriction base="U">{}</xs:restriction></xs:simpleType>{}"#,
        literals, sequence
    )))
    .expect("schema builds");
    let string = build(&schema(&format!(
        r#"<xs:simpleType name="R"><xs:restriction base="xs:string">{}</xs:restriction></xs:simpleType>{}"#,
        literals, sequence
    )))
    .expect("schema builds");
    (union, string)
}

/// Benchmark: neither the rejected member of a union nor the union's
/// enumeration literals may cost time proportional to the document for
/// every value, so 40 000 values of a restricted union validate within a
/// small factor of the same enumeration on `xs:string`. The quadratic
/// version took seconds in a release build. A relative bound only, and
/// ignored by default, so that a loaded machine cannot fail the suite; run
/// it with `cargo test --release --test xsd_derived_simple_types --
/// --ignored`. The crate's unit tests count the work of both parts:
/// `node_positions_read_the_input_once` and
/// `union_enumeration_literals_are_read_once_per_type`.
#[test]
#[ignore = "benchmark; run with --release -- --ignored"]
fn many_union_values_validate_in_linear_time() {
    let (union, string) = union_and_string_enumerations();
    let body: String = (0..40_000).map(|_| "<e>v9</e>\n").collect();
    let good = format!("<r>\n{}</r>", body);
    let union_time = fastest_validation(&union, &good);
    let string_time = fastest_validation(&string, &good);
    assert!(
        union_time < string_time * 8 + std::time::Duration::from_millis(50),
        "40 000 union values took {:?}, as xs:string {:?}",
        union_time,
        string_time
    );
}

/// The error of the last of many union values carries its own line.
#[test]
fn union_value_errors_carry_their_own_position() {
    let (union, _) = union_and_string_enumerations();
    let body: String = (0..2_000).map(|_| "<e>v9</e>\n").collect();
    let bad = format!("<r>\n{}<e>zz</e></r>", body);
    let doc = parse(&bad).expect("parse instance");
    let errs = union.validate(&doc);
    assert_eq!(errs.len(), 1, "{:?}", errs);
    assert_eq!(errs[0].line, Some(2_002), "{:?}", errs[0]);
    assert_eq!(errs[0].column, Some(1), "{:?}", errs[0]);
}

/// The union keeps its readings of its enumeration literals (the unit test
/// `union_enumeration_literals_are_read_once_per_type` counts the reads); a
/// literal with a prefix is still read in the scope of each value, and
/// changing the validator's settings reads them again.
#[test]
fn union_enumeration_literal_readings_follow_scope_and_settings() {
    let xsd = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int xs:QName"/></xs:simpleType>
           <xs:simpleType name="R"><xs:restriction base="U"><xs:enumeration value="1"/><xs:enumeration value="p:x"/></xs:restriction></xs:simpleType>
           <xs:element name="r"><xs:complexType><xs:sequence>
             <xs:element name="e" type="R" maxOccurs="unbounded"/>
           </xs:sequence></xs:complexType></xs:element>"#,
    );
    let validator = build(&xsd).expect("schema builds");
    assert_instances(
        &validator,
        &[
            ("<r><e>01</e><e>1</e><e>+1</e></r>", true),
            ("<r><e>1</e><e>2</e></r>", false),
            (r#"<r xmlns:p="urn:p"><e>p:x</e><e>1</e></r>"#, true),
            (r#"<r xmlns:p="urn:p"><e>p:y</e></r>"#, false),
            ("<r><e>p:x</e></r>", false),
        ],
    );

    // anyURI accepts a space only in lenient mode, for the value and the
    // literal alike.
    let xsd = schema(
        r#"<xs:simpleType name="U"><xs:union memberTypes="xs:int xs:anyURI"/></xs:simpleType>
           <xs:simpleType name="R"><xs:restriction base="U"><xs:enumeration value="a b"/></xs:restriction></xs:simpleType>
           <xs:element name="e" type="R"/>"#,
    );
    let mut validator = build(&xsd).expect("schema builds");
    assert!(!errors(&validator, "<e>a b</e>").is_empty());
    // `1` reaches the enumeration, so the literals are read, strictly.
    assert!(!errors(&validator, "<e>1</e>").is_empty());
    validator.set_lenient(true);
    assert!(errors(&validator, "<e>a b</e>").is_empty());
}

// ─── +INF (pinned) ─────────────────────────────────────────

/// Pinned, deliberately: `+INF` is refused for `xs:float` and `xs:double`,
/// as in 0.10.1. XSD 1.1, whose value semantics the crate follows for these
/// types, allows it; the W3C 2006 suite (MS DataTypes `float018`,
/// `double018`) expects the XSD 1.0 refusal. The refusal fails closed.
#[test]
fn plus_inf_is_refused_for_float_and_double() {
    for ty in ["xs:float", "xs:double"] {
        check_element_values(
            &schema(&format!(r#"<xs:element name="e" type="{}"/>"#, ty)),
            &[
                ("INF", true),
                ("-INF", true),
                ("NaN", true),
                ("+INF", false),
                ("+1.5", true),
            ],
        );
    }
    check_element_values(
        &schema(
            r#"<xs:simpleType name="F"><xs:restriction base="xs:double"><xs:minInclusive value="0"/></xs:restriction></xs:simpleType>
               <xs:element name="e" type="F"/>"#,
        ),
        &[("INF", true), ("+INF", false)],
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

// ─── Chameleon includes of modules that import ─────────────

const NAME_MAX_3: &str = r#"<xs:simpleType name="Name"><xs:restriction base="xs:string"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>"#;

/// An unprefixed name without a default namespace resolves to a type of an
/// included no-namespace document in every place a type name occurs: a list
/// item type, a union member, a `simpleContent` base and the base of an
/// anonymous type.
#[test]
fn unprefixed_names_resolve_across_documents_in_every_position() {
    let (validator, dir) = build_files(
        "unqualified-positions",
        &[
            (
                "main.xsd",
                schema(
                    r#"<xs:include schemaLocation="types.xsd"/>
                       <xs:simpleType name="L"><xs:list itemType="Name"/></xs:simpleType>
                       <xs:simpleType name="U"><xs:union memberTypes="Name xs:int"/></xs:simpleType>
                       <xs:complexType name="SC"><xs:simpleContent><xs:extension base="Name"><xs:attribute name="a" type="xs:string"/></xs:extension></xs:simpleContent></xs:complexType>
                       <xs:simpleType name="AN"><xs:restriction><xs:simpleType><xs:restriction base="Name"/></xs:simpleType><xs:minLength value="1"/></xs:restriction></xs:simpleType>
                       <xs:element name="r"><xs:complexType><xs:sequence>
                         <xs:element name="l" type="L" minOccurs="0"/><xs:element name="u" type="U" minOccurs="0"/>
                         <xs:element name="sc" type="SC" minOccurs="0"/><xs:element name="an" type="AN" minOccurs="0"/>
                       </xs:sequence></xs:complexType></xs:element>"#,
                ),
            ),
            ("types.xsd", schema(NAME_MAX_3)),
        ],
    );
    assert_instances(
        &validator,
        &[
            ("<r><l>AB CD</l><u>AB</u><sc>AB</sc><an>AB</an></r>", true),
            ("<r><l>AB ABCDE</l></r>", false),
            ("<r><u>ABCDE</u></r>", false),
            ("<r><u>12345</u></r>", true),
            ("<r><sc>ABCDE</sc></r>", false),
            ("<r><an>ABCDE</an></r>", false),
        ],
    );
    let _ = fs::remove_dir_all(&dir);
}

// ─── Schema documents that are not loaded ──────────────────

/// Characters that Unicode calls white space but XML does not (XML white
/// space is only `#x20`, `#x9`, `#xD` and `#xA`): no-break space, em space,
/// ideographic space, next line.
const NON_XML_SPACES: [char; 4] = ['\u{A0}', '\u{2003}', '\u{3000}', '\u{85}'];

/// A value padded with a character that is white space in Unicode but not in
/// XML is not in the lexical space of a numeric, temporal, boolean or name
/// type, whether the type is the built-in or a restriction of it. XML white
/// space around the value is still collapsed away.
#[test]
fn values_padded_with_non_xml_whitespace_are_refused() {
    let types = [
        ("decimal", "3.5"),
        ("integer", "3"),
        ("byte", "3"),
        ("nonNegativeInteger", "3"),
        ("int", "3"),
        ("boolean", "true"),
        ("float", "1.5"),
        ("dateTime", "2026-09-29T10:00:00Z"),
        ("date", "2026-09-29"),
        ("time", "10:00:00"),
        ("gYear", "2026"),
        ("duration", "P1D"),
        ("NCName", "a"),
        ("Name", "a"),
        ("QName", "a"),
        ("anyURI", "a"),
    ];
    for (name, value) in types {
        let direct = schema(&format!(r#"<xs:element name="e" type="xs:{}"/>"#, name));
        let derived = schema(&format!(
            r#"<xs:simpleType name="R"><xs:restriction base="xs:{}"/></xs:simpleType><xs:element name="e" type="R"/>"#,
            name
        ));
        for xsd in [direct, derived] {
            let validator = build(&xsd).expect("schema builds");
            for good in [
                value.to_string(),
                format!(" {} ", value),
                format!("\t{}\n", value),
            ] {
                let errs = errors(&validator, &format!("<e>{}</e>", good));
                assert!(errs.is_empty(), "xs:{} {:?}: {:?}", name, good, errs);
            }
            if name == "anyURI" {
                // U+00A0 is a legal anyURI character (it is escaped when the
                // URI is used), so the padded value stays valid.
                continue;
            }
            for c in NON_XML_SPACES {
                for bad in [format!("{}{}", c, value), format!("{}{}", value, c)] {
                    let errs = errors(&validator, &format!("<e>{}</e>", bad));
                    assert!(!errs.is_empty(), "xs:{} {:?} was accepted", name, bad);
                }
            }
        }
    }
}

/// Range, `totalDigits` and `fractionDigits` facets see the value as it is:
/// a padded value is refused, not trimmed and compared.
#[test]
fn range_facets_do_not_trim_non_xml_whitespace() {
    check_element_values(
        &schema(
            r#"<xs:element name="e"><xs:simpleType><xs:restriction base="xs:decimal"><xs:totalDigits value="9"/><xs:fractionDigits value="6"/><xs:minInclusive value="0"/><xs:maxInclusive value="100"/></xs:restriction></xs:simpleType></xs:element>"#,
        ),
        &[
            ("23", true),
            ("23.5", true),
            ("\u{A0}23", false),
            ("23.5\u{A0}", false),
        ],
    );
    check_element_values(
        &schema(
            r#"<xs:simpleType name="D"><xs:restriction base="xs:dateTime"><xs:whiteSpace value="collapse"/></xs:restriction></xs:simpleType>
<xs:element name="e"><xs:simpleType><xs:restriction base="D"><xs:minInclusive value="2025-09-01T00:00:00Z"/></xs:restriction></xs:simpleType></xs:element>"#,
        ),
        &[
            ("2026-09-29T10:00:00Z", true),
            ("\u{A0}2026-09-29T10:00:00Z", false),
            ("2026-09-29T10:00:00Z\u{A0}", false),
        ],
    );
    check_element_values(
        &schema(
            r#"<xs:simpleType name="N"><xs:restriction base="xs:nonNegativeInteger"><xs:totalDigits value="14"/></xs:restriction></xs:simpleType>
<xs:element name="e"><xs:simpleType><xs:restriction base="N"><xs:minExclusive value="0"/></xs:restriction></xs:simpleType></xs:element>"#,
        ),
        &[
            ("1", true),
            ("\u{A0}1", false),
            ("\u{202F}1", false),
            ("\u{2028}1", false),
        ],
    );
}

/// List items are separated by XML white space only: a no-break space is
/// part of an item, not a separator.
#[test]
fn list_items_are_separated_by_xml_whitespace_only() {
    check_element_values(
        &schema(
            r#"<xs:simpleType name="L"><xs:list itemType="xs:int"/></xs:simpleType>
<xs:element name="e"><xs:simpleType><xs:restriction base="L"><xs:maxLength value="3"/></xs:restriction></xs:simpleType></xs:element>"#,
        ),
        &[
            ("1 2\t3", true),
            ("1\u{A0}2", false),
            ("1 2\u{2003}3", false),
        ],
    );
    check_element_values(
        &schema(r#"<xs:element name="e" type="xs:NMTOKENS"/>"#),
        &[("a b", true), ("a\u{A0}b", false)],
    );
    check_element_values(
        &schema(
            r#"<xs:simpleType name="T"><xs:restriction base="xs:NMTOKENS"/></xs:simpleType><xs:element name="e" type="T"/>"#,
        ),
        &[("a\tb", true), ("a\u{3000}b", false)],
    );
}

/// Character data made of a non-XML space is not white space: element-only
/// and empty content refuse it.
#[test]
fn element_only_content_refuses_non_xml_whitespace_text() {
    let validator = build(&schema(
        r#"<xs:element name="r"><xs:complexType><xs:sequence><xs:element name="a" type="xs:string"/></xs:sequence></xs:complexType></xs:element>
<xs:element name="e"><xs:complexType/></xs:element>"#,
    ))
    .expect("schema builds");
    for (xml, valid) in [
        ("<r> \n\t<a/> </r>", true),
        ("<r>\u{A0}<a/></r>", false),
        ("<r><a/>\u{2003}</r>", false),
        ("<e/>", true),
        ("<e>\u{A0}</e>", false),
    ] {
        let errs = errors(&validator, xml);
        assert_eq!(errs.is_empty(), valid, "{:?}: {:?}", xml, errs);
    }
}

/// Binary values may hold XML white space between their characters, and no
/// other.
#[test]
fn binary_values_allow_only_xml_whitespace() {
    check_element_values(
        &schema(r#"<xs:element name="e" type="xs:base64Binary"/>"#),
        &[
            ("QUJD", true),
            ("QU JD", true),
            ("QUJD\u{A0}", false),
            ("QU\u{A0}JD", false),
        ],
    );
    check_element_values(
        &schema(r#"<xs:element name="e" type="xs:hexBinary"/>"#),
        &[
            ("0F", true),
            (" 0F ", true),
            ("\u{A0}0F", false),
            ("0F\u{2003}", false),
        ],
    );
}
