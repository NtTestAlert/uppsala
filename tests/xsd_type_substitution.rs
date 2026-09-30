//! Regression tests for `xsi:type` in an instance (XSD 1.0 Part 1 §3.3.4,
//! cvc-elt.4).
//!
//! The type an `xsi:type` names must be the declared type or validly derived
//! from it (cvc-elt.4.3). An anonymous declared type has no name, so no
//! `xsi:type` names it, and a named type can only derive from a named base:
//! every `xsi:type` on an element of anonymous type is invalid. A built-in
//! type derives only from built-in types. The QName is resolved with the
//! instance's namespace bindings to one expanded name (cvc-elt.4.1). The
//! declaration's fixed value and identity constraints apply whatever the
//! `xsi:type` (cvc-elt.5, cvc-elt.6).
//!
//! Also the other XSI attributes and the attributes of an element of simple
//! type: an attribute is an XSI attribute by its namespace URI, only four
//! XSI names are exempt from attribute assessment, `xsi:nil` needs a
//! nillable declaration (cvc-elt.3), and an element of simple type carries
//! no other attribute (cvc-type.3.1.1).

mod common;
use common::parse;

use std::fs;
use std::path::PathBuf;

use uppsala::XsdValidator;

const XS: &str = r#"xmlns:xs="http://www.w3.org/2001/XMLSchema""#;
const XSI: &str = r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#;

