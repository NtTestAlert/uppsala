//! Built-in type validation helpers for XSD datatypes.
//!
//! Provides validation of values against XSD built-in types (string, boolean,
//! decimal, float, double, integer variants, date/time types, binary types,
//! name types, etc.), whitespace normalization, and facet enforcement.

use std::cmp::Ordering;
use std::num::IntErrorKind;

use crate::dom::{Document, NodeId};
use crate::error::ValidationError;
use crate::namespace::build_resolver_for_node;
use crate::parser::is_xml_whitespace;
use crate::xsd_regex::XsdRegex;

use super::datetime::{
    is_valid_date, is_valid_datetime, is_valid_duration, is_valid_gday, is_valid_gmonth,
    is_valid_gmonthday, is_valid_gyear, is_valid_gyearmonth, is_valid_time, normalize_datetime_tz,
};
use super::decimal::{compare_decimal_strings, compare_values};
use super::types::{BuiltInType, Facet, WhiteSpaceHandling};

/// Strip leading and trailing XML white space (`#x20`, `#x9`, `#xD`, `#xA`)
/// and nothing else. `str::trim` also strips Unicode white space such as
/// U+00A0, which is an ordinary character in an XML value: a value padded
/// with it is not in the lexical space of a numeric or temporal type.
pub(crate) fn trim_xml_whitespace(s: &str) -> &str {
    s.trim_matches(is_xml_whitespace)
}

/// Split a value at XML white space only (see `trim_xml_whitespace`),
/// dropping empty pieces.
pub(crate) fn split_xml_whitespace(s: &str) -> impl Iterator<Item = &str> {
    s.split(is_xml_whitespace).filter(|t| !t.is_empty())
}

/// Check if a string is a valid NCName (non-colonized name).
pub(crate) fn is_valid_ncname(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Check if a string is a valid XML Name (allows colons, unlike NCName).
/// NameStartChar = letter | '_' | ':'
/// NameChar = NameStartChar | digit | '.' | '-'
/// Covers MS tests: Name001/004/005/006/014/017/018
fn is_valid_xml_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_' || first == ':') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// Whether a type carries an XSD timezone/fractional-second lexical form whose
/// enumeration values must be normalized before comparison (e.g. `...+00:00`
/// vs `...Z`). All XSD date/time types are timezone-bearing; non-temporal types
/// (string, etc.) must keep their lexical value so a string that merely looks
/// like a timestamp is not silently normalized.
fn is_temporal_type(base_type: &BuiltInType) -> bool {
    matches!(
        base_type,
        BuiltInType::DateTime
            | BuiltInType::Time
            | BuiltInType::Date
            | BuiltInType::GYear
            | BuiltInType::GYearMonth
            | BuiltInType::GMonth
            | BuiltInType::GMonthDay
            | BuiltInType::GDay
    )
}

/// Range-validate a value against a date-like temporal base type, so an invalid
/// facet bound (e.g. `--99` gMonth) is rejected instead of compared lexically.
fn is_valid_date_like(s: &str, base_type: &BuiltInType) -> bool {
    match base_type {
        BuiltInType::Date => is_valid_date(s),
        BuiltInType::GYear => is_valid_gyear(s),
        BuiltInType::GYearMonth => is_valid_gyearmonth(s),
        BuiltInType::GMonth => is_valid_gmonth(s),
        BuiltInType::GMonthDay => is_valid_gmonthday(s),
        BuiltInType::GDay => is_valid_gday(s),
        _ => true,
    }
}

/// Whether `value`, a lexically valid value of the date or time primitive
/// `primitive`, lies so far from year zero that its instant cannot be
/// represented. Such a value is never compared: it equals nothing, and a
/// range or enumeration facet refuses it by name. The lexical space of
/// `gYear` and `gYearMonth` has no bound on the year's digits.
pub(crate) fn instant_is_unrepresentable(value: &str, primitive: &BuiltInType) -> bool {
    match primitive {
        BuiltInType::DateTime => is_valid_datetime(value) && datetime_to_instant(value).is_none(),
        BuiltInType::Date
        | BuiltInType::GYear
        | BuiltInType::GYearMonth
        | BuiltInType::GMonth
        | BuiltInType::GMonthDay
        | BuiltInType::GDay => {
            is_valid_date_like(value, primitive) && date_like_to_instant(value, primitive).is_none()
        }
        _ => false,
    }
}

/// A temporal value placed on the timeline: `seconds` is UTC-normalized when
/// the lexical form carries a timezone, otherwise local. Local vs UTC-normalized
/// instants are only partially ordered (see `compare_temporal_instants`).
struct TemporalInstant {
    seconds: i128,
    fraction: String,
    has_tz: bool,
}

/// Maximum timezone offset (14:00) in seconds. Per the XSD order relation on
/// dateTime (Part 2 section 3.2.7.4), a timezone-less value compared against a
/// timezoned one is determinate only when they are more than this far apart.
const MAX_TZ_OFFSET_SECONDS: i128 = 14 * 3_600;

/// Reference year used to place the recurring gMonth/gMonthDay/gDay types on
/// the timeline for comparison (a leap year, so --02-29 is representable),
/// mirroring the XSD 1.1 timeline mapping.
const G_TYPE_REFERENCE_YEAR: i128 = 1972;

fn compare_facet_values(
    value: &str,
    facet_value: &str,
    base_type: &BuiltInType,
) -> Option<Ordering> {
    match base_type {
        BuiltInType::DateTime => compare_datetime_values(value, facet_value),
        BuiltInType::Time => compare_time_values(value, facet_value),
        BuiltInType::Date
        | BuiltInType::GYear
        | BuiltInType::GYearMonth
        | BuiltInType::GMonth
        | BuiltInType::GMonthDay
        | BuiltInType::GDay => {
            // Fail closed on lexically-parseable but out-of-range operands, as
            // compare_datetime_values/compare_time_values do. A raw comparison of
            // an invalid facet bound would otherwise silently accept instances.
            if !is_valid_date_like(value, base_type) || !is_valid_date_like(facet_value, base_type)
            {
                return None;
            }
            let left = date_like_to_instant(value, base_type)?;
            let right = date_like_to_instant(facet_value, base_type)?;
            compare_temporal_instants(&left, &right)
        }
        BuiltInType::Float => compare_float_values(value, facet_value, true),
        BuiltInType::Double => compare_float_values(value, facet_value, false),
        BuiltInType::Duration => compare_duration_values(value, facet_value),
        _ => Some(compare_values(value, facet_value)),
    }
}

/// `float` / `double` semantics. Values compare as in **XSD 1.1** Part 2
/// (3.3.4, 3.3.5), which the crate targets: `NaN` is incomparable, so it
/// satisfies no range facet, and is identical to itself, so it matches a
/// `NaN` enumeration or fixed value; `-0` equals `0`. XSD 1.0 instead
/// ordered `NaN` above every value and `-0` below `0`. The lexical space
/// (see `validate_builtin_value`) is XSD 1.0's: `+INF` is refused, as the
/// W3C 2006 test suite requires, although XSD 1.1 accepts it. That is a
/// known inconsistency, kept on purpose from 0.10.1 because it fails closed.
///
/// Parse an XSD `float`/`double` lexical form (`INF`, `-INF`, `NaN` or a
/// decimal/scientific number). A `float` is parsed directly to the nearest
/// single-precision value, its value space (round-half-even), and then
/// widened exactly; going through `f64` first would round twice. Forms Rust
/// accepts but XSD does not (`inf`, `infinity`, ...) are rejected.
fn parse_xsd_float(s: &str, single: bool) -> Option<f64> {
    let s = trim_xml_whitespace(s);
    let value = match s {
        "INF" => f64::INFINITY,
        "-INF" => f64::NEG_INFINITY,
        "NaN" => f64::NAN,
        _ => {
            let numeric = s
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E'));
            if s.is_empty() || !numeric {
                return None;
            }
            if single {
                f64::from(s.parse::<f32>().ok()?)
            } else {
                s.parse::<f64>().ok()?
            }
        }
    };
    Some(value)
}

/// Order two `float`/`double` values. `NaN` is incomparable (`None`) and
/// `-0` equals `0` (XSD 1.1; see `parse_xsd_float`).
fn compare_float_values(value: &str, facet_value: &str, single: bool) -> Option<Ordering> {
    parse_xsd_float(value, single)?.partial_cmp(&parse_xsd_float(facet_value, single)?)
}

/// A duration split into months and seconds: `whole_seconds` plus the
/// fractional digits `fraction` (trailing zeros dropped).
struct DurationParts {
    negative: bool,
    months: i128,
    whole_seconds: i128,
    fraction: String,
}

/// Parse `-?PnYnMnDTnHnMnS` into months and seconds.
fn parse_duration_parts(s: &str) -> Option<DurationParts> {
    let s = trim_xml_whitespace(s);
    let (negative, rest) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let rest = rest.strip_prefix('P')?;
    let (date_part, time_part) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };
    let mut months: i128 = 0;
    let mut days: i128 = 0;
    let mut seconds: i128 = 0;
    let mut fraction = String::new();
    let mut number = String::new();
    for c in date_part.chars() {
        match c {
            '0'..='9' => number.push(c),
            'Y' | 'M' | 'D' => {
                let n: i128 = number.parse().ok()?;
                number.clear();
                match c {
                    'Y' => months = months.checked_add(n.checked_mul(12)?)?,
                    'M' => months = months.checked_add(n)?,
                    _ => days = days.checked_add(n)?,
                }
            }
            _ => return None,
        }
    }
    if !number.is_empty() {
        return None;
    }
    for c in time_part.unwrap_or("").chars() {
        match c {
            '0'..='9' | '.' => number.push(c),
            'H' | 'M' | 'S' => {
                let (whole, frac) = match number.split_once('.') {
                    Some((w, f)) if c == 'S' => (w.to_string(), f.to_string()),
                    Some(_) => return None,
                    None => (number.clone(), String::new()),
                };
                number.clear();
                let n: i128 = if whole.is_empty() && !frac.is_empty() {
                    0
                } else {
                    whole.parse().ok()?
                };
                let unit = match c {
                    'H' => 3_600,
                    'M' => 60,
                    _ => 1,
                };
                seconds = seconds.checked_add(n.checked_mul(unit)?)?;
                if c == 'S' {
                    if !frac.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    fraction = frac.trim_end_matches('0').to_string();
                }
            }
            _ => return None,
        }
    }
    if !number.is_empty() {
        return None;
    }
    Some(DurationParts {
        negative,
        months,
        whole_seconds: days.checked_mul(86_400)?.checked_add(seconds)?,
        fraction,
    })
}

