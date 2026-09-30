//! Unit tests for the XSD validator.

use crate::parse;
use crate::xsd::types::XsdValidator;

#[test]
fn test_validate_string_element() {
    let schema_xml = r#"
    <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
        <xs:element name="root" type="xs:string"/>
    </xs:schema>
    "#;
    let doc_xml = "<root>hello</root>";

    let schema = parse(schema_xml).unwrap();
    let doc = parse(doc_xml).unwrap();
    let validator = XsdValidator::from_schema(&schema).unwrap();
    let errors = validator.validate(&doc);
    assert!(errors.is_empty(), "Errors: {:?}", errors);
}

#[test]
fn test_validate_integer_valid() {
    let schema_xml = r#"
    <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
        <xs:element name="count" type="xs:integer"/>
    </xs:schema>
    "#;
    let doc_xml = "<count>42</count>";

    let schema = parse(schema_xml).unwrap();
    let doc = parse(doc_xml).unwrap();
    let validator = XsdValidator::from_schema(&schema).unwrap();
    let errors = validator.validate(&doc);
    assert!(errors.is_empty(), "Errors: {:?}", errors);
}

#[test]
fn test_validate_integer_invalid() {
    let schema_xml = r#"
    <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
        <xs:element name="count" type="xs:integer"/>
    </xs:schema>
    "#;
    let doc_xml = "<count>not-a-number</count>";

    let schema = parse(schema_xml).unwrap();
    let doc = parse(doc_xml).unwrap();
    let validator = XsdValidator::from_schema(&schema).unwrap();
    let errors = validator.validate(&doc);
    assert!(!errors.is_empty());
}

#[test]
fn test_validate_boolean() {
    let schema_xml = r#"
    <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
        <xs:element name="flag" type="xs:boolean"/>
    </xs:schema>
    "#;

    let schema = parse(schema_xml).unwrap();

    for val in &["true", "false", "1", "0"] {
        let input = format!("<flag>{}</flag>", val);
        let doc = parse(&input).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        assert!(validator.validate(&doc).is_empty(), "Failed for {}", val);
    }

    let doc = parse("<flag>yes</flag>").unwrap();
    let validator = XsdValidator::from_schema(&schema).unwrap();
    assert!(!validator.validate(&doc).is_empty());
}

#[test]
fn test_validate_complex_type_sequence() {
    let schema_xml = r#"
    <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
        <xs:element name="person">
            <xs:complexType>
                <xs:sequence>
                    <xs:element name="name" type="xs:string"/>
                    <xs:element name="age" type="xs:integer"/>
                </xs:sequence>
            </xs:complexType>
        </xs:element>
    </xs:schema>
    "#;

    let doc_xml = "<person><name>Alice</name><age>30</age></person>";
    let schema = parse(schema_xml).unwrap();
    let doc = parse(doc_xml).unwrap();
    let validator = XsdValidator::from_schema(&schema).unwrap();
    let errors = validator.validate(&doc);
    assert!(errors.is_empty(), "Errors: {:?}", errors);
}