fn schema_ns(body: &str) -> String {
    format!(
        r#"<xs:schema {} xmlns:m="urn:m" targetNamespace="urn:m" elementFormDefault="qualified">{}</xs:schema>"#,
        XS, body
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
fn assert_verdicts(validator: &XsdValidator, cases: &[(String, bool)]) {
    for (xml, valid) in cases {
        let errs = errors(validator, xml);
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

const ANONYMOUS: &str = r#"
<xs:complexType name="TAddr"><xs:sequence>
  <xs:element name="country" type="xs:string"/><xs:element name="line" type="xs:string"/>
</xs:sequence></xs:complexType>
<xs:simpleType name="TText"><xs:restriction base="xs:string"><xs:minLength value="1"/></xs:restriction></xs:simpleType>
<xs:element name="document"><xs:complexType><xs:sequence>
  <xs:element name="header"><xs:complexType><xs:sequence>
    <xs:element name="variant"><xs:simpleType><xs:restriction base="xs:byte"><xs:enumeration value="3"/></xs:restriction></xs:simpleType></xs:element>
    <xs:element name="created"><xs:simpleType><xs:restriction base="xs:dateTime"/></xs:simpleType></xs:element>
  </xs:sequence></xs:complexType></xs:element>
  <xs:element name="addr" type="m:TAddr"/>
</xs:sequence></xs:complexType></xs:element>"#;

fn document(attrs: &str, header: &str, variant: &str, created: &str) -> String {
    format!(
        r#"<m:document xmlns:m="urn:m" {XSI}{attrs}>{header}<m:addr><m:country>PL</m:country><m:line>x</m:line></m:addr></m:document>"#,
        header = if header.is_empty() {
            format!("<m:header>{variant}{created}</m:header>")
        } else {
            header.to_string()
        }
    )
}

#[test]
fn xsi_type_on_an_element_of_anonymous_type_is_refused() {
    let validator = build(&schema_ns(ANONYMOUS));
    let variant = "<m:variant>3</m:variant>";
    let created = "<m:created>2026-09-29T10:00:00Z</m:created>";
    let address = "<m:country>PL</m:country><m:line>x</m:line>";
    assert_verdicts(
        &validator,
        &[
            (document("", "", variant, created), true),
            // The whole document replaced by the content of a named type.
            (
                format!(
                    r#"<m:document xmlns:m="urn:m" {XSI} xsi:type="m:TAddr">{address}</m:document>"#
                ),
                false,
            ),
            // A nested element of anonymous complex type.
            (
                document(
                    "",
                    &format!(r#"<m:header xsi:type="m:TAddr">{address}</m:header>"#),
                    "",
                    "",
                ),
                false,
            ),
            // Anonymous simple types, with a built-in and a named `xsi:type`.
            (
                document(
                    "",
                    "",
                    variant,
                    r#"<m:created xsi:type="xs:string" xmlns:xs="http://www.w3.org/2001/XMLSchema">nonsense</m:created>"#,
                ),
                false,
            ),
            (
                document(
                    "",
                    "",
                    r#"<m:variant xsi:type="m:TText">9</m:variant>"#,
                    created,
                ),
                false,
            ),
            // Refused even when the value is valid for both types.
            (
                document(
                    "",
                    "",
                    variant,
                    r#"<m:created xsi:type="xs:dateTime" xmlns:xs="http://www.w3.org/2001/XMLSchema">2026-09-29T10:00:00Z</m:created>"#,
                ),
                false,
            ),
            // Control: a named declared type admits itself.
            (
                format!(
                    r#"<m:document xmlns:m="urn:m" {XSI}><m:header>{variant}{created}</m:header><m:addr xsi:type="m:TAddr">{address}</m:addr></m:document>"#
                ),
                true,
            ),
        ],
    );
}

#[test]
fn builtin_xsi_type_must_derive_from_the_declared_type() {
    let validator = build(&schema_ns(
        r#"<xs:simpleType name="S"><xs:restriction base="xs:string"/></xs:simpleType>
<xs:element name="r"><xs:complexType><xs:choice>
  <xs:element name="d" type="xs:decimal"/>
  <xs:element name="s" type="xs:string"/>
  <xs:element name="n" type="m:S"/>
  <xs:element name="a"/>
  <xs:element name="b" type="xs:decimal" block="restriction"/>
  <xs:element name="t" type="xs:anySimpleType"/>
</xs:choice></xs:complexType></xs:element>"#,
    ));
    let r = |child: &str| {
        format!(
            r#"<m:r xmlns:m="urn:m" xmlns:xs="http://www.w3.org/2001/XMLSchema" {XSI}>{child}</m:r>"#
        )
    };
    assert_verdicts(
        &validator,
        &[
            (r(r#"<m:d xsi:type="xs:integer">5</m:d>"#), true),
            (r(r#"<m:d xsi:type="xs:unsignedByte">5</m:d>"#), true),
            (r(r#"<m:d xsi:type="xs:integer">5.5</m:d>"#), false),
            (r(r#"<m:d xsi:type="xs:string">5</m:d>"#), false),
            (r(r#"<m:d xsi:type="xs:double">5</m:d>"#), false),
            (r(r#"<m:s xsi:type="xs:token">a</m:s>"#), true),
            (r(r#"<m:s xsi:type="xs:NCName">a</m:s>"#), true),
            (r(r#"<m:s xsi:type="xs:anySimpleType">a</m:s>"#), false),
            // A built-in type is never derived from a schema type.
            (r(r#"<m:n xsi:type="xs:string">a</m:n>"#), false),
            (r(r#"<m:a xsi:type="xs:int">5</m:a>"#), true),
            (r(r#"<m:a xsi:type="xs:int">x</m:a>"#), false),
            // block="restriction" on the declaration.
            (r(r#"<m:b xsi:type="xs:integer">5</m:b>"#), false),
            (r(r#"<m:b xsi:type="xs:decimal">5</m:b>"#), true),
            (r(r#"<m:t xsi:type="xs:NMTOKENS">a b</m:t>"#), true),
        ],
    );
}

/// Two namespaces each define a type named `TAddress`: the `xsi:type` names
/// one of them by its expanded name, never the other.
#[test]
fn xsi_type_is_resolved_by_expanded_name() {
    let dir = mkdir_unique("xsi-type-expanded-name");
    let main = format!(
        r#"<xs:schema {XS} xmlns:m="urn:m" xmlns:o="urn:o" targetNamespace="urn:m" elementFormDefault="qualified">
  <xs:import namespace="urn:o" schemaLocation="o.xsd"/>
  <xs:complexType name="TAddress"><xs:sequence><xs:element name="a" type="xs:int"/></xs:sequence></xs:complexType>
  <xs:complexType name="TSub"><xs:complexContent><xs:extension base="m:TAddress"><xs:sequence>
    <xs:element name="b" type="xs:int"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType>
  <xs:element name="e" type="m:TAddress"/>
</xs:schema>"#
    );
    let other = format!(
        r#"<xs:schema {XS} xmlns:o="urn:o" targetNamespace="urn:o" elementFormDefault="qualified">
  <xs:complexType name="TAddress"><xs:sequence><xs:element name="z" type="xs:string"/></xs:sequence></xs:complexType>
</xs:schema>"#
    );
    let main_path = dir.join("main.xsd");
    fs::write(&main_path, &main).unwrap();
    fs::write(dir.join("o.xsd"), &other).unwrap();
    let schema_doc = parse(&main).expect("parse schema");
    let built = XsdValidator::from_schema_with_base_path(&schema_doc, Some(&main_path));
    fs::remove_dir_all(&dir).ok();
    let validator = built.expect("build validator");
    let e = |attrs: &str, content: &str| {
        format!(r#"<m:e xmlns:m="urn:m" xmlns:o="urn:o" {XSI} {attrs}>{content}</m:e>"#)
    };
    assert_verdicts(
        &validator,
        &[
            (e(r#"xsi:type="m:TAddress""#, "<m:a>1</m:a>"), true),
            (e(r#"xsi:type="o:TAddress""#, "<o:z>1</o:z>"), false),
            (e(r#"xsi:type="o:TAddress""#, "<m:a>1</m:a>"), false),
            (e(r#"xsi:type="m:TSub""#, "<m:a>1</m:a><m:b>2</m:b>"), true),
        ],
    );
}

/// A prefix without a binding, or a namespace without the type, names no
/// type; there is no fallback to a no-namespace type of the same local name.
#[test]
fn xsi_type_that_does_not_resolve_is_refused() {
    let validator = build(&format!(
        r#"<xs:schema {XS}>
  <xs:complexType name="T"><xs:sequence><xs:element name="a" type="xs:int"/></xs:sequence></xs:complexType>
  <xs:element name="e" type="T"/>
</xs:schema>"#
    ));
    let e = |attrs: &str| format!(r#"<e {XSI} {attrs}><a>1</a></e>"#);
    assert_verdicts(
        &validator,
        &[
            (e(r#"xsi:type="T""#), true),
            (e(r#"xmlns:p="urn:other" xsi:type="p:T""#), false),
            (e(r#"xsi:type="q:T""#), false),
        ],
    );
}

#[test]
fn fixed_value_and_identity_constraints_apply_with_xsi_type() {
    let validator = build(&schema_ns(
        r#"<xs:simpleType name="Code"><xs:restriction base="xs:string"><xs:length value="2"/></xs:restriction></xs:simpleType>
<xs:complexType name="R"><xs:sequence><xs:element name="k" type="xs:string" maxOccurs="unbounded"/></xs:sequence></xs:complexType>
<xs:element name="root"><xs:complexType><xs:choice>
  <xs:element name="c" type="m:Code" fixed="PL"/>
  <xs:element name="f" type="xs:decimal" fixed="1"/>
  <xs:element name="list" type="m:R">
    <xs:unique name="u"><xs:selector xpath="m:k"/><xs:field xpath="."/></xs:unique>
  </xs:element>
</xs:choice></xs:complexType></xs:element>"#,
    ));
    let root = |child: &str| {
        format!(
            r#"<m:root xmlns:m="urn:m" xmlns:xs="http://www.w3.org/2001/XMLSchema" {XSI}>{child}</m:root>"#
        )
    };
    assert_verdicts(
        &validator,
        &[
            (root(r#"<m:c xsi:type="m:Code">PL</m:c>"#), true),
            (root(r#"<m:c xsi:type="m:Code">DE</m:c>"#), false),
            // An empty element takes the fixed value.
            (root(r#"<m:c xsi:type="m:Code"/>"#), true),
            (root(r#"<m:f xsi:type="xs:integer">01</m:f>"#), true),
            (root(r#"<m:f xsi:type="xs:integer">2</m:f>"#), false),
            (
                root(r#"<m:list xsi:type="m:R"><m:k>a</m:k><m:k>b</m:k></m:list>"#),
                true,
            ),
            (
                root(r#"<m:list xsi:type="m:R"><m:k>a</m:k><m:k>a</m:k></m:list>"#),
                false,
            ),
            (root(r#"<m:list><m:k>a</m:k><m:k>a</m:k></m:list>"#), false),
        ],
    );
}

/// Assert that each instance is refused with an error naming `name`.
fn assert_refused_naming(validator: &XsdValidator, cases: &[(String, &str)]) {
    for (xml, name) in cases {
        let errs = errors(validator, xml);
        assert!(
            errs.iter().any(|e| e.contains(name)),
            "instance {}: expected an error naming {:?}, got {:?}",
            xml,
            name,
            errs
        );
    }
}

const XSI_TYPES: &str = r###"
<xs:complexType name="T"><xs:sequence><xs:element name="a" type="xs:int"/></xs:sequence></xs:complexType>
<xs:complexType name="B"><xs:sequence><xs:element name="a" type="xs:int"/></xs:sequence>
  <xs:anyAttribute namespace="##other" processContents="skip"/></xs:complexType>
<xs:complexType name="L"><xs:sequence><xs:element name="a" type="xs:int"/></xs:sequence>
  <xs:anyAttribute namespace="##local" processContents="skip"/></xs:complexType>
<xs:complexType name="D"><xs:complexContent><xs:extension base="m:B"><xs:sequence>
  <xs:element name="b" type="xs:int"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType>
<xs:element name="r"><xs:complexType><xs:choice>
  <xs:element name="c" type="m:T"/>
  <xs:element name="w" type="m:B"/>
  <xs:element name="l" type="m:L"/>
  <xs:element name="z" type="m:T" nillable="true"/>
  <xs:element name="s" type="xs:string"/>
</xs:choice></xs:complexType></xs:element>"###;

fn xsi_root(child: &str) -> String {
    format!(r#"<m:r xmlns:m="urn:m" xmlns:xs="http://www.w3.org/2001/XMLSchema">{child}</m:r>"#)
}

/// A declaration that is not nillable admits no `xsi:nil` at all, whatever
/// its value (cvc-elt.3.1); on a nillable one the value must be a boolean.
#[test]
fn xsi_nil_is_refused_on_an_element_that_is_not_nillable() {
    let validator = build(&schema_ns(XSI_TYPES));
    let not_nillable = |name: &str| format!("'{}', which is not nillable", name);
    let c = not_nillable("c");
    let s = not_nillable("s");
    assert_refused_naming(
        &validator,
        &[
            (
                xsi_root(&format!(r#"<m:c {XSI} xsi:nil="false"><m:a>1</m:a></m:c>"#)),
                &c,
            ),
            (xsi_root(&format!(r#"<m:c {XSI} xsi:nil="true"/>"#)), &c),
            (xsi_root(&format!(r#"<m:s {XSI} xsi:nil="0">a</m:s>"#)), &s),
            (
                xsi_root(&format!(r#"<m:z {XSI} xsi:nil="maybe"/>"#)),
                "'maybe' is not a valid boolean",
            ),
            (
                xsi_root(&format!(r#"<m:z {XSI} xsi:nil="maybe"><m:a>1</m:a></m:z>"#)),
                "'maybe' is not a valid boolean",
            ),
        ],
    );
    assert_verdicts(
        &validator,
        &[
            (xsi_root(&format!(r#"<m:z {XSI} xsi:nil="true"/>"#)), true),
            (xsi_root(&format!(r#"<m:z {XSI} xsi:nil=" 1 "/>"#)), true),
            (
                xsi_root(&format!(r#"<m:z {XSI} xsi:nil="false"><m:a>1</m:a></m:z>"#)),
                true,
            ),
            (xsi_root(&format!(r#"<m:z {XSI} xsi:nil="false"/>"#)), false),
            (
                xsi_root(&format!(r#"<m:z {XSI} xsi:nil="true"><m:a>1</m:a></m:z>"#)),
                false,
            ),
            (xsi_root(r#"<m:c><m:a>1</m:a></m:c>"#), true),
        ],
    );
}

/// Under `xsi:type`, the element's value is compared with the fixed value in
/// the value space of the actual type (cvc-elt.5.2.2.2.2): as an `xs:token`,
/// ` 1 ` is `1`.
#[test]
fn fixed_value_is_compared_in_the_xsi_type_value_space() {
    let validator = build(&schema_ns(
        r#"<xs:element name="r"><xs:complexType><xs:choice>
  <xs:element name="s" type="xs:string" fixed="1"/>
</xs:choice></xs:complexType></xs:element>"#,
    ));
    let r = |child: &str| {
        format!(
            r#"<m:r xmlns:m="urn:m" xmlns:xs="http://www.w3.org/2001/XMLSchema" {XSI}>{child}</m:r>"#
        )
    };
    assert_verdicts(
        &validator,
        &[
            (r(r#"<m:s>1</m:s>"#), true),
            (r(r#"<m:s> 1 </m:s>"#), false),
            (r(r#"<m:s xsi:type="xs:token"> 1 </m:s>"#), true),
            (r(r#"<m:s xsi:type="xs:token">2</m:s>"#), false),
            (r(r#"<m:s xsi:type="xs:token"/>"#), true),
        ],
    );
}

/// The fixed value is the declaration's value constraint, a value of the
/// declared type (§3.3.1); only the element's value is read in the actual
/// type (cvc-elt.5.2.2.2.2). Under `xsi:type="xs:token"`, the text `1` is
/// the token `1`, which is not the `xs:string` fixed value ` 1 `. A decimal
/// fixed value `1.0` is the integer `1`. The literal of an
/// `xs:anySimpleType` declaration has no determinate value (XSD 1.1 Part 2
/// §3.2.1), and `xsi:type` does not choose one, so the element's normalized
/// value must be that literal character for character: `1.0` and `true`
/// are not the literal `1`, and the token `1` is not ` 1 `. The fixed value of an `xs:anyType` declaration, which
/// includes one without a type, is an `xs:string` (§3.3.2), so only a
/// string-derived actual type can match it.
#[test]
fn fixed_value_is_a_value_of_the_declared_type() {
    let validator = build(&schema_ns(
        r#"<xs:element name="r"><xs:complexType><xs:choice>
  <xs:element name="s" type="xs:string" fixed=" 1 "/>
  <xs:element name="h" type="xs:string" fixed="a  b"/>
  <xs:element name="g" type="xs:decimal" fixed="1.0"/>
  <xs:element name="f" type="xs:anySimpleType" fixed="1"/>
  <xs:element name="f2" type="xs:anySimpleType" fixed=" 1 "/>
  <xs:element name="ft"><xs:complexType><xs:attribute name="t" type="xs:anySimpleType" fixed="1"/></xs:complexType></xs:element>
  <xs:element name="y" fixed=" 1 "/>
  <xs:element name="y2" type="xs:anyType" fixed="1"/>
</xs:choice></xs:complexType></xs:element>"#,
    ));
    let r = |child: &str| {
        format!(
            r#"<m:r xmlns:m="urn:m" xmlns:xs="http://www.w3.org/2001/XMLSchema" {XSI}>{child}</m:r>"#
        )
    };
    assert_verdicts(
        &validator,
        &[
            (r(r#"<m:s> 1 </m:s>"#), true),
            (r(r#"<m:s>1</m:s>"#), false),
            (r(r#"<m:s xsi:type="xs:token">1</m:s>"#), false),
            (r(r#"<m:s xsi:type="xs:token"> 1 </m:s>"#), false),
            (r(r#"<m:s xsi:type="xs:normalizedString"> 1 </m:s>"#), true),
            (r(r#"<m:h xsi:type="xs:token">a b</m:h>"#), false),
            (r(r#"<m:h xsi:type="xs:token">a  b</m:h>"#), false),
            (r(r#"<m:h xsi:type="xs:normalizedString">a  b</m:h>"#), true),
            (r(r#"<m:g>1</m:g>"#), true),
            (r(r#"<m:g xsi:type="xs:integer">1</m:g>"#), true),
            (r(r#"<m:g xsi:type="xs:integer">2</m:g>"#), false),
            (r(r#"<m:f xsi:type="xs:decimal">1.0</m:f>"#), false),
            (r(r#"<m:f xsi:type="xs:decimal">1</m:f>"#), true),
            (r(r#"<m:f xsi:type="xs:decimal">2</m:f>"#), false),
            (r(r#"<m:f xsi:type="xs:boolean">true</m:f>"#), false),
            (r(r#"<m:f xsi:type="xs:boolean">1</m:f>"#), true),
            (r(r#"<m:f xsi:type="xs:string">1</m:f>"#), true),
            (r(r#"<m:f>1.0</m:f>"#), false),
            (r(r#"<m:f> 1</m:f>"#), false),
            (r(r#"<m:f>1</m:f>"#), true),
            (r(r#"<m:f2 xsi:type="xs:token">1</m:f2>"#), false),
            (r(r#"<m:f2 xsi:type="xs:string"> 1 </m:f2>"#), true),
            (r(r#"<m:ft t="1"/>"#), true),
            (r(r#"<m:ft t="1.0"/>"#), false),
            (r(r#"<m:y> 1 </m:y>"#), true),
            (r(r#"<m:y>1</m:y>"#), false),
            (r(r#"<m:y xsi:type="xs:token">1</m:y>"#), false),
            (r(r#"<m:y xsi:type="xs:string"> 1 </m:y>"#), true),
            (r(r#"<m:y2 xsi:type="xs:boolean">true</m:y2>"#), false),
            (r(r#"<m:y2 xsi:type="xs:decimal">1.0</m:y2>"#), false),
            (r(r#"<m:y2 xsi:type="xs:string">1</m:y2>"#), true),
        ],
    );
}

/// A nilled element has no character children (cvc-elt.3.2.1): XML white
/// space is a character child too, whatever the element's type.
#[test]
fn nilled_element_refuses_white_space_content() {
    let validator = build(&schema_ns(
        r#"<xs:complexType name="T"><xs:sequence><xs:element name="a" type="xs:int"/></xs:sequence></xs:complexType>
<xs:element name="r"><xs:complexType><xs:choice>
  <xs:element name="v" type="xs:int" nillable="true"/>
  <xs:element name="z" type="m:T" nillable="true"/>
</xs:choice></xs:complexType></xs:element>"#,
    ));
    let nilled = |name: &str, content: &str| {
        xsi_root(&format!(
            r#"<m:{name} {XSI} xsi:nil="true">{content}</m:{name}>"#
        ))
    };
    assert_refused_naming(
        &validator,
        &[
            (nilled("v", " "), "'v' has xsi:nil='true'"),
            (nilled("v", "&#9;&#10;"), "'v' has xsi:nil='true'"),
            (nilled("v", "<![CDATA[ ]]>"), "'v' has xsi:nil='true'"),
            (nilled("z", "\n  "), "'z' has xsi:nil='true'"),
        ],
    );
    assert_verdicts(
        &validator,
        &[
            (xsi_root(&format!(r#"<m:v {XSI} xsi:nil="true"/>"#)), true),
            (nilled("v", ""), true),
            (nilled("v", "<!-- a comment -->"), true),
            (nilled("z", ""), true),
            (
                xsi_root(&format!(r#"<m:v {XSI} xsi:nil="false"> 1 </m:v>"#)),
                true,
            ),
        ],
    );
}