/// Order two durations per XSD 1.0 Part 2 section 3.2.6.2: each is added to
/// the four reference dateTimes 1696-09-01, 1697-02-01, 1903-03-01 and
/// 1903-07-01 (UTC); the order holds only if it is the same for all four.
/// Otherwise the durations are incomparable (`None`), for example `P1M`
/// against `P30D`. Durations too large to place, or with more than 18
/// fractional second digits, are treated as incomparable too.
fn compare_duration_values(value: &str, facet_value: &str) -> Option<Ordering> {
    const REFERENCES: [(i128, i128); 4] = [(1696, 9), (1697, 2), (1903, 3), (1903, 7)];
    const MAX_YEAR: i128 = 1_000_000_000_000;
    let left = parse_duration_parts(value)?;
    let right = parse_duration_parts(facet_value)?;
    let scale = left.fraction.len().max(right.fraction.len());
    if scale > 18 {
        return None;
    }
    let factor = 10i128.checked_pow(scale as u32)?;
    let instant = |d: &DurationParts, year: i128, month: i128| -> Option<i128> {
        let sign = if d.negative { -1 } else { 1 };
        let month_index = (month - 1).checked_add(sign * d.months)?;
        let y = year.checked_add(month_index.div_euclid(12))?;
        if y.abs() > MAX_YEAR {
            return None;
        }
        let m = (month_index.rem_euclid(12) + 1) as u32;
        let start = days_from_civil(y, m, 1)?.checked_mul(86_400)?;
        let fraction: i128 = if d.fraction.is_empty() {
            0
        } else {
            let digits = format!("{:0<width$}", d.fraction, width = scale);
            digits.parse().ok()?
        };
        let span = d.whole_seconds.checked_mul(factor)?.checked_add(fraction)?;
        start.checked_mul(factor)?.checked_add(sign * span)
    };
    let mut result = None;
    for (year, month) in REFERENCES {
        let order = instant(&left, year, month)?.cmp(&instant(&right, year, month)?);
        match result {
            Some(previous) if previous != order => return None,
            _ => result = Some(order),
        }
    }
    result
}

/// The primitive type whose value space a built-in type's values belong to,
/// for equality across a derivation chain or between union members.
fn primitive_type(bt: &BuiltInType) -> BuiltInType {
    match bt {
        BuiltInType::Integer
        | BuiltInType::Long
        | BuiltInType::Int
        | BuiltInType::Short
        | BuiltInType::Byte
        | BuiltInType::NonNegativeInteger
        | BuiltInType::PositiveInteger
        | BuiltInType::NonPositiveInteger
        | BuiltInType::NegativeInteger
        | BuiltInType::UnsignedLong
        | BuiltInType::UnsignedInt
        | BuiltInType::UnsignedShort
        | BuiltInType::UnsignedByte => BuiltInType::Decimal,
        BuiltInType::NormalizedString
        | BuiltInType::Token
        | BuiltInType::Language
        | BuiltInType::Name
        | BuiltInType::NCName
        | BuiltInType::ID
        | BuiltInType::IDREF
        | BuiltInType::ENTITY
        | BuiltInType::NMTOKEN
        | BuiltInType::AnyType
        | BuiltInType::AnySimpleType => BuiltInType::String,
        other => other.clone(),
    }
}

