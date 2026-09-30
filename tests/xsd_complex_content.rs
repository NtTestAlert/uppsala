//! Regression tests for the content type of complex types derived by
//! `complexContent` extension or restriction, and for elements whose type
//! has simple content.
//!
//! XSD 1.0 Part 1 §3.4.2: a `complexContent` restriction has its own
//! explicit content, which is empty when it declares no particle; an
//! extension without particles has the base's content type, and an
//! extension with particles has the base's particle followed by its own.
//! cvc-complex-type.2.2: an element whose type has simple content has no
//! element children.

mod common;
use common::parse;

use uppsala::XsdValidator;

const XS: &str = r#"xmlns:xs="http://www.w3.org/2001/XMLSchema""#;

fn schema(body: &str) -> String {
    format!(r#"<xs:schema {}>{}</xs:schema>"#, XS, body)
}

fn schema_ns(tns: &str, body: &str) -> String {
    format!(
        r#"<xs:schema {} xmlns:m="{}" targetNamespace="{}">{}</xs:schema>"#,
        XS, tns, tns, body
    )
}

fn build(xsd: &str) -> XsdValidator {
    let doc = parse(xsd).expect("schema parses");
    XsdValidator::from_schema(&doc).expect("schema builds")
}

fn errors(validator: &XsdValidator, xml: &str) -> Vec<String> {
    let doc = parse(xml).expect("parse instance");
    validator
        .validate(&doc)
        .iter()
        .map(|e| e.to_string())
        .collect()
}

/// Assert the verdict for each `(instance, valid)` pair.
fn assert_instances(xsd: &str, cases: &[(&str, bool)]) {
    let validator = build(xsd);
    for (xml, valid) in cases {
        let errs = errors(&validator, xml);
        assert_eq!(
            errs.is_empty(),
            *valid,
            "instance {}: expected {}, got errors {:?}",
            xml,
            if *valid { "valid" } else { "invalid" },
            errs
        );
    }
}

const CT_X_INT: &str = r#"<xs:complexType name="CT"><xs:sequence><xs:element name="x" type="xs:int"/></xs:sequence></xs:complexType>"#;

#[test]
fn extension_without_particles_has_the_base_content() {
    // Anonymous and named extension, in a target namespace.
    let anonymous = schema_ns(
        "urn:m",
        &format!(
            r#"{}<xs:element name="r"><xs:complexType><xs:complexContent><xs:extension base="m:CT"/></xs:complexContent></xs:complexType></xs:element>"#,
            CT_X_INT
        ),
    );
    let named = schema_ns(
        "urn:m",
        &format!(
            r#"{}<xs:complexType name="D"><xs:complexContent><xs:extension base="m:CT"/></xs:complexContent></xs:complexType><xs:element name="r" type="m:D"/>"#,
            CT_X_INT
        ),
    );
    for xsd in [&anonymous, &named] {
        assert_instances(
            xsd,
            &[
                (r#"<m:r xmlns:m="urn:m"><x>1</x></m:r>"#, true),
                (r#"<m:r xmlns:m="urn:m"><x>abc</x></m:r>"#, false),
                (r#"<m:r xmlns:m="urn:m"/>"#, false),
                (r#"<m:r xmlns:m="urn:m"><x>1</x><y/></m:r>"#, false),
                (r#"<m:r xmlns:m="urn:m">text</m:r>"#, false),
                (r#"<m:r xmlns:m="urn:m"><x>1</x>text</m:r>"#, false),
                (r#"<m:r xmlns:m="urn:m"> <x>1</x> </m:r>"#, true),
            ],
        );
    }

    // Two extensions without particles in a row.
    assert_instances(
        &schema(&format!(
            r#"{}<xs:complexType name="D1"><xs:complexContent><xs:extension base="CT"/></xs:complexContent></xs:complexType><xs:complexType name="D2"><xs:complexContent><xs:extension base="D1"/></xs:complexContent></xs:complexType><xs:element name="r" type="D2"/>"#,
            CT_X_INT
        )),
        &[
            ("<r><x>1</x></r>", true),
            ("<r><x>abc</x></r>", false),
            ("<r/>", false),
        ],
    );
}

#[test]
fn extension_adding_only_attributes_keeps_the_base_content_and_checks_attributes() {
    assert_instances(
        &schema(&format!(
            r#"{}<xs:complexType name="D"><xs:complexContent><xs:extension base="CT"><xs:attribute name="a" type="xs:int" use="required"/></xs:extension></xs:complexContent></xs:complexType><xs:element name="r" type="D"/>"#,
            CT_X_INT
        )),
        &[
            (r#"<r a="1"><x>1</x></r>"#, true),
            (r#"<r a="1"><x>abc</x></r>"#, false),
            (r#"<r a="1"/>"#, false),
            (r#"<r><x>1</x></r>"#, false),
            (r#"<r a="z"><x>1</x></r>"#, false),
            (r#"<r a="1" b="2"><x>1</x></r>"#, false),
        ],
    );
}

#[test]
fn extension_of_a_choice_base_keeps_the_choice() {
    // A base whose content is a choice of sequences, extended with an
    // attribute only.
    let base = r#"<xs:complexType name="A"><xs:choice><xs:sequence><xs:element name="p" type="xs:int"/></xs:sequence><xs:sequence><xs:element name="z" type="xs:string"/></xs:sequence></xs:choice></xs:complexType>"#;
    assert_instances(
        &schema(&format!(
            r#"{}<xs:element name="r"><xs:complexType><xs:complexContent><xs:extension base="A"><xs:attribute name="t" type="xs:string"/></xs:extension></xs:complexContent></xs:complexType></xs:element>"#,
            base
        )),
        &[
            (r#"<r t="1"><p>1</p></r>"#, true),
            (r#"<r><z>a</z></r>"#, true),
            (r#"<r><p>x</p></r>"#, false),
            (r#"<r><p>1</p><z>a</z></r>"#, false),
            (r#"<r/>"#, false),
        ],
    );

    // The same base extended with a sequence: the choice, then the sequence.
    assert_instances(
        &schema(&format!(
            r#"{}<xs:element name="r"><xs:complexType><xs:complexContent><xs:extension base="A"><xs:sequence><xs:element name="y" type="xs:int"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType></xs:element>"#,
            base
        )),
        &[
            ("<r><p>1</p><y>2</y></r>", true),
            ("<r><z>a</z><y>2</y></r>", true),
            ("<r><y>2</y></r>", false),
            ("<r><p>1</p></r>", false),
            ("<r><p>x</p><y>2</y></r>", false),
        ],
    );
}

#[test]
fn extension_keeps_the_occurrence_of_the_base_sequence() {
    // The base's sequence may occur twice; the extension's element follows.
    assert_instances(
        &schema(
            r#"<xs:complexType name="B"><xs:sequence maxOccurs="2"><xs:element name="x" type="xs:int"/></xs:sequence></xs:complexType><xs:element name="r"><xs:complexType><xs:complexContent><xs:extension base="B"><xs:sequence><xs:element name="y" type="xs:int"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType></xs:element>"#,
        ),
        &[
            ("<r><x>1</x><y>2</y></r>", true),
            ("<r><x>1</x><x>2</x><y>3</y></r>", true),
            ("<r><x>1</x><x>2</x><x>3</x><y>4</y></r>", false),
            ("<r><y>2</y></r>", false),
        ],
    );
}

#[test]
fn extension_with_a_group_reference_or_all_group_adds_its_particles() {
    assert_instances(
        &schema(&format!(
            r#"{}<xs:group name="G"><xs:sequence><xs:element name="y" type="xs:int"/></xs:sequence></xs:group><xs:element name="r"><xs:complexType><xs:complexContent><xs:extension base="CT"><xs:group ref="G"/></xs:extension></xs:complexContent></xs:complexType></xs:element>"#,
            CT_X_INT
        )),
        &[
            ("<r><x>1</x><y>2</y></r>", true),
            ("<r><x>1</x><y>z</y></r>", false),
            ("<r><x>1</x></r>", false),
        ],
    );
    assert_instances(
        &schema(
            r#"<xs:element name="r"><xs:complexType><xs:complexContent><xs:restriction base="xs:anyType"><xs:all><xs:element name="a" type="xs:int"/><xs:element name="b" type="xs:int"/></xs:all></xs:restriction></xs:complexContent></xs:complexType></xs:element>"#,
        ),
        &[
            ("<r><b>1</b><a>2</a></r>", true),
            ("<r><a>1</a></r>", false),
            ("<r><a>x</a><b>1</b></r>", false),
        ],
    );
}

#[test]
fn a_group_reference_as_content_keeps_its_occurrence() {
    // Extension: the base's x, then group A (a choice) at most once, optional.
    assert_instances(
        &schema(
            r#"<xs:complexType name="B"><xs:sequence><xs:element name="x"/></xs:sequence></xs:complexType><xs:group name="A"><xs:choice><xs:element name="A1"/><xs:element name="A2"/></xs:choice></xs:group><xs:element name="r"><xs:complexType><xs:complexContent><xs:extension base="B"><xs:group ref="A" minOccurs="0"/></xs:extension></xs:complexContent></xs:complexType></xs:element>"#,
        ),
        &[
            ("<r><x/></r>", true),
            ("<r><x/><A2/></r>", true),
            ("<r><x/><A1/><A2/></r>", false),
            ("<r><A1/></r>", false),
        ],
    );
    // Restriction: group G (a choice) one to three times.
    assert_instances(
        &schema(
            r#"<xs:complexType name="A"><xs:choice minOccurs="0" maxOccurs="4"><xs:element name="y1"/><xs:element name="y2"/></xs:choice></xs:complexType><xs:group name="G"><xs:choice><xs:element name="y1"/><xs:element name="y2"/></xs:choice></xs:group><xs:element name="r"><xs:complexType><xs:complexContent><xs:restriction base="A"><xs:group ref="G" maxOccurs="3"/></xs:restriction></xs:complexContent></xs:complexType></xs:element>"#,
        ),
        &[
            ("<r><y1/><y2/></r>", true),
            ("<r><y2/><y2/><y1/></r>", true),
            ("<r/>", false),
            ("<r><y1/><y2/><y1/><y2/></r>", false),
        ],
    );
    // A group reference that is the complexType's own content.
    assert_instances(
        &schema(
            r#"<xs:group name="G"><xs:sequence><xs:element name="a"/></xs:sequence></xs:group><xs:element name="r"><xs:complexType><xs:group ref="G" minOccurs="0" maxOccurs="2"/></xs:complexType></xs:element>"#,
        ),
        &[
            ("<r/>", true),
            ("<r><a/><a/></r>", true),
            ("<r><a/><a/><a/></r>", false),
        ],
    );
}

#[test]
fn restriction_without_particles_is_empty() {
    assert_instances(
        &schema(
            r#"<xs:complexType name="CT"><xs:sequence><xs:element name="x" type="xs:int" minOccurs="0"/></xs:sequence></xs:complexType><xs:complexType name="D"><xs:complexContent><xs:restriction base="CT"/></xs:complexContent></xs:complexType><xs:element name="r" type="D"/>"#,
        ),
        &[
            ("<r/>", true),
            ("<r><x>1</x></r>", false),
            ("<r>text</r>", false),
        ],
    );
    // A restriction of xs:anyType without particles: empty as well.
    assert_instances(
        &schema(
            r#"<xs:element name="r"><xs:complexType><xs:complexContent><xs:restriction base="xs:anyType"><xs:attribute name="a" type="xs:int"/></xs:restriction></xs:complexContent></xs:complexType></xs:element>"#,
        ),
        &[
            (r#"<r a="1"/>"#, true),
            (r#"<r a="x"/>"#, false),
            ("<r><x/></r>", false),
            ("<r>text</r>", false),
        ],
    );
    // Mixed (on complexContent): text, no elements.
    assert_instances(
        &schema(
            r#"<xs:element name="r"><xs:complexType><xs:complexContent mixed="true"><xs:restriction base="xs:anyType"/></xs:complexContent></xs:complexType></xs:element>"#,
        ),
        &[
            ("<r/>", true),
            ("<r>text</r>", true),
            ("<r><x/></r>", false),
        ],
    );
}

/// An element whose content type is empty has no character children, XML
/// white space included (cvc-complex-type.2.1), whether the type declares
/// no particle, only attributes, or restricts `xs:anyType` without
/// particles. A comment or an empty CDATA section is not a character child.
/// Mixed empty content admits text.
#[test]
fn empty_content_admits_no_character_children() {
    let validator = build(&schema(
        r#"<xs:element name="r"><xs:complexType><xs:choice>
  <xs:element name="e"><xs:complexType/></xs:element>
  <xs:element name="ea"><xs:complexType><xs:attribute name="u" type="xs:int"/></xs:complexType></xs:element>
  <xs:element name="er"><xs:complexType><xs:complexContent><xs:restriction base="xs:anyType"/></xs:complexContent></xs:complexType></xs:element>
  <xs:element name="em"><xs:complexType mixed="true"/></xs:element>
</xs:choice></xs:complexType></xs:element>"#,
    ));
    for (xml, name) in [
        ("<r><e> </e></r>", "'e'"),
        ("<r><e>&#32;</e></r>", "'e'"),
        ("<r><e><![CDATA[ ]]></e></r>", "'e'"),
        ("<r><ea u=\"1\">&#10;</ea></r>", "'ea'"),
        ("<r><er> </er></r>", "'er'"),
        ("<r><e><!-- c --> </e></r>", "'e'"),
    ] {
        let errs = errors(&validator, xml);
        assert!(
            errs.iter()
                .any(|e| e.contains(name) && e.contains("empty content type")),
            "instance {}: expected a refusal naming {}, got {:?}",
            xml,
            name,
            errs
        );
    }
    for xml in [
        "<r><e/></r>",
        "<r><e></e></r>",
        "<r><e><!-- c --></e></r>",
        "<r><e><![CDATA[]]></e></r>",
        "<r><ea u=\"1\"/></r>",
        "<r><er></er></r>",
        "<r><em> text </em></r>",
    ] {
        let errs = errors(&validator, xml);
        assert!(errs.is_empty(), "instance {}: got errors {:?}", xml, errs);
    }
}

#[test]
fn extension_of_any_type_without_particles_has_any_content() {
    assert_instances(
        &schema(
            r#"<xs:element name="g" type="xs:int"/><xs:element name="r"><xs:complexType><xs:complexContent><xs:extension base="xs:anyType"/></xs:complexContent></xs:complexType></xs:element>"#,
        ),
        &[
            ("<r/>", true),
            ("<r>text<x><y/></x></r>", true),
            ("<r><g>1</g></r>", true),
            // A child with a global declaration is validated (lax).
            ("<r><g>abc</g></r>", false),
        ],
    );
}

#[test]
fn mixed_base_content_is_kept_by_an_extension_without_particles() {
    assert_instances(
        &schema(
            r#"<xs:complexType name="M" mixed="true"><xs:sequence><xs:element name="x" type="xs:int"/></xs:sequence></xs:complexType><xs:element name="r"><xs:complexType><xs:complexContent><xs:extension base="M"/></xs:complexContent></xs:complexType></xs:element>"#,
        ),
        &[
            ("<r>a<x>1</x>b</r>", true),
            ("<r>a</r>", false),
            ("<r>a<x>z</x></r>", false),
        ],
    );
}

#[test]
fn simple_content_refuses_element_children() {
    let t = r#"<xs:simpleType name="T"><xs:restriction base="xs:token"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>"#;
    // Extension without attributes.
    assert_instances(
        &schema(&format!(
            r#"{}<xs:element name="n"><xs:complexType><xs:simpleContent><xs:extension base="T"/></xs:simpleContent></xs:complexType></xs:element>"#,
            t
        )),
        &[
            ("<n>abc</n>", true),
            ("<n>a<!-- c -->bc</n>", true),
            ("<n>abcd</n>", false),
            ("<n><n/></n>", false),
            ("<n>ab<x/></n>", false),
            ("<n><x>ab</x></n>", false),
        ],
    );
    // Extension with a required attribute.
    assert_instances(
        &schema(&format!(
            r#"{}<xs:element name="k"><xs:complexType><xs:simpleContent><xs:extension base="T"><xs:attribute name="a" type="xs:string" use="required"/></xs:extension></xs:simpleContent></xs:complexType></xs:element>"#,
            t
        )),
        &[
            (r#"<k a="v">abc</k>"#, true),
            (r#"<k a="v">ab<k a="v"/></k>"#, false),
            (r#"<k a="v">ab<x/></k>"#, false),
            ("<k>abc</k>", false),
        ],
    );
    // A simpleContent restriction of a simple-content complex type.
    assert_instances(
        &schema(&format!(
            r#"{}<xs:complexType name="S"><xs:simpleContent><xs:extension base="T"/></xs:simpleContent></xs:complexType><xs:element name="n"><xs:complexType><xs:simpleContent><xs:restriction base="S"><xs:maxLength value="2"/></xs:restriction></xs:simpleContent></xs:complexType></xs:element>"#,
            t
        )),
        &[
            ("<n>ab</n>", true),
            ("<n>abc</n>", false),
            ("<n>a<x/></n>", false),
        ],
    );
}

#[test]
fn complex_content_extension_without_particles_of_simple_content_keeps_it() {
    // complexContent extension adding only an attribute to a type with
    // simple content: the content type stays the base's simple content.
    assert_instances(
        &schema(
            r#"<xs:complexType name="S"><xs:simpleContent><xs:extension base="xs:int"/></xs:simpleContent></xs:complexType><xs:complexType name="D"><xs:complexContent><xs:extension base="S"><xs:attribute name="a" type="xs:string"/></xs:extension></xs:complexContent></xs:complexType><xs:element name="e" type="D"/><xs:element name="f" type="D" fixed="7"/>"#,
        ),
        &[
            (r#"<e a="v">12</e>"#, true),
            ("<e>abc</e>", false),
            ("<e>1<x/></e>", false),
            ("<f>7</f>", true),
            ("<f>07</f>", true),
            ("<f>8</f>", false),
        ],
    );
}

#[test]
fn simple_content_base_with_element_content_is_refused() {
    // src-ct.2: the base of a simpleContent extension must have simple
    // content; the element cannot be valid.
    let validator = build(&schema(&format!(
        r#"{}<xs:element name="n"><xs:complexType><xs:simpleContent><xs:extension base="CT"/></xs:simpleContent></xs:complexType></xs:element>"#,
        CT_X_INT
    )));
    let errs = errors(&validator, "<n>abc</n>");
    assert!(
        errs.iter()
            .any(|e| e.contains("does not have simple content")),
        "{:?}",
        errs
    );
}

/// A chain of `n` named `complexContent` extensions of `T0`, each adding a
/// choice of one element. `T0` has a required attribute and an attribute
/// wildcard, which every extension inherits.
fn extension_chain(n: usize) -> String {
    let mut body = String::from(
        r#"<xs:complexType name="T0"><xs:sequence><xs:element name="x" type="xs:int"/></xs:sequence><xs:attribute name="a" type="xs:int" use="required"/><xs:anyAttribute namespace="urn:w" processContents="skip"/></xs:complexType>"#,
    );
    for i in 1..=n {
        body.push_str(&format!(
            r#"<xs:complexType name="T{}"><xs:complexContent><xs:extension base="T{}"><xs:choice><xs:element name="e{}" type="xs:int"/></xs:choice></xs:extension></xs:complexContent></xs:complexType>"#,
            i,
            i - 1,
            i
        ));
    }
    body
}

fn chain_instance(attrs: &str, n: usize, last: &str) -> String {
    let mut xml = format!(r#"<r xmlns:w="urn:w" {}><x>1</x>"#, attrs);
    for i in 1..n {
        xml.push_str(&format!("<e{}>1</e{}>", i, i));
    }
    xml.push_str(last);
    xml.push_str("</r>");
    xml
}

#[test]
fn derivation_chain_at_the_limit_builds_and_validates() {
    // 256 derivation steps from T256 to T0: the most the build accepts. The
    // content, the inherited attribute use and the inherited wildcard are
    // all checked through the whole chain, and `xsi:type` substitutes T256
    // for its root T0 across all 256 steps.
    let n = 256;
    let last = format!("<e{}>1</e{}>", n, n);
    let xsd = schema(&format!(
        r#"{}<xs:element name="r" type="T{}"/><xs:element name="s" type="T0"/>"#,
        extension_chain(n),
        n
    ));
    let validator = build(&xsd);
    let as_s = |xml: String| {
        xml.replacen(
            "<r ",
            r#"<s xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:type="T256" "#,
            1,
        )
        .replace("</r>", "</s>")
    };
    for (xml, valid) in [
        (as_s(chain_instance(r#"a="1""#, n, &last)), true),
        (as_s(chain_instance(r#"a="1""#, n, "")), false),
        (chain_instance(r#"a="1" w:q="1""#, n, &last), true),
        (chain_instance(r#"a="1""#, n, ""), false),
        (
            chain_instance(r#"a="1""#, n, &format!("<e{}>x</e{}>", n, n)),
            false,
        ),
        (chain_instance(r#"w:q="1""#, n, &last), false),
        (chain_instance(r#"a="1" b="1""#, n, &last), false),
    ] {
        assert_eq!(
            errors(&validator, &xml).is_empty(),
            valid,
            "instance with attributes {:?}",
            &xml[..40]
        );
    }
}

#[test]
fn derivation_chain_longer_than_the_limit_is_refused_at_build() {
    let refused = |xsd: String| {
        let doc = parse(&xsd).expect("schema parses");
        match XsdValidator::from_schema(&doc) {
            Ok(_) => panic!("a derivation chain of 257 steps must be refused"),
            Err(err) => err.to_string(),
        }
    };
    // A named type 257 steps from T0.
    let err = refused(schema(&format!(
        r#"{}<xs:element name="r" type="T257"/>"#,
        extension_chain(257)
    )));
    assert!(err.contains("'T257'"), "{}", err);
    assert!(err.contains("longer than 256 steps"), "{}", err);
    // An anonymous type one step below T256 is refused too, whether or not
    // anything uses it.
    let err = refused(schema(&format!(
        r#"{}<xs:element name="r"><xs:complexType><xs:complexContent><xs:extension base="T256"><xs:attribute name="z"/></xs:extension></xs:complexContent></xs:complexType></xs:element>"#,
        extension_chain(256)
    )));
    assert!(err.contains("'(anonymous)'"), "{}", err);
    assert!(err.contains("longer than 256 steps"), "{}", err);
}

/// A chain of 256 extensions, each adding an optional choice, of a type
/// whose sequence holds a child of the most derived type: every level of
/// nesting in the instance is an element of a 256-step type. Validating 32
/// levels fits in a 1 MiB stack, because the content model does not nest
/// deeper with the derivation.
#[test]
fn deep_extension_chain_validates_nested_elements_on_a_small_stack() {
    let steps = 256;
    let nesting = 32;
    let mut body = format!(
        r#"<xs:complexType name="T0"><xs:sequence><xs:element name="c" type="T{steps}" minOccurs="0"/></xs:sequence></xs:complexType>"#
    );
    for k in 1..=steps {
        body.push_str(&format!(
            r#"<xs:complexType name="T{k}"><xs:complexContent><xs:extension base="T{}"><xs:choice minOccurs="0"><xs:element name="e{k}" type="xs:string"/></xs:choice></xs:extension></xs:complexContent></xs:complexType>"#,
            k - 1
        ));
    }
    body.push_str(&format!(r#"<xs:element name="r" type="T{steps}"/>"#));
    let xsd = schema(&body);
    let nested = |innermost: &str| {
        format!(
            "<r>{}{}{}</r>",
            "<c>".repeat(nesting),
            innermost,
            "</c>".repeat(nesting)
        )
    };
    let (valid, invalid) = (nested("<e7>x</e7>"), nested("<e7>x</e7><e3>x</e3>"));
    let verdicts = std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(move || {
            let validator = build(&xsd);
            (errors(&validator, &valid), errors(&validator, &invalid))
        })
        .expect("spawn")
        .join()
        .expect("validation completes on a 1 MiB stack");
    assert!(verdicts.0.is_empty(), "{:?}", verdicts.0);
    assert!(!verdicts.1.is_empty(), "e3 after e7 is out of order");
}
