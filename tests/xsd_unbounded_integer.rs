//! `xs:integer` and the types derived from it, on values of any length. The
//! value space of `integer` is unbounded (XSD 1.0 Part 2, 3.3.13), so neither
//! validity nor a facet may depend on a machine integer; the bounded types
//! (`long` … `unsignedByte`) still refuse every value outside their range.

mod common;
use common::parse;

use uppsala::XsdValidator;

const XS: &str = r#"xmlns:xs="http://www.w3.org/2001/XMLSchema""#;

/// `1` followed by `n - 1` zeros: 10^(n-1), a number of `n` digits.
fn power(n: usize) -> String {
    format!("1{}", "0".repeat(n - 1))
}

/// `n` nines: 10^n - 1.
fn nines(n: usize) -> String {
    "9".repeat(n)
}

/// 10^(n-1) + 1.
fn power_plus_one(n: usize) -> String {
    format!("1{}1", "0".repeat(n - 2))
}

fn validator(body: &str) -> XsdValidator {
    let xsd = format!(r#"<xs:schema {XS}>{body}</xs:schema>"#);
    XsdValidator::from_schema(&parse(&xsd).expect("schema parses")).expect("schema builds")
}

/// Validate `<e>value</e>` against `type_body` (the content of an anonymous
/// `xs:simpleType`, or `type="..."` when it starts with `type=`).
fn check(type_body: &str, cases: &[(String, bool)]) {
    let element = if let Some(t) = type_body.strip_prefix("type=") {
        format!(r#"<xs:element name="e" type={t}/>"#)
    } else {
        format!(r#"<xs:element name="e"><xs:simpleType>{type_body}</xs:simpleType></xs:element>"#)
    };
    let v = validator(&element);
    for (value, valid) in cases {
        let errors = v.validate(&parse(&format!("<e>{value}</e>")).expect("instance parses"));
        let short: String = value.chars().take(24).collect();
        assert_eq!(
            errors.is_empty(),
            *valid,
            "{type_body} with {short}… ({} chars): {:?}",
            value.len(),
            errors
                .first()
                .map(|e| e.to_string().chars().take(120).collect::<String>())
        );
    }
}

fn cases(list: &[(&str, bool)]) -> Vec<(String, bool)> {
    list.iter().map(|(v, ok)| (v.to_string(), *ok)).collect()
}

#[test]
fn integer_accepts_values_of_any_length() {
    let mut list = vec![];
    for n in [39, 40, 1000] {
        list.push((power(n), true));
        list.push((format!("-{}", power(n)), true));
        list.push((format!("+{}", nines(n)), true));
        list.push((format!("{}x", power(n)), false));
        list.push((format!("{}.0", power(n)), false));
        list.push((format!("--{}", power(n)), false));
        list.push((format!("1 {}", power(n)), false));
    }
    list.push((format!("{}1", "0".repeat(1000)), true));
    list.push((format!(" {} ", power(40)), true));
    list.extend(cases(&[
        ("+", false),
        ("-", false),
        ("", false),
        ("\u{0661}", false),
    ]));
    check(r#"type="xs:integer""#, &list);
}

#[test]
fn range_facets_compare_long_values_exactly() {
    let p40 = power(40);
    let restriction = |facet: &str, value: &str| {
        format!(
            r#"<xs:restriction base="xs:integer"><xs:{facet} value="{value}"/></xs:restriction>"#
        )
    };
    // minInclusive 10^39 (40 digits).
    check(
        &restriction("minInclusive", &p40),
        &[
            (p40.clone(), true),
            (nines(39), false),
            (power(1000), true),
            (format!("-{}", power(1000)), false),
            (format!("000{}", p40), true),
        ],
    );
    // maxInclusive 10^39.
    check(
        &restriction("maxInclusive", &p40),
        &[
            (p40.clone(), true),
            (power_plus_one(40), false),
            (nines(39), true),
            (power(1000), false),
            (format!("-{}", power(1000)), true),
        ],
    );
    // minExclusive 10^39.
    check(
        &restriction("minExclusive", &p40),
        &[
            (p40.clone(), false),
            (power_plus_one(40), true),
            (power(1000), true),
            (nines(39), false),
        ],
    );
    // maxExclusive 10^39.
    check(
        &restriction("maxExclusive", &p40),
        &[
            (p40.clone(), false),
            (nines(39), true),
            (power(1000), false),
            (format!("-{}", power(1000)), true),
        ],
    );
    // A facet value of 1 000 digits, on the schema side.
    let p1000 = power(1000);
    check(
        &restriction("minInclusive", &p1000),
        &[
            (p1000.clone(), true),
            (nines(999), false),
            (power(1001), true),
        ],
    );
    check(
        &restriction("maxExclusive", &format!("-{p1000}")),
        &[
            (format!("-{}", power(1001)), true),
            (format!("-{p1000}"), false),
            ("0".into(), false),
        ],
    );
}

#[test]
fn total_digits_counts_long_values() {
    let td = |n: usize| {
        format!(
            r#"<xs:restriction base="xs:integer"><xs:totalDigits value="{n}"/></xs:restriction>"#
        )
    };
    check(
        &td(40),
        &[
            (power(40), true),
            (nines(39), true),
            (format!("-000{}", nines(40)), true),
            (power(41), false),
            (power(1000), false),
        ],
    );
    check(
        &td(1000),
        &[
            (power(1000), true),
            (format!("-{}", nines(1000)), true),
            (power(1001), false),
        ],
    );
}

#[test]
fn enumeration_and_fixed_compare_long_values_by_value() {
    let p40 = power(40);
    let p1000 = power(1000);
    check(
        &format!(
            r#"<xs:restriction base="xs:integer">
                 <xs:enumeration value="{p40}"/><xs:enumeration value="{p1000}"/>
               </xs:restriction>"#
        ),
        &[
            (p40.clone(), true),
            (format!("+00{p40}"), true),
            (format!("0{p1000}"), true),
            (power_plus_one(40), false),
            (power_plus_one(1000), false),
            (nines(39), false),
        ],
    );
    let v = validator(&format!(
        r#"<xs:element name="e" type="xs:integer" fixed="{p40}"/>
           <xs:element name="a"><xs:complexType>
             <xs:attribute name="n" type="xs:integer" fixed="{p1000}"/>
           </xs:complexType></xs:element>"#
    ));
    for (xml, valid) in [
        (format!("<e>{p40}</e>"), true),
        (format!("<e>00{p40}</e>"), true),
        (format!("<e>{}</e>", power_plus_one(40)), false),
        (format!("<e>{}</e>", nines(39)), false),
        (format!("<e>{}</e>", power(1000)), false),
        (format!(r#"<a n="+{p1000}"/>"#), true),
        (format!(r#"<a n="{}"/>"#, power_plus_one(1000)), false),
    ] {
        let errors = v.validate(&parse(&xml).expect("instance parses"));
        assert_eq!(errors.is_empty(), valid, "{}…: {errors:?}", &xml[..20]);
    }
}

#[test]
fn bounded_types_refuse_every_value_out_of_range() {
    // (type, least value, greatest value); "" is unbounded.
    let types = [
        ("long", "-9223372036854775808", "9223372036854775807"),
        ("int", "-2147483648", "2147483647"),
        ("short", "-32768", "32767"),
        ("byte", "-128", "127"),
        ("unsignedLong", "0", "18446744073709551615"),
        ("unsignedInt", "0", "4294967295"),
        ("unsignedShort", "0", "65535"),
        ("unsignedByte", "0", "255"),
        ("nonNegativeInteger", "0", ""),
        ("positiveInteger", "1", ""),
        ("nonPositiveInteger", "", "0"),
        ("negativeInteger", "", "-1"),
    ];
    let step = |v: &str, up: bool| -> String {
        // v ± 1 for the small bounds above, done on i128 for the test only.
        let n: i128 = v.parse().unwrap();
        (if up { n + 1 } else { n - 1 }).to_string()
    };
    for (name, min, max) in types {
        let mut list = vec![];
        if !min.is_empty() {
            list.push((min.to_string(), true));
            list.push((step(min, false), false));
            list.push((format!("-{}", power(1000)), false));
            list.push((format!("-{}", power(40)), false));
        } else {
            list.push((format!("-{}", power(1000)), true));
            list.push((format!("-{}", power(39)), true));
        }
        if !max.is_empty() {
            list.push((max.to_string(), true));
            list.push((step(max, true), false));
            list.push((power(1000), false));
            list.push((power(40), false));
            list.push((power(39), false));
            let digits = max.trim_start_matches('-');
            let sign = if max.starts_with('-') { "-" } else { "" };
            list.push((format!("{sign}{}{digits}", "0".repeat(1000)), true));
        } else {
            list.push((power(1000), true));
            list.push((power(40), true));
            list.push((format!("+{}", power(39)), true));
        }
        list.push((format!("{}x", power(40)), false));
        check(&format!(r#"type="xs:{name}""#), &list);
    }
    // A minus sign on zero: nonNegativeInteger allows it (Part 2,
    // 3.3.20.1); the unsigned types have no minus sign at all.
    check(
        r#"type="xs:nonNegativeInteger""#,
        &cases(&[("-0", true), ("+0", true)]),
    );
    check(
        r#"type="xs:nonPositiveInteger""#,
        &cases(&[("-0", true), ("+0", true)]),
    );
    for name in [
        "unsignedLong",
        "unsignedInt",
        "unsignedShort",
        "unsignedByte",
    ] {
        check(
            &format!(r#"type="xs:{name}""#),
            &cases(&[("-0", false), ("+0", true), ("0", true)]),
        );
    }
}

#[test]
fn forty_digit_integer_is_valid() {
    let p40 = power(40);
    check(r#"type="xs:integer""#, &[(p40.clone(), true)]);
    check(
        r#"<xs:restriction base="xs:integer"><xs:minExclusive value="0"/></xs:restriction>"#,
        &[(p40, true), ("0".into(), false)],
    );
}