/// The value range of an integer-family built-in type. The value space of
/// `integer` is unbounded, so a value is never refused for its length alone.
/// A value that fits in `i128` is compared as a number. Every bound lies well
/// inside `i128`, so a longer value is beyond every bound on its side: refused
/// if that side is bounded, and otherwise checked on its digit string, against
/// the other bound written as a digit string.
struct IntegerRange {
    /// The type's name, for messages.
    name: &'static str,
    /// The least value, or `None` when the type is unbounded below.
    min: Option<IntegerBound>,
    /// The greatest value, or `None` when the type is unbounded above.
    max: Option<IntegerBound>,
    /// Whether a leading `-` is refused, even on zero. The lexical space of
    /// the `unsigned*` types has no minus sign (Part 2, 3.3.21.1 to
    /// 3.3.24.1); a leading `+` is accepted for them, as before.
    no_minus: bool,
}

/// A bound of an integer-family type, as a decimal digit string and as the
/// same number.
#[derive(Clone, Copy)]
struct IntegerBound {
    digits: &'static str,
    value: i128,
}

const fn bound(digits: &'static str, value: i128) -> Option<IntegerBound> {
    Some(IntegerBound { digits, value })
}

const fn integer_range(
    name: &'static str,
    min: Option<IntegerBound>,
    max: Option<IntegerBound>,
    no_minus: bool,
) -> IntegerRange {
    IntegerRange {
        name,
        min,
        max,
        no_minus,
    }
}

/// The range of `bt`, one of the 13 integer-family built-in types; any other
/// type gets the unbounded range of `integer`.
fn integer_type_range(bt: &BuiltInType) -> IntegerRange {
    let zero = bound("0", 0);
    match bt {
        BuiltInType::Long => integer_range(
            "long",
            bound("-9223372036854775808", i64::MIN as i128),
            bound("9223372036854775807", i64::MAX as i128),
            false,
        ),
        BuiltInType::Int => integer_range(
            "int",
            bound("-2147483648", i32::MIN as i128),
            bound("2147483647", i32::MAX as i128),
            false,
        ),
        BuiltInType::Short => integer_range(
            "short",
            bound("-32768", i16::MIN as i128),
            bound("32767", i16::MAX as i128),
            false,
        ),
        BuiltInType::Byte => integer_range(
            "byte",
            bound("-128", i8::MIN as i128),
            bound("127", i8::MAX as i128),
            false,
        ),
        BuiltInType::NonNegativeInteger => integer_range("nonNegativeInteger", zero, None, false),
        BuiltInType::PositiveInteger => {
            integer_range("positiveInteger", bound("1", 1), None, false)
        }
        BuiltInType::NonPositiveInteger => integer_range("nonPositiveInteger", None, zero, false),
        BuiltInType::NegativeInteger => {
            integer_range("negativeInteger", None, bound("-1", -1), false)
        }
        BuiltInType::UnsignedLong => integer_range(
            "unsignedLong",
            zero,
            bound("18446744073709551615", u64::MAX as i128),
            true,
        ),
        BuiltInType::UnsignedInt => integer_range(
            "unsignedInt",
            zero,
            bound("4294967295", u32::MAX as i128),
            true,
        ),
        BuiltInType::UnsignedShort => integer_range(
            "unsignedShort",
            zero,
            bound("65535", u16::MAX as i128),
            true,
        ),
        BuiltInType::UnsignedByte => {
            integer_range("unsignedByte", zero, bound("255", u8::MAX as i128), true)
        }
        _ => integer_range("integer", None, None, false),
    }
}

/// Whether `v`, already whitespace-normalized, is a value of the integer type
/// with `range`: an optional sign and one or more ASCII digits
/// (`[\-+]?[0-9]+`) whose value lies within the bounds. The cost is linear in
/// the length of `v`, and nothing is allocated.
fn is_valid_integer_value(v: &str, range: &IntegerRange) -> bool {
    if range.no_minus && v.starts_with('-') {
        return false;
    }
    // `i128::from_str` accepts exactly `[\-+]?[0-9]+`. It stops at the first
    // digit that overflows: such a value, if it is an integer at all, is
    // beyond every bound on that side, so a bounded side refuses it at once.
    match v.parse::<i128>() {
        Ok(n) => {
            return !matches!(range.min, Some(b) if n < b.value)
                && !matches!(range.max, Some(b) if n > b.value);
        }
        Err(e) => match e.kind() {
            IntErrorKind::PosOverflow if range.max.is_some() => return false,
            IntErrorKind::NegOverflow if range.min.is_some() => return false,
            _ => {}
        },
    }
    let digits = v.strip_prefix(['+', '-']).unwrap_or(v);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    // Beyond `i128`: compare the digit strings.
    if let Some(min) = range.min {
        if !matches!(
            compare_decimal_strings(v, min.digits),
            Some(Ordering::Greater | Ordering::Equal)
        ) {
            return false;
        }
    }
    if let Some(max) = range.max {
        if !matches!(
            compare_decimal_strings(v, max.digits),
            Some(Ordering::Less | Ordering::Equal)
        ) {
            return false;
        }
    }
    true
}

/// Whether two whitespace-normalized lexical values denote the same value.
/// Each is read as a value of its built-in type; values of different
/// primitive types are never equal. Numbers, booleans, dates and times,
/// durations and binary types compare by value (`01` equals `1`, `1.0E0`
/// equals `1`); every other type compares the normalized strings. For
/// `float`/`double` this is XSD 1.1's "equal or identical" of enumeration
/// and fixed values: `NaN` matches `NaN`, `-0` matches `0`.
pub(crate) fn values_equal(
    left: &str,
    left_type: &BuiltInType,
    right: &str,
    right_type: &BuiltInType,
) -> bool {
    let primitive = primitive_type(left_type);
    if primitive != primitive_type(right_type) {
        return false;
    }
    match primitive {
        BuiltInType::Decimal => compare_decimal_strings(left, right) == Some(Ordering::Equal),
        BuiltInType::Float | BuiltInType::Double => {
            let single = primitive == BuiltInType::Float;
            match (
                parse_xsd_float(left, single),
                parse_xsd_float(right, single),
            ) {
                (Some(a), Some(b)) => a == b || (a.is_nan() && b.is_nan()),
                _ => false,
            }
        }
        BuiltInType::Boolean => {
            let as_bool = |s: &str| match trim_xml_whitespace(s) {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            };
            matches!((as_bool(left), as_bool(right)), (Some(a), Some(b)) if a == b)
        }
        BuiltInType::Duration => {
            compare_facet_values(
                trim_xml_whitespace(left),
                trim_xml_whitespace(right),
                &primitive,
            ) == Some(Ordering::Equal)
        }
        _ if is_temporal_type(&primitive) => {
            let (left, right) = (trim_xml_whitespace(left), trim_xml_whitespace(right));
            if instant_is_unrepresentable(left, &primitive)
                || instant_is_unrepresentable(right, &primitive)
            {
                return false;
            }
            compare_facet_values(left, right, &primitive) == Some(Ordering::Equal)
                || normalize_datetime_tz(left) == normalize_datetime_tz(right)
        }
        BuiltInType::HexBinary => {
            trim_xml_whitespace(left).eq_ignore_ascii_case(trim_xml_whitespace(right))
        }
        BuiltInType::Base64Binary => left
            .chars()
            .filter(|c| !is_xml_whitespace(*c))
            .eq(right.chars().filter(|c| !is_xml_whitespace(*c))),
        _ => left == right,
    }
}