#[test]
fn test_validate_required_attribute() {
    let schema_xml = r#"
    <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
        <xs:element name="item">
            <xs:complexType>
                <xs:sequence/>
                <xs:attribute name="id" type="xs:string" use="required"/>
            </xs:complexType>
        </xs:element>
    </xs:schema>
    "#;

    let schema = parse(schema_xml).unwrap();

    // Missing required attribute
    let doc = parse("<item/>").unwrap();
    let validator = XsdValidator::from_schema(&schema).unwrap();
    let errors = validator.validate(&doc);
    assert!(!errors.is_empty());

    // With required attribute
    let doc = parse(r#"<item id="123"/>"#).unwrap();
    let errors = validator.validate(&doc);
    assert!(errors.is_empty(), "Errors: {:?}", errors);
}

#[test]
fn test_validate_min_max_inclusive() {
    let schema_xml = r#"
    <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
        <xs:element name="score">
            <xs:simpleType>
                <xs:restriction base="xs:integer">
                    <xs:minInclusive value="0"/>
                    <xs:maxInclusive value="100"/>
                </xs:restriction>
            </xs:simpleType>
        </xs:element>
    </xs:schema>
    "#;

    let schema = parse(schema_xml).unwrap();
    let validator = XsdValidator::from_schema(&schema).unwrap();

    let doc = parse("<score>50</score>").unwrap();
    assert!(validator.validate(&doc).is_empty());

    let doc = parse("<score>150</score>").unwrap();
    assert!(!validator.validate(&doc).is_empty());

    let doc = parse("<score>-1</score>").unwrap();
    assert!(!validator.validate(&doc).is_empty());
}

#[test]
fn test_validate_enumeration() {
    let schema_xml = r#"
    <xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
        <xs:element name="color">
            <xs:simpleType>
                <xs:restriction base="xs:string">
                    <xs:enumeration value="red"/>
                    <xs:enumeration value="green"/>
                    <xs:enumeration value="blue"/>
                </xs:restriction>
            </xs:simpleType>
        </xs:element>
    </xs:schema>
    "#;

    let schema = parse(schema_xml).unwrap();
    let validator = XsdValidator::from_schema(&schema).unwrap();

    let doc = parse("<color>red</color>").unwrap();
    assert!(validator.validate(&doc).is_empty());

    let doc = parse("<color>yellow</color>").unwrap();
    assert!(!validator.validate(&doc).is_empty());
}

// ─── Complexity, by operation count ───────────────────────────────────────
//
// These count operations instead of measuring time, so a loaded machine
// cannot fail them; the counters exist only in test builds.

use super::validation::test_counters::{
    take, CONTENT_PARTICLE_COPIES, UNION_LITERAL_READS, UNION_MEMBER_SELECTIONS,
};

/// A restriction of a union reads its enumeration literals once per type,
/// not once per value. A literal with a prefix is the exception: a `QName`
/// member reads it in the scope of each value.
#[test]
fn union_enumeration_literals_are_read_once_per_type() {
    let literals: String = (0..10)
        .map(|i| format!(r#"<xs:enumeration value="v{}"/>"#, i))
        .collect();
    let schema_xml = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
             <xs:simpleType name="U"><xs:union memberTypes="xs:int xs:QName"/></xs:simpleType>
             <xs:simpleType name="R"><xs:restriction base="U">{}<xs:enumeration value="p:x"/></xs:restriction></xs:simpleType>
             <xs:element name="r"><xs:complexType><xs:sequence>
               <xs:element name="e" type="R" maxOccurs="unbounded"/>
             </xs:sequence></xs:complexType></xs:element>
           </xs:schema>"#,
        literals
    );
    let schema = parse(&schema_xml).unwrap();
    let validator = XsdValidator::from_schema(&schema).unwrap();

    let values = 1000;
    let body: String = (0..values).map(|_| "<e>v9</e>").collect();
    let xml = format!(r#"<r xmlns:p="urn:p">{}</r>"#, body);
    let doc = parse(&xml).unwrap();
    take(&UNION_LITERAL_READS);
    assert!(validator.validate(&doc).is_empty());
    // `v9` matches the tenth literal, so `p:x` is never reached.
    assert_eq!(take(&UNION_LITERAL_READS), 10);

    // A value that matches none reaches `p:x` each time: the ten unprefixed
    // literals are still read once, `p:x` once per value.
    let body: String = (0..values).map(|_| "<e>7</e>").collect();
    let xml = format!(r#"<r xmlns:p="urn:p">{}</r>"#, body);
    let doc = parse(&xml).unwrap();
    assert_eq!(validator.validate(&doc).len(), values);
    assert_eq!(take(&UNION_LITERAL_READS), values);
}

/// Unions that share member unions form a DAG: `U{i} = union(U{i+1} W{i+1})`
/// with `W{i}` a restriction of `U{i}`. Each union selects a member for a
/// value once, so the work is linear in the depth, not 2^depth.
#[test]
fn union_sharing_member_unions_selects_each_member_once() {
    let levels = 16;
    let mut body = String::new();
    for i in 0..levels {
        body.push_str(&format!(
            r#"<xs:simpleType name="U{i}"><xs:union memberTypes="U{j} W{j}"/></xs:simpleType>
               <xs:simpleType name="W{j}"><xs:restriction base="U{j}"/></xs:simpleType>"#,
            i = i,
            j = i + 1
        ));
    }
    body.push_str(&format!(
        r#"<xs:simpleType name="U{levels}"><xs:restriction base="xs:int"/></xs:simpleType><xs:element name="e" type="U0"/>"#
    ));
    let schema_xml = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{}</xs:schema>"#,
        body
    );
    let schema = parse(&schema_xml).unwrap();
    let validator = XsdValidator::from_schema(&schema).unwrap();
    let doc = parse("<e>x</e>").unwrap();
    take(&UNION_MEMBER_SELECTIONS);
    assert!(!validator.validate(&doc).is_empty());
    let selections = take(&UNION_MEMBER_SELECTIONS);
    assert!(
        selections <= 2 * levels,
        "{} member selections for a union DAG of depth {}",
        selections,
        levels
    );
}

/// The content type of an extension chain is built by moving the model
/// accumulated so far from step to step: an element of a type `depth` steps
/// from its root copies each schema particle once, not once per step below
/// it. Two chains: each step adds a sequence of one element (merged into the
/// base's sequence), or an optional choice (appended to it).
#[test]
fn extension_chain_content_is_built_in_linear_work() {
    let depth = 64;
    for own in [
        r#"<xs:sequence><xs:element name="e{k}" type="xs:string" minOccurs="0"/></xs:sequence>"#,
        r#"<xs:choice minOccurs="0"><xs:element name="e{k}" type="xs:string"/></xs:choice>"#,
    ] {
        let mut body = String::from(
            r#"<xs:complexType name="T0"><xs:sequence><xs:element name="e0" type="xs:string" minOccurs="0"/></xs:sequence></xs:complexType>"#,
        );
        for k in 1..=depth {
            body.push_str(&format!(
                r#"<xs:complexType name="T{k}"><xs:complexContent><xs:extension base="T{}">{}</xs:extension></xs:complexContent></xs:complexType>"#,
                k - 1,
                own.replace("{k}", &k.to_string())
            ));
        }
        let schema_xml = format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{}<xs:element name="x" type="T{}"/></xs:schema>"#,
            body, depth
        );
        let schema = parse(&schema_xml).unwrap();
        let validator = XsdValidator::from_schema(&schema).unwrap();
        let doc = parse("<x/>").unwrap();
        take(&CONTENT_PARTICLE_COPIES);
        assert!(validator.validate(&doc).is_empty());
        let copies = take(&CONTENT_PARTICLE_COPIES);
        assert!(
            copies <= 4 * (depth + 1),
            "{} particle copies for one element of a {}-step chain",
            copies,
            depth
        );
    }
}