/// Whether an enumeration value, as written in the schema, matches `text`,
/// an already normalized value of `base_type`. The literal is normalized
/// with `ws`, the mode of the type the facet restricts, then both are
/// compared as values.
pub(crate) fn enumeration_matches(
    text: &str,
    literal: &str,
    base_type: &BuiltInType,
    ws: &WhiteSpaceHandling,
) -> bool {
    values_equal(
        text,
        base_type,
        &apply_whitespace_normalization(literal, ws),
        base_type,
    )
}

fn compare_datetime_values(value: &str, facet_value: &str) -> Option<Ordering> {
    // Fail closed on lexically-parseable but out-of-range values (e.g. year 0000,
    // month 99, hour 99). Facet values are stored as raw strings and are not
    // otherwise range-checked, so an invalid minInclusive/maxInclusive must not
    // yield a comparable ordering.
    if !is_valid_datetime(value) || !is_valid_datetime(facet_value) {
        return None;
    }
    let left = datetime_to_instant(value)?;
    let right = datetime_to_instant(facet_value)?;
    compare_temporal_instants(&left, &right)
}

fn compare_time_values(value: &str, facet_value: &str) -> Option<Ordering> {
    // Fail closed on out-of-range time strings (e.g. 99:99:99Z); see
    // compare_datetime_values for the rationale.
    if !is_valid_time(value) || !is_valid_time(facet_value) {
        return None;
    }
    let left = time_to_instant(value)?;
    let right = time_to_instant(facet_value)?;
    compare_temporal_instants(&left, &right)
}

fn datetime_to_instant(value: &str) -> Option<TemporalInstant> {
    let (date, time) = value.split_once('T')?;
    let (year, month, day) = parse_xsd_date_parts(date)?;
    let (hour, minute, second, fraction, offset_minutes) = parse_xsd_time_parts(time)?;
    let time_of_day = i128::from(hour) * 3_600 + i128::from(minute) * 60 + i128::from(second);
    let local_seconds = days_from_civil(year, month, day)?
        .checked_mul(86_400)?
        .checked_add(time_of_day)?;
    Some(TemporalInstant {
        seconds: local_seconds.checked_sub(i128::from(offset_minutes.unwrap_or(0)) * 60)?,
        fraction: normalize_fraction(&fraction),
        has_tz: offset_minutes.is_some(),
    })
}

fn time_to_instant(value: &str) -> Option<TemporalInstant> {
    let (hour, minute, second, fraction, offset_minutes) = parse_xsd_time_parts(value)?;
    let local_seconds = i128::from(hour) * 3_600 + i128::from(minute) * 60 + i128::from(second);
    Some(TemporalInstant {
        seconds: local_seconds - i128::from(offset_minutes.unwrap_or(0)) * 60,
        fraction: normalize_fraction(&fraction),
        has_tz: offset_minutes.is_some(),
    })
}

/// Map a date-like temporal value (date, gYear, gYearMonth, gMonth, gMonthDay,
/// gDay) onto the timeline as the starting instant of its period, so ordering
/// honors timezone offsets and numeric years instead of lexical form.
fn date_like_to_instant(value: &str, base_type: &BuiltInType) -> Option<TemporalInstant> {
    let (body, offset_minutes) = split_tz_suffix(value);
    let (year, month, day) = match base_type {
        BuiltInType::Date => parse_xsd_date_parts(body)?,
        BuiltInType::GYear => (body.parse().ok()?, 1, 1),
        BuiltInType::GYearMonth => {
            let (year, month) = body.rsplit_once('-')?;
            (year.parse().ok()?, month.parse().ok()?, 1)
        }
        BuiltInType::GMonth => {
            // Accept --MM and the XSD 1.0 legacy --MM-- form.
            let month = body.strip_prefix("--")?.trim_end_matches("--");
            (G_TYPE_REFERENCE_YEAR, month.parse().ok()?, 1)
        }
        BuiltInType::GMonthDay => {
            let (month, day) = body.strip_prefix("--")?.split_once('-')?;
            (
                G_TYPE_REFERENCE_YEAR,
                month.parse().ok()?,
                day.parse().ok()?,
            )
        }
        BuiltInType::GDay => (
            G_TYPE_REFERENCE_YEAR,
            1,
            body.strip_prefix("---")?.parse().ok()?,
        ),
        _ => return None,
    };
    Some(TemporalInstant {
        seconds: days_from_civil(year, month, day)?
            .checked_mul(86_400)?
            .checked_sub(i128::from(offset_minutes.unwrap_or(0)) * 60)?,
        fraction: String::new(),
        has_tz: offset_minutes.is_some(),
    })
}

/// Compare two timeline instants per XSD's partial order: values that both
/// carry (or both omit) a timezone are totally ordered; a timezone-less value
/// against a timezoned one is determinate only when more than 14 hours apart.
/// Indeterminate comparisons return None, which the facet checks report as an
/// error (fail closed) instead of assuming UTC.
fn compare_temporal_instants(left: &TemporalInstant, right: &TemporalInstant) -> Option<Ordering> {
    match (left.has_tz, right.has_tz) {
        (true, true) | (false, false) => Some(compare_instant_parts(
            left.seconds,
            &left.fraction,
            right.seconds,
            &right.fraction,
        )),
        (false, true) => {
            // Timezone-less left could lie anywhere in [-14:00, +14:00]:
            // left < right only if even its latest interpretation is earlier,
            // and left > right only if even its earliest one is later.
            let latest = left.seconds.checked_add(MAX_TZ_OFFSET_SECONDS)?;
            let earliest = left.seconds.checked_sub(MAX_TZ_OFFSET_SECONDS)?;
            if compare_instant_parts(latest, &left.fraction, right.seconds, &right.fraction)
                == Ordering::Less
            {
                Some(Ordering::Less)
            } else if compare_instant_parts(
                earliest,
                &left.fraction,
                right.seconds,
                &right.fraction,
            ) == Ordering::Greater
            {
                Some(Ordering::Greater)
            } else {
                None
            }
        }
        (true, false) => compare_temporal_instants(right, left).map(Ordering::reverse),
    }
}

fn compare_instant_parts(
    left_seconds: i128,
    left_fraction: &str,
    right_seconds: i128,
    right_fraction: &str,
) -> Ordering {
    match left_seconds.cmp(&right_seconds) {
        Ordering::Equal => compare_fraction(left_fraction, right_fraction),
        ord => ord,
    }
}

fn normalize_fraction(fraction: &str) -> String {
    fraction.trim_end_matches('0').to_string()
}

fn compare_fraction(left: &str, right: &str) -> Ordering {
    // Digit-wise comparison with an implied '0' past the shorter end. The
    // fractional part is attacker-sized (validation allows any number of
    // digits), so nothing is allocated: the shared prefix is an ordered byte
    // slice comparison (lowered to SIMD-optimized memcmp) and the leftover
    // tail only decides the ordering if it contains a non-zero digit. Both
    // strings are ASCII digits by the time they reach a comparison.
    let left = left.as_bytes();
    let right = right.as_bytes();
    let shared = left.len().min(right.len());
    match left[..shared].cmp(&right[..shared]) {
        Ordering::Equal => {}
        ord => return ord,
    }
    if left[shared..].iter().any(|&b| b != b'0') {
        Ordering::Greater
    } else if right[shared..].iter().any(|&b| b != b'0') {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

fn parse_xsd_date_parts(date: &str) -> Option<(i128, u32, u32)> {
    let (negative, rest) = date
        .strip_prefix('-')
        .map(|s| (true, s))
        .unwrap_or((false, date));
    let mut parts = rest.split('-');
    let year_str = parts.next()?;
    let month_str = parts.next()?;
    let day_str = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let year: i128 = year_str.parse().ok()?;
    let month: u32 = month_str.parse().ok()?;
    let day: u32 = day_str.parse().ok()?;
    Some((if negative { -year } else { year }, month, day))
}

fn parse_xsd_time_parts(time: &str) -> Option<(u32, u32, u32, String, Option<i32>)> {
    let (time, offset_minutes) = split_tz_suffix(time);
    let mut parts = time.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second_part = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (second_str, frac_str) = second_part.split_once('.').unwrap_or((second_part, ""));
    let second: u32 = second_str.parse().ok()?;
    if !frac_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((hour, minute, second, frac_str.to_string(), offset_minutes))
}

/// Split an optional trailing timezone (`Z` or `+hh:mm`/`-hh:mm`) from a
/// temporal lexical form. Returns the remaining body and the offset in
/// minutes; a missing timezone is `None`, not UTC. Unrecognized suffixes are
/// left in the body, where the callers' numeric parsing fails closed (the
/// overall lexical form is range-validated separately before comparison).
fn split_tz_suffix(s: &str) -> (&str, Option<i32>) {
    if let Some(stripped) = s.strip_suffix('Z') {
        return (stripped, Some(0));
    }
    if s.len() >= 6 {
        let tz_start = s.len() - 6;
        let tz = &s.as_bytes()[tz_start..];
        if !tz.is_ascii() {
            return (s, None);
        }
        let sign = match tz[0] {
            b'+' => 1,
            b'-' => -1,
            _ => return (s, None),
        };
        if tz[3] != b':' {
            return (s, None);
        }
        if let (Ok(hours), Ok(minutes)) = (
            s[tz_start + 1..tz_start + 3].parse::<i32>(),
            s[tz_start + 4..].parse::<i32>(),
        ) {
            return (&s[..tz_start], Some(sign * (hours * 60 + minutes)));
        }
    }
    (s, None)
}

/// Days from 1970-01-01 to a proleptic Gregorian date, or `None` when the
/// year is too far from zero for the count to be represented. Every step is
/// checked: a wrapped count could equal another date's, so an overflow must
/// never yield a number.
fn days_from_civil(year: i128, month: u32, day: u32) -> Option<i128> {
    let m = i128::from(month);
    let y = year.checked_sub(i128::from(month <= 2))?;
    let era = if y >= 0 { y } else { y.checked_sub(399)? } / 400;
    let yoe = y.checked_sub(era.checked_mul(400)?)?;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i128::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era.checked_mul(146_097)?
        .checked_add(doe)?
        .checked_sub(719_468)
}

fn push_facet_compare_error(
    facet_name: &str,
    value: &str,
    facet_value: &str,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
) {
    errors.push(ValidationError {
        message: format!(
            "Cannot compare value '{}' with {} {} for this datatype",
            value, facet_name, facet_value
        ),
        line: Some(doc.node_line(node)),
        column: Some(doc.node_column(node)),
    });
}

/// Check if a string is a valid QName (prefix:localname or just localname).
/// Both prefix and localname must be valid NCNames.
/// Covers MS tests: QName001/004/005/007/008/010/011
fn is_valid_qname(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if let Some(colon_pos) = s.find(':') {
        // Must have exactly one colon
        if s[colon_pos + 1..].contains(':') {
            return false;
        }
        let prefix = &s[..colon_pos];
        let local = &s[colon_pos + 1..];
        is_valid_ncname(prefix) && is_valid_ncname(local)
    } else {
        is_valid_ncname(s)
    }
}

/// Determine the whiteSpace normalization mode for a built-in type.
/// Per XSD Part 2: string→preserve, normalizedString→replace,
/// token and all types derived from token→collapse.
pub(crate) fn whitespace_for_type(bt: &BuiltInType) -> WhiteSpaceHandling {
    match bt {
        BuiltInType::String | BuiltInType::AnyType | BuiltInType::AnySimpleType => {
            WhiteSpaceHandling::Preserve
        }
        BuiltInType::NormalizedString => WhiteSpaceHandling::Replace,
        // Token and everything derived from it use collapse
        _ => WhiteSpaceHandling::Collapse,
    }
}

/// Apply XSD whiteSpace normalization to a string value.
/// - Preserve: return as-is
/// - Replace: replace CR, LF, TAB with space
/// - Collapse: replace CR/LF/TAB with space, collapse runs of spaces, strip leading/trailing
pub(crate) fn apply_whitespace_normalization(text: &str, mode: &WhiteSpaceHandling) -> String {
    match mode {
        WhiteSpaceHandling::Preserve => text.to_string(),
        WhiteSpaceHandling::Replace => text
            .chars()
            .map(|c| {
                if c == '\r' || c == '\n' || c == '\t' {
                    ' '
                } else {
                    c
                }
            })
            .collect(),
        WhiteSpaceHandling::Collapse => {
            let replaced: String = text
                .chars()
                .map(|c| {
                    if c == '\r' || c == '\n' || c == '\t' {
                        ' '
                    } else {
                        c
                    }
                })
                .collect();
            let mut result = String::with_capacity(replaced.len());
            let mut prev_space = true; // true to strip leading spaces
            for c in replaced.chars() {
                if c == ' ' {
                    if !prev_space {
                        result.push(' ');
                    }
                    prev_space = true;
                } else {
                    result.push(c);
                    prev_space = false;
                }
            }
            // Strip trailing space
            if result.ends_with(' ') {
                result.pop();
            }
            result
        }
    }
}

pub(crate) fn validate_builtin_value(
    text: &str,
    bt: &BuiltInType,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
    lenient: bool,
) {
    // Apply XSD whiteSpace normalization before any validation.
    // Per XSD Part 2, whiteSpace is a pre-processing step applied to the
    // ·lexical representation· before all other facet checks and type validation.
    let ws_mode = whitespace_for_type(bt);
    let normalized = apply_whitespace_normalization(text, &ws_mode);
    let text = &normalized;

    match bt {
        BuiltInType::String | BuiltInType::AnyType | BuiltInType::AnySimpleType => {
            // Any string is valid
        }
        BuiltInType::NormalizedString => {
            // After replace normalization, CR/LF/TAB should already be gone.
            // This check is for safety.
            if text.contains('\r') || text.contains('\n') || text.contains('\t') {
                errors.push(ValidationError {
                    message: "normalizedString must not contain CR, LF, or TAB".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Token => {
            // After collapse normalization, text is already collapsed.
            // Nothing further to check for plain xs:token.
        }
        BuiltInType::Boolean => {
            let v = trim_xml_whitespace(text);
            if !matches!(v, "true" | "false" | "1" | "0") {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid boolean", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS tests: decimal019-022/025 — reject scientific notation, INF, NaN
        BuiltInType::Decimal => {
            let v = trim_xml_whitespace(text);
            // XSD decimal lexical space: [+-]?digit+(.digit+)?
            // Must NOT accept scientific notation (E/e), INF, NaN
            let valid = {
                let s = if v.starts_with('+') || v.starts_with('-') {
                    &v[1..]
                } else {
                    v
                };
                if s.is_empty() {
                    false
                } else if let Some(dot_pos) = s.find('.') {
                    let integer_part = &s[..dot_pos];
                    let frac_part = &s[dot_pos + 1..];
                    // Integer part can be empty if there's a fractional part (e.g., ".5")
                    // but at least one of integer or fractional must be non-empty
                    (integer_part.is_empty() || integer_part.chars().all(|c| c.is_ascii_digit()))
                        && !frac_part.is_empty()
                        && frac_part.chars().all(|c| c.is_ascii_digit())
                } else {
                    s.chars().all(|c| c.is_ascii_digit())
                }
            };
            if !valid {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid decimal", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS tests: float018/022-026, double018/022-026 — case-sensitive special values.
        // `+INF` is refused per the XSD 1.0 lexical space, which the W3C 2006
        // suite tests; XSD 1.1 would accept it (see `parse_xsd_float`).
        // Deliberately unchanged from 0.10.1, and a known inconsistency with
        // the XSD 1.1 value semantics of these types: the refusal fails
        // closed, and accepting `+INF` would fail MS DataTypes `float018` and
        // `double018`. Pinned by the test `plus_inf_is_refused_for_float_and_double`.
        BuiltInType::Float | BuiltInType::Double => {
            let v = trim_xml_whitespace(text);
            let valid = if v == "INF" || v == "-INF" || v == "NaN" {
                true
            } else if v.eq_ignore_ascii_case("inf")
                || v.eq_ignore_ascii_case("nan")
                || v.eq_ignore_ascii_case("-nan")
                || v.eq_ignore_ascii_case("+nan")
                || v == "+INF"
                || v == "+inf"
                || v == "infinity"
                || v == "+infinity"
                || v == "-infinity"
                || v.eq_ignore_ascii_case("infinity")
            {
                false
            } else {
                v.parse::<f64>().is_ok()
            };
            if !valid {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid float/double", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Integer
        | BuiltInType::Long
        | BuiltInType::Int
        | BuiltInType::Short
        | BuiltInType::Byte
        | BuiltInType::NonNegativeInteger
        | BuiltInType::PositiveInteger
        | BuiltInType::NonPositiveInteger
        | BuiltInType::NegativeInteger
        | BuiltInType::UnsignedLong
        | BuiltInType::UnsignedInt
        | BuiltInType::UnsignedShort
        | BuiltInType::UnsignedByte => {
            let v = trim_xml_whitespace(text);
            let range = integer_type_range(bt);
            if !is_valid_integer_value(v, &range) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid {}", text, range.name),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::DateTime => {
            let v = trim_xml_whitespace(text);
            if !is_valid_datetime(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid dateTime", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Date => {
            let v = trim_xml_whitespace(text);
            if !is_valid_date(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid date", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Time => {
            let v = trim_xml_whitespace(text);
            if !is_valid_time(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid time", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS test: hexBinary003 — strip internal whitespace before validation
        BuiltInType::HexBinary => {
            let v: String = text.chars().filter(|c| !is_xml_whitespace(*c)).collect();
            if !v.len().is_multiple_of(2) || !v.chars().all(|c| c.is_ascii_hexdigit()) {
                errors.push(ValidationError {
                    message: format!("'{}' is not valid hexBinary", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::Base64Binary => {
            let v: String = text.chars().filter(|c| !is_xml_whitespace(*c)).collect();
            let is_valid = if v.is_empty() {
                true
            } else if !v.len().is_multiple_of(4) {
                false
            } else {
                let pad_count = v.chars().rev().take_while(|&c| c == '=').count();
                if pad_count > 2 {
                    false
                } else {
                    let data_part = &v[..v.len() - pad_count];
                    let pad_part = &v[v.len() - pad_count..];
                    data_part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
                        && pad_part.chars().all(|c| c == '=')
                }
            };
            if !is_valid {
                errors.push(ValidationError {
                    message: format!("'{}' is not valid base64Binary", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::AnyURI => {
            // `anyURI` validation here is intentionally minimal: after XSD whitespace normalization,
            // strict mode rejects values containing a space (and thus any collapsed whitespace).
            // libxml2 is more permissive and accepts spaces too; in lenient mode we match it
            // (this also allows whitespace-separated tokens that reach here as a single anyURI
            // value to validate). Strict mode
            // keeps the space check.
            let v = trim_xml_whitespace(text);
            if !lenient && v.contains(' ') {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid anyURI", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NCName | BuiltInType::ID | BuiltInType::IDREF => {
            let v = trim_xml_whitespace(text);
            if !is_valid_ncname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid NCName/ID/IDREF", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS tests: language008/010 — enforce [a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})* pattern
        BuiltInType::Language => {
            let v = trim_xml_whitespace(text);
            let valid = if v.is_empty() {
                false
            } else {
                let subtags: Vec<&str> = v.split('-').collect();
                if subtags[0].is_empty()
                    || subtags[0].len() > 8
                    || !subtags[0].chars().all(|c| c.is_ascii_alphabetic())
                {
                    false
                } else {
                    subtags[1..].iter().all(|sub| {
                        !sub.is_empty()
                            && sub.len() <= 8
                            && sub.chars().all(|c| c.is_ascii_alphanumeric())
                    })
                }
            };
            if !valid {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid language tag", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NMTOKEN => {
            let v = trim_xml_whitespace(text);
            if v.is_empty()
                || !v
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
            {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid NMTOKEN", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::NMTOKENS => {
            let v = trim_xml_whitespace(text);
            if v.is_empty() {
                errors.push(ValidationError {
                    message: "NMTOKENS must contain at least one token".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else {
                for token in split_xml_whitespace(v) {
                    if token.is_empty()
                        || !token.chars().all(|c| {
                            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
                        })
                    {
                        errors.push(ValidationError {
                            message: format!("'{}' is not a valid NMTOKEN in NMTOKENS", token),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
        }
        BuiltInType::IDREFS => {
            let v = trim_xml_whitespace(text);
            if v.is_empty() {
                errors.push(ValidationError {
                    message: "IDREFS must contain at least one IDREF".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else {
                for token in split_xml_whitespace(v) {
                    if !is_valid_ncname(token) {
                        errors.push(ValidationError {
                            message: format!("'{}' is not a valid IDREF in IDREFS", token),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
        }
        BuiltInType::NOTATION => {
            let v = trim_xml_whitespace(text);
            if !is_valid_ncname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid NOTATION value", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::ENTITY => {
            let v = trim_xml_whitespace(text);
            if !is_valid_ncname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid ENTITY value", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::ENTITIES => {
            let v = trim_xml_whitespace(text);
            if v.is_empty() {
                errors.push(ValidationError {
                    message: "ENTITIES must contain at least one ENTITY".to_string(),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else {
                for token in split_xml_whitespace(v) {
                    if !is_valid_ncname(token) {
                        errors.push(ValidationError {
                            message: format!("'{}' is not a valid ENTITY in ENTITIES", token),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
        }
        BuiltInType::Duration => {
            let v = trim_xml_whitespace(text);
            if !is_valid_duration(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid duration", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GYear => {
            let v = trim_xml_whitespace(text);
            if !is_valid_gyear(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gYear", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GYearMonth => {
            let v = trim_xml_whitespace(text);
            if !is_valid_gyearmonth(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gYearMonth", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GMonth => {
            let v = trim_xml_whitespace(text);
            if !is_valid_gmonth(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gMonth", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GMonthDay => {
            let v = trim_xml_whitespace(text);
            if !is_valid_gmonthday(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gMonthDay", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        BuiltInType::GDay => {
            let v = trim_xml_whitespace(text);
            if !is_valid_gday(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid gDay", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS tests: Name001/004/005/006/014/017/018
        BuiltInType::Name => {
            let v = trim_xml_whitespace(text);
            if !is_valid_xml_name(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid Name", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        // MS tests: QName001/004/005/007/008/010/011
        // Note: NOTATION is handled above (validates as NCName, not full QName).
        BuiltInType::QName => {
            let v = trim_xml_whitespace(text);
            if !is_valid_qname(v) {
                errors.push(ValidationError {
                    message: format!("'{}' is not a valid QName", text),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else if let Some(colon_pos) = v.find(':') {
                let prefix = &v[..colon_pos];
                let resolver = build_resolver_for_node(doc, node);
                if resolver.resolve(prefix).is_none() {
                    errors.push(ValidationError {
                        message: format!("QName prefix '{}' is not bound", prefix),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
    }
}

/// Validate a facet for a list type. Length facets count items, not characters.
pub(crate) fn validate_list_facet(
    items: &[&str],
    facet: &Facet,
    text: &str,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
) {
    let item_count = items.len();
    match facet {
        Facet::MinLength(min) => {
            if item_count < *min {
                errors.push(ValidationError {
                    message: format!("List has {} items, less than minLength {}", item_count, min),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MaxLength(max) => {
            if item_count > *max {
                errors.push(ValidationError {
                    message: format!("List has {} items, exceeds maxLength {}", item_count, max),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Length(len) => {
            if item_count != *len {
                errors.push(ValidationError {
                    message: format!("List has {} items, expected length {}", item_count, len),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Enumeration(values) => {
            // For list enumerations, the entire space-collapsed value must match
            let collapsed: String = split_xml_whitespace(text).collect::<Vec<_>>().join(" ");
            if !values.contains(&collapsed) {
                errors.push(ValidationError {
                    message: format!(
                        "'{}' is not one of the allowed values: {:?}",
                        collapsed, values
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::Pattern(pattern) => {
            // Pattern facets on lists apply to the whole collapsed space-separated value
            match XsdRegex::compile(pattern) {
                Ok(re) if !re.is_match(text) => {
                    errors.push(ValidationError {
                        message: format!("Value '{}' does not match pattern '{}'", text, pattern),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
                Ok(_) => {}
                Err(e) => errors.push(ValidationError {
                    message: format!("Pattern facet '{}' could not be compiled: {}", pattern, e),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                }),
            }
        }
        Facet::WhiteSpace(_) => {}
        _ => {
            // Other facets (min/max inclusive/exclusive, digits) don't apply to lists
        }
    }
}

/// Compute the "length" of a value for Length/MinLength/MaxLength facets,
/// taking into account type-specific semantics per XSD 1.1 spec:
/// - hexBinary: number of octets (string length / 2)
/// - base64Binary: number of decoded octets
/// - QName/NOTATION: number of URI-qualified characters (URI + local-name length)
/// - All others: number of characters
pub(crate) fn type_aware_length(
    text: &str,
    base_type: &BuiltInType,
    doc: &Document,
    node: NodeId,
) -> usize {
    match base_type {
        BuiltInType::HexBinary => {
            // Each pair of hex characters = 1 octet
            let trimmed = trim_xml_whitespace(text);
            trimmed.len() / 2
        }
        BuiltInType::Base64Binary => {
            // Count decoded octets from base64
            let stripped: String = text.chars().filter(|c| !is_xml_whitespace(*c)).collect();
            if stripped.is_empty() {
                return 0;
            }
            let padding = stripped.chars().rev().take_while(|&c| c == '=').count();
            let non_padding = stripped.len() - padding;
            // Each 4 base64 chars = 3 bytes, minus padding bytes
            (non_padding * 3) / 4
        }
        BuiltInType::QName => {
            // XSD spec: QName length = len(namespace URI) + len(local name).
            // We resolve the QName prefix against the instance document's namespace context.
            let trimmed = trim_xml_whitespace(text);
            let (prefix, local_name) = if let Some(colon_pos) = trimmed.find(':') {
                (&trimmed[..colon_pos], &trimmed[colon_pos + 1..])
            } else {
                ("", trimmed)
            };

            if prefix.is_empty() {
                // Unprefixed QName: in no namespace, length = local name length.
                local_name.len()
            } else {
                // Prefixed QName: resolve the prefix to a namespace URI
                let resolver = build_resolver_for_node(doc, node);
                if let Some(ns_uri) = resolver.resolve(prefix) {
                    ns_uri.len() + local_name.len()
                } else {
                    // Prefix not bound — fall back to local name length
                    local_name.len()
                }
            }
        }
        _ => text.len(),
    }
}

/// Check one facet on `text`, a value of `base_type` that is already
/// whitespace-normalized. `ws` is the whitespace mode of the type the
/// enumeration facet restricts (its base; the built-in type's own when
/// `None`): enumeration literals are values of that type, so they are
/// normalized with it before the comparison (XSD 1.0 Part 2 4.3.5).
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_facet(
    text: &str,
    facet: &Facet,
    base_type: &BuiltInType,
    ws: Option<&WhiteSpaceHandling>,
    doc: &Document,
    node: NodeId,
    errors: &mut Vec<ValidationError>,
    enforce_qname_length_facets: bool,
) {
    // When enforce_qname_length_facets is false, skip length/minLength/maxLength
    // for QName and NOTATION types (NIST test suite interpretation of W3C Bug #4009).
    let skip_length = !enforce_qname_length_facets
        && matches!(base_type, BuiltInType::QName | BuiltInType::NOTATION);

    match facet {
        Facet::MinLength(min) => {
            if !skip_length {
                let len = type_aware_length(text, base_type, doc, node);
                if len < *min {
                    errors.push(ValidationError {
                        message: format!("Value length {} is less than minLength {}", len, min),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::MaxLength(max) => {
            if !skip_length {
                let len = type_aware_length(text, base_type, doc, node);
                if len > *max {
                    errors.push(ValidationError {
                        message: format!("Value length {} exceeds maxLength {}", len, max),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::Length(expected) => {
            if !skip_length {
                let len = type_aware_length(text, base_type, doc, node);
                if len != *expected {
                    errors.push(ValidationError {
                        message: format!("Value length {} does not match length {}", len, expected),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::Enumeration(values) => {
            // `text` is already whitespace-normalized for its type; the
            // literals are normalized with the mode of the facet's base
            // type, then compared as values. Nothing is trimmed: a string
            // keeps its spaces.
            let ws = ws
                .cloned()
                .unwrap_or_else(|| whitespace_for_type(base_type));
            let unrepresentable =
                instant_is_unrepresentable(trim_xml_whitespace(text), &primitive_type(base_type));
            let match_found = !unrepresentable
                && values
                    .iter()
                    .any(|v| enumeration_matches(text, v, base_type, &ws));
            if unrepresentable {
                errors.push(ValidationError {
                    message: format!(
                        "Value '{}' cannot be compared with the allowed values: its year is too \
                         far from zero to place on the timeline",
                        trim_xml_whitespace(text)
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else if !match_found {
                errors.push(ValidationError {
                    message: format!("'{}' is not one of the allowed values: {:?}", text, values),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::MinInclusive(min) => {
            match compare_facet_values(trim_xml_whitespace(text), min, base_type) {
                Some(Ordering::Less) => {
                    errors.push(ValidationError {
                        message: format!(
                            "Value '{}' is less than minInclusive {}",
                            trim_xml_whitespace(text),
                            min
                        ),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
                Some(_) => {}
                None => push_facet_compare_error(
                    "minInclusive",
                    trim_xml_whitespace(text),
                    min,
                    doc,
                    node,
                    errors,
                ),
            }
        }
        Facet::MaxInclusive(max) => {
            match compare_facet_values(trim_xml_whitespace(text), max, base_type) {
                Some(Ordering::Greater) => {
                    errors.push(ValidationError {
                        message: format!(
                            "Value '{}' exceeds maxInclusive {}",
                            trim_xml_whitespace(text),
                            max
                        ),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
                Some(_) => {}
                None => push_facet_compare_error(
                    "maxInclusive",
                    trim_xml_whitespace(text),
                    max,
                    doc,
                    node,
                    errors,
                ),
            }
        }
        Facet::MinExclusive(min) => {
            match compare_facet_values(trim_xml_whitespace(text), min, base_type) {
                Some(Ordering::Less | Ordering::Equal) => {
                    errors.push(ValidationError {
                        message: format!(
                            "Value '{}' is not greater than minExclusive {}",
                            trim_xml_whitespace(text),
                            min
                        ),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
                Some(_) => {}
                None => push_facet_compare_error(
                    "minExclusive",
                    trim_xml_whitespace(text),
                    min,
                    doc,
                    node,
                    errors,
                ),
            }
        }
        Facet::MaxExclusive(max) => {
            match compare_facet_values(trim_xml_whitespace(text), max, base_type) {
                Some(Ordering::Greater | Ordering::Equal) => {
                    errors.push(ValidationError {
                        message: format!(
                            "Value '{}' is not less than maxExclusive {}",
                            trim_xml_whitespace(text),
                            max
                        ),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
                Some(_) => {}
                None => push_facet_compare_error(
                    "maxExclusive",
                    trim_xml_whitespace(text),
                    max,
                    doc,
                    node,
                    errors,
                ),
            }
        }
        Facet::TotalDigits(max_digits) => {
            let digits = total_digits(text);
            if digits > *max_digits {
                errors.push(ValidationError {
                    message: format!("Total digits {} exceeds totalDigits {}", digits, max_digits),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
        Facet::FractionDigits(max_frac) => {
            if let Some(dot_pos) = text.find('.') {
                let frac = &text[dot_pos + 1..];
                let frac_len = frac.trim_end_matches('0').len();
                if frac_len > *max_frac {
                    errors.push(ValidationError {
                        message: format!(
                            "Fraction digits {} exceeds fractionDigits {}",
                            frac_len, max_frac
                        ),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
            }
        }
        Facet::Pattern(pattern) => match XsdRegex::compile(pattern) {
            Ok(re) if !re.is_match(text) => {
                errors.push(ValidationError {
                    message: format!("Value '{}' does not match pattern '{}'", text, pattern),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
            Ok(_) => {}
            Err(e) => errors.push(ValidationError {
                message: format!("Pattern facet '{}' could not be compiled: {}", pattern, e),
                line: Some(doc.node_line(node)),
                column: Some(doc.node_column(node)),
            }),
        },
        Facet::WhiteSpace(_) => {
            // White space normalization is applied during parsing
        }
    }
}

/// The number of digits `totalDigits` counts in a decimal lexical form. The
/// value is `i × 10^-n`, with `n` the fraction digits left after dropping
/// trailing zeros; the count is the larger of the digits of `i` (leading
/// zeros dropped, at least one) and `n`. So `0001.5` and `1.500` count 2
/// digits and `0.001` counts 3.
fn total_digits(text: &str) -> usize {
    let unsigned = trim_xml_whitespace(text).trim_start_matches(['+', '-']);
    let (int_part, frac_part) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let frac_part = frac_part.trim_end_matches('0');
    let significant = format!("{}{}", int_part, frac_part);
    let significant = significant.trim_start_matches('0');
    significant.len().max(1).max(frac_part.len())
}

#[cfg(test)]
mod integer_range_tests {
    use super::*;

    const INTEGER_TYPES: [BuiltInType; 13] = [
        BuiltInType::Integer,
        BuiltInType::Long,
        BuiltInType::Int,
        BuiltInType::Short,
        BuiltInType::Byte,
        BuiltInType::NonNegativeInteger,
        BuiltInType::PositiveInteger,
        BuiltInType::NonPositiveInteger,
        BuiltInType::NegativeInteger,
        BuiltInType::UnsignedLong,
        BuiltInType::UnsignedInt,
        BuiltInType::UnsignedShort,
        BuiltInType::UnsignedByte,
    ];

    /// The digit string and the number of every bound are the same value.
    #[test]
    fn bound_digits_and_values_agree() {
        for bt in &INTEGER_TYPES {
            let range = integer_type_range(bt);
            for b in [range.min, range.max].into_iter().flatten() {
                assert_eq!(b.digits.parse::<i128>(), Ok(b.value), "{}", range.name);
            }
        }
    }

    /// The `i128` path and the digit-string path decide alike at and around
    /// every bound.
    #[test]
    fn number_and_digit_string_paths_agree() {
        let by_digits = |v: &str, range: &IntegerRange| {
            [range.min, range.max]
                .iter()
                .zip([Ordering::Less, Ordering::Greater])
                .all(|(b, outside)| match b {
                    Some(b) => compare_decimal_strings(v, b.digits) != Some(outside),
                    None => true,
                })
        };
        for bt in &INTEGER_TYPES {
            let range = integer_type_range(bt);
            for b in [range.min, range.max].into_iter().flatten() {
                for n in [b.value - 1, b.value, b.value + 1] {
                    let v = n.to_string();
                    let expected = by_digits(&v, &range) && !(range.no_minus && n < 0);
                    assert_eq!(
                        is_valid_integer_value(&v, &range),
                        expected,
                        "{} {v}",
                        range.name
                    );
                }
            }
        }
    }
}
