//! XSD validation logic for element and content model validation.
//!
//! This module contains the core validation methods on `XsdValidator`:
//! - `validate()` — entry point: validates an entire document against the schema
//! - `validate_element()` — validates a single element against its declaration
//! - `validate_complex_content()` — validates attributes and content model of complex types
//! - `validate_sequence()` / `validate_choice()` / `validate_all()` — content model validators
//! - `validate_simple_content()` — validates text content against a simple type
//! - `validate_attribute_value()` — validates an attribute value against its declared type
//! - xsi:type resolution and type substitution blocking checks
//! - Substitution group matching for element declarations

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::dom::{Document, NodeId, NodeKind};
use crate::error::ValidationError;
use crate::namespace::build_resolver_for_node;
use crate::xsd_regex::XsdRegex;

use super::builtins::{
    apply_whitespace_normalization, split_xml_whitespace, trim_xml_whitespace,
    validate_builtin_value, validate_facet, validate_list_facet, values_equal, whitespace_for_type,
};
use super::parser::parse_builtin_type;
use super::types::*;
use super::wildcard::wildcard_allows_namespace;
use super::{XSI_NAMESPACE, XS_NAMESPACE};

fn attr_decl_matches(attr: &crate::dom::Attribute<'_>, decl: &AttributeDecl) -> bool {
    attr.name.local_name == decl.name
        && attr.name.namespace_uri.as_deref() == decl.namespace.as_deref()
}

/// Whether an attribute is one of the four XSI attributes that any element
/// may carry: `type`, `nil`, `schemaLocation` and `noNamespaceSchemaLocation`
/// in the XSI namespace (XSD 1.0 Part 1 §3.2.7, cvc-complex-type.3,
/// cvc-type.3.1.1). It is matched by namespace URI, never by prefix: a
/// prefix `xsi` bound to another namespace names an ordinary attribute, and
/// any other name in the XSI namespace is assessed like any other attribute.
fn is_exempt_xsi_attribute(attr: &crate::dom::Attribute<'_>) -> bool {
    attr.name.namespace_uri.as_deref() == Some(XSI_NAMESPACE)
        && matches!(
            &*attr.name.local_name,
            "type" | "nil" | "schemaLocation" | "noNamespaceSchemaLocation"
        )
}

/// Whether an attribute has the name of a namespace declaration: a name in
/// the reserved xmlns namespace (Namespaces in XML §3; the infoset's
/// [namespace attributes]) or, without a namespace URI, the unprefixed
/// `xmlns` or a name with the prefix `xmlns`.
///
/// No such attribute is a declaration. The parser keeps declarations apart
/// from the attributes, so only a tree built through the DOM API holds one,
/// and the serializer writes it out under another name, as an ordinary
/// attribute. It is therefore never exempt from assessment: it is refused by
/// name, so that a built tree is never accepted where its serialized form
/// would be refused. Any other name is an ordinary attribute, whatever its
/// local name: `q:xmlns` with `q` bound to `urn:q` is `{urn:q}xmlns`.
fn has_namespace_declaration_name(attr: &crate::dom::Attribute<'_>) -> bool {
    match attr.name.namespace_uri.as_deref() {
        Some(uri) => uri == crate::namespace::XMLNS_NAMESPACE,
        None => match attr.name.prefix.as_deref() {
            None => attr.name.local_name == "xmlns",
            Some(prefix) => prefix == "xmlns",
        },
    }
}

/// The refusal of an attribute with the name of a namespace declaration
/// (see [`has_namespace_declaration_name`]) on the element `element`.
fn namespace_declaration_name_error(
    attr: &crate::dom::Attribute<'_>,
    element: &str,
    doc: &Document,
    node: NodeId,
) -> ValidationError {
    let name = match (
        attr.name.namespace_uri.as_deref(),
        attr.name.prefix.as_deref(),
    ) {
        (Some(_), _) => attribute_display(attr),
        (None, Some(prefix)) => format!("{}:{}", prefix, attr.name.local_name),
        (None, None) => attr.name.local_name.to_string(),
    };
    ValidationError {
        message: format!(
            "Attribute '{}' on element '{}' has the name of a namespace declaration, \
             which an attribute cannot have",
            name, element
        ),
        line: Some(doc.node_line(node)),
        column: Some(doc.node_column(node)),
    }
}

/// An attribute's expanded name for messages: `{uri}local`, or `local`.
fn attribute_display(attr: &crate::dom::Attribute<'_>) -> String {
    match attr.name.namespace_uri.as_deref() {
        Some(uri) => format!("{{{}}}{}", uri, attr.name.local_name),
        None => attr.name.local_name.to_string(),
    }
}

/// Result of resolving an `xsi:type` attribute on an element.
///
/// When an instance element carries `xsi:type`, the validator resolves it to one of:
/// - A built-in XSD type (e.g. `xs:int`)
/// - A named schema type (simple or complex)
/// - Not found (error case)
enum XsiTypeResult {
    /// A built-in XSD type like xs:string, xs:int, etc.
    BuiltIn(BuiltInType),
    /// A named type definition from the schema, with the key it was found
    /// under.
    Named((Option<String>, String), Box<TypeDef>),
    /// The xsi:type QName could not be resolved.
    NotFound(String),
}

/// The content type of a complex type, see [`XsdValidator::content_type`].
enum ContentType<'a> {
    /// Empty, element-only or mixed content: the model and whether it is
    /// mixed.
    Model(Cow<'a, ContentModel>, bool),
    /// Simple content, read with `simple_content_types` of this definition.
    Simple(&'a ComplexTypeDef),
}

/// Whether a model has no particle: the explicit content of a derivation
/// is then empty (XSD 1.0 Part 1 §3.4.2, clause 2.1 of the complexContent
/// mapping).
fn model_is_empty(model: &ContentModel) -> bool {
    match model {
        ContentModel::Empty => true,
        ContentModel::Sequence(particles, _, max) => {
            particles.is_empty() || matches!(max, MaxOccurs::Bounded(0))
        }
        ContentModel::Choice(particles, min, max) => {
            (particles.is_empty() && *min == 0) || matches!(max, MaxOccurs::Bounded(0))
        }
        ContentModel::All(particles) => particles.is_empty(),
        ContentModel::SimpleContent(_) | ContentModel::Any => false,
    }
}

/// The content type of `derived`, an extension, given its base's.
///
/// The result is `sequence(base, own)` (XSD 1.0 Part 1 §3.4.2). A base that
/// is a sequence occurring once contributes its particles rather than one
/// nested particle, and so does an own sequence occurring once: a (1,1)
/// sequence inside a sequence matches exactly what its particles do in its
/// place. Any other base (a choice, or a sequence with other occurrences) is
/// kept whole as one particle; the result is then a (1,1) sequence, so the
/// next extension splices it. The model therefore nests at most one level
/// more than its root, whatever the derivation depth, and validating a child
/// never runs one frame deeper per step. The accumulated model is moved from
/// step to step, not copied, so building it is linear in the depth.
fn extend_content_type<'a>(base: ContentType<'a>, derived: &'a ComplexTypeDef) -> ContentType<'a> {
    // An extension without particles has the base's content type.
    if model_is_empty(&derived.content) {
        return base;
    }
    let own = ContentType::Model(Cow::Borrowed(&derived.content), derived.mixed);
    let ContentType::Model(base_model, _) = base else {
        // Particles added to simple content: not a valid schema
        // (cos-ct-extends.1.4); the extension's own particles apply.
        return own;
    };
    if model_is_empty(&base_model) {
        return own;
    }
    let is_group = |model: &ContentModel| {
        matches!(model, ContentModel::Sequence(..) | ContentModel::Choice(..))
    };
    if !is_group(&base_model) || !is_group(&derived.content) {
        // An `all` group on either side (not valid in an extension of
        // non-empty content in XSD 1.0), or the lax wildcard content of
        // xs:anyType: the extension's own particles apply.
        return own;
    }
    let base_model = match base_model {
        Cow::Owned(model) => model,
        Cow::Borrowed(model) => clone_model(model),
    };
    let mut particles = match base_model {
        ContentModel::Sequence(particles, 1, MaxOccurs::Bounded(1)) => particles,
        ContentModel::Sequence(particles, min, max) => vec![Particle {
            kind: ParticleKind::Sequence(particles),
            min_occurs: min,
            max_occurs: max,
        }],
        ContentModel::Choice(particles, min, max) => vec![Particle {
            kind: ParticleKind::Choice(particles),
            min_occurs: min,
            max_occurs: max,
        }],
        _ => return own,
    };
    match &derived.content {
        ContentModel::Sequence(own_particles, 1, MaxOccurs::Bounded(1)) => {
            particles.extend(clone_particles(own_particles));
        }
        ContentModel::Sequence(own_particles, min, max) => particles.push(Particle {
            kind: ParticleKind::Sequence(clone_particles(own_particles)),
            min_occurs: *min,
            max_occurs: *max,
        }),
        ContentModel::Choice(own_particles, min, max) => particles.push(Particle {
            kind: ParticleKind::Choice(clone_particles(own_particles)),
            min_occurs: *min,
            max_occurs: *max,
        }),
        _ => return own,
    }
    ContentType::Model(
        Cow::Owned(ContentModel::Sequence(particles, 1, MaxOccurs::Bounded(1))),
        derived.mixed,
    )
}

/// A copy of a schema's content model, for a content type built from it.
fn clone_model(model: &ContentModel) -> ContentModel {
    match model {
        ContentModel::Sequence(particles, min, max) => {
            ContentModel::Sequence(clone_particles(particles), *min, *max)
        }
        ContentModel::Choice(particles, min, max) => {
            ContentModel::Choice(clone_particles(particles), *min, *max)
        }
        other => other.clone(),
    }
}

/// A copy of a schema's particles, counted for the complexity tests.
fn clone_particles(particles: &[Particle]) -> Vec<Particle> {
    test_counters::note_many(&test_counters::CONTENT_PARTICLE_COPIES, particles.len());
    particles.to_vec()
}

impl XsdValidator {
    /// Validate a document against this schema.
    ///
    /// Finds the root element, looks up its global element declaration, and
    /// delegates to `validate_element()`. Returns a (possibly empty) list of
    /// validation errors.
    pub fn validate(&self, doc: &Document) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        let doc_elem = match doc.document_element() {
            Some(e) => e,
            None => {
                errors.push(ValidationError {
                    message: "Document has no root element".to_string(),
                    line: None,
                    column: None,
                });
                return errors;
            }
        };

        let elem = match doc.element(doc_elem) {
            Some(e) => e,
            None => return errors,
        };

        // Find matching top-level element declaration
        let key_with_ns = (
            elem.name.namespace_uri.as_deref().map(|s| s.to_string()),
            elem.name.local_name.to_string(),
        );
        let key_no_ns = (None, elem.name.local_name.to_string());

        let decl = self.elements.get(&key_with_ns).or_else(|| {
            if elem.name.namespace_uri.is_none() {
                self.elements.get(&key_no_ns)
            } else {
                None
            }
        });

        match decl {
            Some(decl) => {
                self.validate_element(doc, doc_elem, decl, &mut errors);
            }
            None => {
                errors.push(ValidationError {
                    message: format!(
                        "No element declaration found for '{}'",
                        elem.name.local_name
                    ),
                    line: Some(doc.node_line(doc_elem)),
                    column: Some(doc.node_column(doc_elem)),
                });
            }
        }

        errors
    }

    /// Check if an element has any child elements (not just text nodes).
    fn element_has_child_elements(&self, doc: &Document, node: NodeId) -> bool {
        for child in doc.children(node) {
            if let Some(NodeKind::Element(_)) = doc.node_kind(child) {
                return true;
            }
        }
        false
    }

    /// Get a display name for a built-in type (e.g. "xs:string").
    fn builtin_type_name(&self, bt: &BuiltInType) -> &'static str {
        match bt {
            BuiltInType::String => "xs:string",
            BuiltInType::Boolean => "xs:boolean",
            BuiltInType::Decimal => "xs:decimal",
            BuiltInType::Float => "xs:float",
            BuiltInType::Double => "xs:double",
            BuiltInType::Integer => "xs:integer",
            BuiltInType::Long => "xs:long",
            BuiltInType::Int => "xs:int",
            BuiltInType::Short => "xs:short",
            BuiltInType::Byte => "xs:byte",
            BuiltInType::NonNegativeInteger => "xs:nonNegativeInteger",
            BuiltInType::PositiveInteger => "xs:positiveInteger",
            BuiltInType::NonPositiveInteger => "xs:nonPositiveInteger",
            BuiltInType::NegativeInteger => "xs:negativeInteger",
            BuiltInType::UnsignedLong => "xs:unsignedLong",
            BuiltInType::UnsignedInt => "xs:unsignedInt",
            BuiltInType::UnsignedShort => "xs:unsignedShort",
            BuiltInType::UnsignedByte => "xs:unsignedByte",
            BuiltInType::DateTime => "xs:dateTime",
            BuiltInType::Date => "xs:date",
            BuiltInType::Time => "xs:time",
            BuiltInType::Duration => "xs:duration",
            BuiltInType::GYear => "xs:gYear",
            BuiltInType::GYearMonth => "xs:gYearMonth",
            BuiltInType::GMonth => "xs:gMonth",
            BuiltInType::GMonthDay => "xs:gMonthDay",
            BuiltInType::GDay => "xs:gDay",
            BuiltInType::HexBinary => "xs:hexBinary",
            BuiltInType::Base64Binary => "xs:base64Binary",
            BuiltInType::AnyURI => "xs:anyURI",
            BuiltInType::QName => "xs:QName",
            BuiltInType::NormalizedString => "xs:normalizedString",
            BuiltInType::Token => "xs:token",
            BuiltInType::Language => "xs:language",
            BuiltInType::Name => "xs:Name",
            BuiltInType::NCName => "xs:NCName",
            BuiltInType::ID => "xs:ID",
            BuiltInType::IDREF => "xs:IDREF",
            BuiltInType::IDREFS => "xs:IDREFS",
            BuiltInType::NMTOKEN => "xs:NMTOKEN",
            BuiltInType::NMTOKENS => "xs:NMTOKENS",
            BuiltInType::NOTATION => "xs:NOTATION",
            BuiltInType::ENTITY => "xs:ENTITY",
            BuiltInType::ENTITIES => "xs:ENTITIES",
            BuiltInType::AnyType => "xs:anyType",
            BuiltInType::AnySimpleType => "xs:anySimpleType",
        }
    }

    /// Resolve an `xsi:type` attribute on an element.
    ///
    /// Looks for `xsi:type` in the element's attributes, parses the QName value,
    /// resolves the prefix to a namespace URI, and looks up the type in the schema
    /// or built-in type registry.
    ///
    /// Returns `None` if no `xsi:type` is present.
    fn resolve_xsi_type(&self, doc: &Document, node: NodeId) -> Option<XsiTypeResult> {
        let elem = doc.element(node)?;

        // The attribute is found by namespace URI only: a prefix `xsi` bound
        // to another namespace names an ordinary attribute.
        let xsi_type_value = elem.get_attribute_ns(XSI_NAMESPACE, "type")?;

        // Parse the QName value (may be prefixed like "xs:int")
        let (prefix, local_name) = if let Some(colon_pos) = xsi_type_value.find(':') {
            (
                Some(&xsi_type_value[..colon_pos]),
                &xsi_type_value[colon_pos + 1..],
            )
        } else {
            (None, xsi_type_value)
        };

        // Resolve prefix to namespace URI
        let type_ns = if let Some(pfx) = prefix {
            // Look up prefix in namespace declarations. A prefix without a
            // binding names no type (cvc-elt.4.1).
            let resolver = build_resolver_for_node(doc, node);
            match resolver.resolve(pfx) {
                Some(uri) => Some(uri.to_string()),
                None => return Some(XsiTypeResult::NotFound(xsi_type_value.to_string())),
            }
        } else {
            // No prefix — use default namespace if present
            // Per XSD spec, an unprefixed QName in xsi:type uses the default namespace
            let resolver = build_resolver_for_node(doc, node);
            resolver.resolve_default().map(|s| s.to_string())
        };

        // Check if it's a built-in XSD type
        if type_ns.as_deref() == Some(XS_NAMESPACE) {
            if let Some(bt) = parse_builtin_type(local_name) {
                return Some(XsiTypeResult::BuiltIn(bt));
            }
        }

        // Look the expanded name up in the schema types. A type of another
        // namespace with the same local name is a different type, so there is
        // no fallback to the no-namespace key.
        let key = (type_ns, local_name.to_string());
        if let Some(td) = self.types.get(&key) {
            return Some(XsiTypeResult::Named(key, Box::new(td.clone())));
        }

        Some(XsiTypeResult::NotFound(xsi_type_value.to_string()))
    }

    /// Check if xsi:type substitution is blocked by element or type block constraints.
    ///
    /// Per XSD §3.4.4.2 "Type Derivation OK (Complex)", blocking is checked against:
    /// 1. The element declaration's block (`decl_block_ext`/`decl_block_rst`) — blocks any
    ///    derivation step in the entire chain that uses the blocked method.
    /// 2. The declared type's block (`decl_type_block_ext`/`decl_type_block_rst`) — same rule,
    ///    applied to the type that the element declaration refers to.
    ///
    /// Intermediate types' block constraints do NOT affect xsi:type substitution checking.
    ///
    /// Returns `Some(error_message)` if blocked, `None` if allowed.
    fn check_type_substitution_blocked(
        &self,
        xsi_type: &TypeDef,
        decl_block_ext: bool,
        decl_block_rst: bool,
        decl_type_block_ext: bool,
        decl_type_block_rst: bool,
    ) -> Option<String> {
        // Walk up the derivation chain of the xsi:type type.
        // Track which derivation methods appear in the chain.
        let mut has_extension_in_chain = false;
        let mut has_restriction_in_chain = false;
        let mut current = xsi_type;
        let mut seen = HashSet::new();

        while let TypeDef::Complex(ct) = current {
            let is_extension = match ct.derived_by_extension {
                Some(true) => true,
                Some(false) => false,
                None => break, // Not derived, we're at a root type
            };

            if is_extension {
                has_extension_in_chain = true;
            } else {
                has_restriction_in_chain = true;
            }

            // Check element-level block against accumulated chain
            if has_extension_in_chain && decl_block_ext {
                return Some(
                    "Type substitution blocked: derivation chain includes extension, which is blocked by element declaration".to_string(),
                );
            }
            if has_restriction_in_chain && decl_block_rst {
                return Some(
                    "Type substitution blocked: derivation chain includes restriction, which is blocked by element declaration".to_string(),
                );
            }

            // Check the declared type's block against accumulated chain
            if has_extension_in_chain && decl_type_block_ext {
                return Some(
                    "Type substitution blocked: derivation chain includes extension, which is blocked by the declared type".to_string(),
                );
            }
            if has_restriction_in_chain && decl_type_block_rst {
                return Some(
                    "Type substitution blocked: derivation chain includes restriction, which is blocked by the declared type".to_string(),
                );
            }

            // Move up to base type
            if let Some(ref base_key) = ct.base_type {
                if !seen.insert(base_key.clone()) {
                    return Some(format!(
                        "Type derivation cycle detected involving '{}'",
                        base_key.1
                    ));
                }
                if let Some(base_td) = self.types.get(base_key) {
                    current = base_td;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        None
    }

    /// Check if the `xsi:type` type found under `xsi_key` is the declared type
    /// or derived from it (cvc-elt.4.3), ignoring block constraints.
    fn is_type_derived_from_decl(
        &self,
        xsi_key: &(Option<String>, String),
        decl_type_ref: &TypeRef,
    ) -> bool {
        let decl_key = match decl_type_ref {
            TypeRef::Named(ns, name) => (ns.clone(), name.clone()),
            // Every type derives from xs:anyType; a named schema type is not
            // compared with the other built-in types here.
            TypeRef::BuiltIn(bt) => return *bt == BuiltInType::AnyType,
            // An anonymous type has no name, so no QName names it, and a
            // named type can only derive from a named base: no `xsi:type`
            // is validly derived from it.
            TypeRef::Inline(_) => return false,
            // Decided at build; never present in a built validator.
            TypeRef::Unqualified(_) => return false,
        };
        *xsi_key == decl_key || self.is_derived_from(xsi_key, &decl_key)
    }

    /// Check that a built-in `xsi:type` type is the declared type or derived
    /// from it (cvc-elt.4.3), and that no derivation step is blocked by the
    /// element declaration. A built-in type derives only from built-in types,
    /// so a declared schema type, named or anonymous, admits none.
    fn builtin_xsi_type_error(&self, xsi_type: &BuiltInType, decl: &ElementDecl) -> Option<String> {
        let not_derived = || {
            Some(format!(
                "xsi:type '{}' is not derived from the declared element type",
                self.builtin_type_name(xsi_type)
            ))
        };
        let TypeRef::BuiltIn(declared) = &decl.type_ref else {
            return not_derived();
        };
        let mut current = Some(xsi_type.clone());
        let mut steps = 0;
        while let Some(t) = current {
            if t == *declared {
                // Every built-in derivation step is a restriction, or a list
                // of xs:anySimpleType, which a block on restriction also
                // covers (cos-st-derived-ok 2.1).
                if steps > 0 && decl.block_restriction {
                    return Some(
                        "Type substitution blocked: derivation chain includes restriction, which is blocked by element declaration".to_string(),
                    );
                }
                return None;
            }
            current = builtin_base_type(&t);
            steps += 1;
        }
        not_derived()
    }

    /// Check if a type identified by `type_key` is derived (directly or transitively)
    /// from a type identified by `ancestor_key`.
    ///
    /// Walks up the derivation chain (via `base_type` links on complex types) up to
    /// 50 levels deep to prevent infinite loops.
    fn is_derived_from(
        &self,
        type_key: &(Option<String>, String),
        ancestor_key: &(Option<String>, String),
    ) -> bool {
        let mut current_key = type_key.clone();
        // Walk up to 50 levels to avoid infinite loops
        for _ in 0..50 {
            if let Some(td) = self.types.get(&current_key) {
                match td {
                    TypeDef::Complex(ct) => {
                        if let Some(ref base_key) = ct.base_type {
                            if base_key == ancestor_key {
                                return true;
                            }
                            current_key = base_key.clone();
                        } else {
                            return false; // no base type
                        }
                    }
                    TypeDef::Simple(_st) => {
                        // Simple types derive from their base built-in type
                        // For now, we can't easily walk the chain for simple types
                        // since base is a BuiltInType, not a key
                        return false;
                    }
                }
            } else {
                return false;
            }
        }
        false
    }

    /// Compute the effective attribute wildcard for a complex type.
    ///
    /// For types derived by extension, this is the union of the base type's
    /// effective wildcard and the derived type's own wildcard.
    /// For restriction types or types not derived, this is just the type's own wildcard.
    fn compute_effective_wildcard(&self, ct: &ComplexTypeDef) -> Option<AttributeWildcard> {
        self.compute_effective_wildcard_inner(ct, &mut HashSet::new())
    }

    fn compute_effective_wildcard_inner(
        &self,
        ct: &ComplexTypeDef,
        seen: &mut HashSet<(Option<String>, String)>,
    ) -> Option<AttributeWildcard> {
        if ct.derived_by_extension == Some(true) {
            // Get the base type's effective wildcard (recursively)
            let base_wildcard = if let Some(ref base_key) = ct.base_type {
                if !seen.insert(base_key.clone()) {
                    return None;
                }
                if let Some(TypeDef::Complex(base_ct)) = self.types.get(base_key) {
                    self.compute_effective_wildcard_inner(base_ct, seen)
                } else {
                    None
                }
            } else {
                None
            };

            // Union of base wildcard and derived wildcard
            match (&base_wildcard, &ct.attribute_wildcard) {
                (Some(base_wc), Some(derived_wc)) => Some(base_wc.union(derived_wc)),
                (Some(base_wc), None) => Some(base_wc.clone()),
                (None, Some(derived_wc)) => Some(derived_wc.clone()),
                (None, None) => None,
            }
        } else {
            // Restriction or not derived: use the type's own wildcard
            ct.attribute_wildcard.clone()
        }
    }

    /// Compute effective attributes for a complex type, including inherited attributes
    /// from the base type chain.
    ///
    /// - **Extension**: base attributes + derived attributes (derived can add new ones)
    /// - **Restriction**: derived attributes override base; attributes not mentioned in
    ///   the restriction are inherited; prohibited attributes are removed
    /// - **Not derived**: just the type's own attributes
    fn compute_effective_attributes(&self, ct: &ComplexTypeDef) -> Vec<AttributeDecl> {
        self.compute_effective_attributes_inner(ct, &mut HashSet::new())
    }

    fn compute_effective_attributes_inner(
        &self,
        ct: &ComplexTypeDef,
        seen: &mut HashSet<(Option<String>, String)>,
    ) -> Vec<AttributeDecl> {
        // Get base type's effective attributes
        let base_attrs = if let Some(ref base_key) = ct.base_type {
            if !seen.insert(base_key.clone()) {
                return Vec::new();
            }
            if let Some(TypeDef::Complex(base_ct)) = self.types.get(base_key) {
                self.compute_effective_attributes_inner(base_ct, seen)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        if base_attrs.is_empty() {
            return ct.attributes.clone();
        }

        // Attribute declarations are namespace-aware: merge and override by
        // expanded name, not local name, so declarations that share a local
        // name across namespaces are kept distinct.
        let same_expanded_name =
            |a: &AttributeDecl, b: &AttributeDecl| a.name == b.name && a.namespace == b.namespace;

        match ct.derived_by_extension {
            Some(true) => {
                // Extension: base attributes + derived attributes
                let mut result = base_attrs;
                for attr in &ct.attributes {
                    if !result.iter().any(|a| same_expanded_name(a, attr)) {
                        result.push(attr.clone());
                    }
                }
                result
            }
            Some(false) => {
                // Restriction: start with base attributes, then apply overrides
                // Attributes explicitly declared in restriction replace base attrs.
                // Attributes with use="prohibited" remove the attribute.
                let mut result = Vec::new();
                for base_attr in &base_attrs {
                    // Check if the restriction overrides or prohibits this attribute
                    let override_attr = ct
                        .attributes
                        .iter()
                        .find(|a| same_expanded_name(a, base_attr));
                    if let Some(oa) = override_attr {
                        // Use the overridden version (but check if prohibited)
                        if !oa.prohibited {
                            result.push(oa.clone());
                        }
                        // If prohibited, skip it (don't add to result)
                    } else {
                        // Not mentioned in restriction: inherit from base
                        result.push(base_attr.clone());
                    }
                }
                // Also add any new attributes from restriction that aren't in base
                // (unusual but technically possible)
                for attr in &ct.attributes {
                    if !attr.prohibited && !result.iter().any(|a| same_expanded_name(a, attr)) {
                        result.push(attr.clone());
                    }
                }
                result
            }
            None => {
                // Not derived
                ct.attributes.clone()
            }
        }
    }

    /// The content type of a complex type (XSD 1.0 Part 1 §3.4.2).
    ///
    /// A `complexContent` restriction has its own explicit content (empty
    /// when it declares no particle). An extension whose explicit content
    /// is empty has the base's content type; otherwise, when the base's
    /// content type is not empty, a sequence of the base's particle
    /// followed by the extension's. A type derived through `simpleContent`
    /// has simple content, read with [`Self::simple_content_types`].
    fn content_type<'a>(&'a self, ct: &'a ComplexTypeDef) -> ContentType<'a> {
        // The extension chain down to the first type that is not an
        // extension of a known complex type.
        let mut chain = vec![ct];
        let mut seen = HashSet::new();
        let mut current = ct;
        while current.derived_by_extension == Some(true) && !current.simple_content {
            let Some(key) = current.base_type.as_ref() else {
                break;
            };
            if !seen.insert(key) {
                break; // a derivation cycle, reported by the caller
            }
            match self.types.get(key) {
                Some(TypeDef::Complex(base)) => {
                    chain.push(base);
                    current = base;
                }
                // A missing base, or a simple type (not a valid
                // complexContent base): the type's own content only.
                _ => break,
            }
        }
        let root = chain.pop().unwrap_or(ct);
        let mut content = if root.simple_content {
            ContentType::Simple(root)
        } else {
            ContentType::Model(Cow::Borrowed(&root.content), root.mixed)
        };
        while let Some(derived) = chain.pop() {
            content = extend_content_type(content, derived);
        }
        content
    }

    fn has_complex_derivation_cycle(&self, ct: &ComplexTypeDef) -> bool {
        let mut seen = HashSet::new();
        let mut current = ct;

        while let Some(base_key) = &current.base_type {
            if !seen.insert(base_key.clone()) {
                return true;
            }
            match self.types.get(base_key) {
                Some(TypeDef::Complex(base_ct)) => current = base_ct,
                _ => return false,
            }
        }

        false
    }

    /// Validate a single element against its element declaration.
    ///
    /// This is the main per-element validation entry point. It handles:
    /// 1. Element reference resolution (is_ref → look up global declaration)
    /// 2. Abstract element rejection
    /// 3. `xsi:nil` processing (nillable elements with nil=true must be empty)
    /// 4. `xsi:type` override resolution and type substitution blocking
    /// 5. Type resolution and dispatch to complex/simple content validation
    /// 6. Identity constraint evaluation (key/unique/keyref)
    fn validate_element(
        &self,
        doc: &Document,
        node: NodeId,
        decl: &ElementDecl,
        errors: &mut Vec<ValidationError>,
    ) {
        // If this is an element reference, resolve the actual declaration from the
        // global elements map to get the real type_ref, nillable, block constraints, etc.
        if decl.is_ref {
            let key = (decl.namespace.clone(), decl.name.clone());
            if let Some(global_decl) = self.elements.get(&key) {
                let resolved = global_decl.clone();
                self.validate_element(doc, node, &resolved, errors);
                return;
            }
            let display = decl
                .namespace
                .as_ref()
                .map(|uri| format!("{{{}}}{}", uri, decl.name))
                .unwrap_or_else(|| decl.name.clone());
            errors.push(ValidationError {
                message: format!("Element reference '{}' not found", display),
                line: Some(doc.node_line(node)),
                column: Some(doc.node_column(node)),
            });
            return;
        }

        // Reject abstract elements: they cannot appear directly in instances
        if decl.is_abstract {
            errors.push(ValidationError {
                message: format!(
                    "Element '{}' is abstract and cannot appear in an instance document",
                    decl.name
                ),
                line: Some(doc.node_line(node)),
                column: Some(doc.node_column(node)),
            });
            return;
        }

        // xsi:nil (cvc-elt.3), found by namespace URI. A declaration that is
        // not nillable admits no xsi:nil at all, whatever its value (3.1);
        // the value is a boolean. A nilled element has no content (3.2.1)
        // and its declaration no fixed value (3.2.2); its attributes are
        // still assessed, its content is not.
        let mut nilled = false;
        let nil_value = doc
            .element(node)
            .and_then(|elem| elem.get_attribute_ns(XSI_NAMESPACE, "nil"));
        if let Some(nil_value) = nil_value {
            if !decl.nillable {
                errors.push(ValidationError {
                    message: format!(
                        "Attribute xsi:nil is not allowed on element '{}', which is not nillable",
                        decl.name
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
                return;
            }
            match trim_xml_whitespace(nil_value) {
                "true" | "1" => nilled = true,
                "false" | "0" => {}
                _ => {
                    errors.push(ValidationError {
                        message: format!("xsi:nil value '{}' is not a valid boolean", nil_value),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                    return;
                }
            }
        }
        if nilled {
            let has_children = self.element_has_child_elements(doc, node);
            // Any character child counts, XML white space included.
            let has_text = !doc.text_content_deep(node).is_empty();
            if has_children || has_text {
                errors.push(ValidationError {
                    message: format!(
                        "Element '{}' has xsi:nil='true' and must have no content",
                        decl.name
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
            if let Some(fixed) = &decl.fixed {
                errors.push(ValidationError {
                    message: format!(
                        "Element '{}' has fixed value '{}' and cannot be nil",
                        decl.name, fixed
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }

        // An empty element with a fixed value takes that value (XSD 1.0
        // cvc-elt.5.1.2); the value is validated in place of the empty text.
        // A nilled element takes none.
        let has_child_elements = self.element_has_child_elements(doc, node);
        let is_empty = !has_child_elements && doc.text_content_deep(node).is_empty();
        let supplied = decl.fixed.as_deref().filter(|_| is_empty && !nilled);
        // The type the element is assessed against: a valid `xsi:type`
        // replaces the declared type (cvc-elt.4.3).
        let mut actual_type = decl.type_ref.clone();
        if decl.fixed.is_some() {
            let is_id = self
                .element_value_type(decl)
                .is_some_and(|value_type| self.is_id_type(&value_type));
            if is_id {
                // e-props-correct.4: an ID-typed element has no value constraint.
                errors.push(ValidationError {
                    message: format!(
                        "Element '{}' has type ID and cannot have a fixed value",
                        decl.name
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }

        // Check for xsi:type override. A valid substitution replaces the
        // declared type for the element's content (cvc-elt.4.3); the
        // declaration's fixed value and identity constraints still apply
        // after it (cvc-elt.5, cvc-elt.6).
        if let Some(xsi_type_ref) = self.resolve_xsi_type(doc, node) {
            match xsi_type_ref {
                XsiTypeResult::BuiltIn(bt) => {
                    // NOTATION cannot be used as the {type definition} of an element
                    if bt == BuiltInType::NOTATION {
                        errors.push(ValidationError {
                            message: "xs:NOTATION cannot be used as the type of an element"
                                .to_string(),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                        return;
                    }
                    if let Some(message) = self.builtin_xsi_type_error(&bt, decl) {
                        errors.push(ValidationError {
                            message,
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                        return;
                    }
                    if bt == BuiltInType::AnyType {
                        self.validate_children_against_global_decls(doc, node, errors);
                    } else {
                        self.check_simple_type_attributes(doc, node, errors);
                        // Simple built-in type: element must not have child elements
                        if self.element_has_child_elements(doc, node) {
                            errors.push(ValidationError {
                                message: format!(
                                    "Element with xsi:type '{}' must not have child elements",
                                    self.builtin_type_name(&bt)
                                ),
                                line: Some(doc.node_line(node)),
                                column: Some(doc.node_column(node)),
                            });
                            return;
                        }
                        if !nilled {
                            let text = match supplied {
                                Some(text) => text.to_string(),
                                None => doc.text_content_deep(node),
                            };
                            validate_builtin_value(&text, &bt, doc, node, errors, self.lenient);
                        }
                    }
                    actual_type = TypeRef::BuiltIn(bt);
                }
                XsiTypeResult::Named(xsi_key, td) => {
                    // Check that xsi:type is the declared type or derived from it
                    if !self.is_type_derived_from_decl(&xsi_key, &decl.type_ref) {
                        let type_name = match td.as_ref() {
                            TypeDef::Complex(ct) => ct.name.as_deref().unwrap_or("anonymous"),
                            TypeDef::Simple(st) => st.name.as_deref().unwrap_or("anonymous"),
                        };
                        errors.push(ValidationError {
                            message: format!(
                                "xsi:type '{}' is not derived from the declared element type",
                                type_name,
                            ),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                        return;
                    }
                    // Check block constraints
                    // Get the declared type's block constraints
                    let (decl_type_block_ext, decl_type_block_rst) = match &decl.type_ref {
                        TypeRef::Named(ns, name) => {
                            let key = (ns.clone(), name.clone());
                            if let Some(TypeDef::Complex(ct)) = self.types.get(&key) {
                                (ct.block_extension, ct.block_restriction)
                            } else {
                                (false, false)
                            }
                        }
                        _ => (false, false),
                    };
                    if let Some(block_msg) = self.check_type_substitution_blocked(
                        &td,
                        decl.block_extension,
                        decl.block_restriction,
                        decl_type_block_ext,
                        decl_type_block_rst,
                    ) {
                        errors.push(ValidationError {
                            message: block_msg,
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                        return;
                    }
                    match *td {
                        TypeDef::Complex(ct) if nilled => {
                            self.validate_attributes(doc, node, &ct, errors);
                        }
                        TypeDef::Complex(ct) => {
                            self.validate_complex_content(doc, node, &ct, supplied, errors);
                        }
                        TypeDef::Simple(st) => {
                            self.check_simple_type_attributes(doc, node, errors);
                            // Simple type: element must not have child elements
                            if self.element_has_child_elements(doc, node) {
                                errors.push(ValidationError {
                                    message:
                                        "Element with simple xsi:type must not have child elements"
                                            .to_string(),
                                    line: Some(doc.node_line(node)),
                                    column: Some(doc.node_column(node)),
                                });
                                return;
                            }
                            if !nilled {
                                self.validate_simple_content(doc, node, &st, supplied, errors);
                            }
                        }
                    }
                    actual_type = TypeRef::Named(xsi_key.0, xsi_key.1);
                }
                XsiTypeResult::NotFound(type_name) => {
                    errors.push(ValidationError {
                        message: format!("xsi:type '{}' not found", type_name),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                    return;
                }
            }
        } else {
            let type_def = self.resolve_type(&decl.type_ref);

            match type_def {
                Some(TypeDef::Complex(ct)) if nilled => {
                    self.validate_attributes(doc, node, ct, errors);
                }
                Some(TypeDef::Complex(ct)) => {
                    self.validate_complex_content(doc, node, ct, supplied, errors);
                }
                Some(TypeDef::Simple(st)) => {
                    self.check_simple_type_attributes(doc, node, errors);
                    // Simple types cannot have child elements
                    if self.element_has_child_elements(doc, node) {
                        let elem_name = doc
                            .element(node)
                            .map(|e| &*e.name.local_name)
                            .unwrap_or("?");
                        errors.push(ValidationError {
                            message: format!(
                                "Element '{}' has simple type but contains child elements",
                                elem_name
                            ),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                    if !nilled {
                        self.validate_simple_content(doc, node, st, supplied, errors);
                    }
                }
                None => {
                    // If type can't be resolved, check if it's a built-in
                    match &decl.type_ref {
                        TypeRef::BuiltIn(bt) => match bt {
                            BuiltInType::AnyType => {
                                // AnyType allows any content, but we should still
                                // validate child elements against their own declarations.
                                self.validate_children_against_global_decls(doc, node, errors);
                            }
                            _ => {
                                self.check_simple_type_attributes(doc, node, errors);
                                // Built-in simple types cannot have child elements
                                if self.element_has_child_elements(doc, node) {
                                    let elem_name = doc
                                        .element(node)
                                        .map(|e| &*e.name.local_name)
                                        .unwrap_or("?");
                                    errors.push(ValidationError {
                                    message: format!(
                                        "Element '{}' has simple type '{:?}' but contains child elements",
                                        elem_name, bt
                                    ),
                                    line: Some(doc.node_line(node)),
                                    column: Some(doc.node_column(node)),
                                });
                                }
                                if !nilled {
                                    let text = match supplied {
                                        Some(text) => text.to_string(),
                                        None => doc.text_content_deep(node),
                                    };
                                    validate_builtin_value(
                                        &text,
                                        bt,
                                        doc,
                                        node,
                                        errors,
                                        self.lenient,
                                    );
                                }
                            }
                        },
                        TypeRef::Named(ns, name) => {
                            let display = ns
                                .as_ref()
                                .map(|uri| format!("{{{}}}{}", uri, name))
                                .unwrap_or_else(|| name.clone());
                            errors.push(ValidationError {
                                message: format!("Type '{}' not found", display),
                                line: Some(doc.node_line(node)),
                                column: Some(doc.node_column(node)),
                            });
                        }
                        // Decided at build; never present in a built validator.
                        TypeRef::Unqualified(name) => errors.push(ValidationError {
                            message: format!("Type '{}' not found", name.local),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        }),
                        TypeRef::Inline(_) => {}
                    }
                }
            }
        }

        // Check the fixed-value constraint of a non-empty element
        // (cvc-elt.5.2.2). The element must have no element children
        // (5.2.2.1), whatever its content type, mixed included; then its
        // text, read in the actual type (the `xsi:type` type when one
        // applies), is compared by value with the fixed value, read in the
        // declared type, or character for character for mixed content
        // (5.2.2.2); see `element_matches_fixed_value`.
        if let Some(ref fixed_value) = decl.fixed.as_ref().filter(|_| has_child_elements) {
            errors.push(ValidationError {
                message: format!(
                    "Element '{}' has fixed value '{}' but contains child elements",
                    decl.name, fixed_value
                ),
                line: Some(doc.node_line(node)),
                column: Some(doc.node_column(node)),
            });
        } else if let Some(ref fixed_value) = decl.fixed.as_ref().filter(|_| !is_empty) {
            let text = doc.text_content_deep(node);
            let matches = self.element_matches_fixed_value(
                &text,
                fixed_value,
                &decl.type_ref,
                &actual_type,
                doc,
                node,
            );
            if !matches {
                errors.push(ValidationError {
                    message: format!(
                        "Element '{}' has fixed value '{}' but content is '{}'",
                        decl.name, fixed_value, text
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }

        // Evaluate identity constraints declared on this element
        if !decl.identity_constraints.is_empty() {
            self.evaluate_identity_constraints(doc, node, &decl.identity_constraints, errors);
        }
    }

    /// Resolve a type reference to a `TypeDef`.
    ///
    /// - `TypeRef::Named` → look up in `self.types` by (namespace, name) key
    /// - `TypeRef::Inline` → return the inline type definition directly
    /// - `TypeRef::BuiltIn` → return `None` (built-in types are handled separately)
    fn resolve_type<'a>(&'a self, type_ref: &'a TypeRef) -> Option<&'a TypeDef> {
        match type_ref {
            TypeRef::Named(ns, name) => {
                let key = (ns.clone(), name.clone());
                self.types.get(&key)
            }
            TypeRef::Inline(td) => Some(td.as_ref()),
            TypeRef::BuiltIn(_) | TypeRef::Unqualified(_) => None,
        }
    }

    /// Recursively validate child elements of an AnyType element against
    /// their global element declarations.
    ///
    /// For elements typed as `xs:anyType`, any content is allowed, but child
    /// elements that have matching global declarations are still validated
    /// against those declarations.
    fn validate_children_against_global_decls(
        &self,
        doc: &Document,
        node: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        for child in doc.children(node) {
            if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
                // Look up child element in global declarations
                let key_with_ns = (
                    child_elem
                        .name
                        .namespace_uri
                        .as_deref()
                        .map(|s| s.to_string()),
                    child_elem.name.local_name.to_string(),
                );
                let key_no_ns = (None, child_elem.name.local_name.to_string());

                let child_decl = self.elements.get(&key_with_ns).or_else(|| {
                    if child_elem.name.namespace_uri.is_none() {
                        self.elements.get(&key_no_ns)
                    } else {
                        None
                    }
                });

                if let Some(decl) = child_decl {
                    self.validate_element(doc, child, decl, errors);
                } else {
                    // No declaration found — for AnyType, that's OK.
                    // Still recurse to validate deeper children.
                    self.validate_children_against_global_decls(doc, child, errors);
                }
            }
        }
    }

    /// Validate the attributes of an element against a complex type definition:
    /// required and declared attributes, the attribute wildcard, and the
    /// refusal of any other attribute. The four XSI attributes are exempt
    /// (cvc-complex-type.3); every other attribute in the XSI namespace is
    /// assessed like any other. Namespace declarations are not attributes;
    /// an attribute with a declaration's name is refused by name.
    fn validate_attributes(
        &self,
        doc: &Document,
        node: NodeId,
        ct: &ComplexTypeDef,
        errors: &mut Vec<ValidationError>,
    ) {
        if self.has_complex_derivation_cycle(ct) {
            errors.push(ValidationError {
                message: "Complex type derivation cycle detected".to_string(),
                line: Some(doc.node_line(node)),
                column: Some(doc.node_column(node)),
            });
            return;
        }

        // Compute effective attributes (including inherited from base types)
        let effective_attrs = self.compute_effective_attributes(ct);

        // Validate attributes
        if let Some(elem) = doc.element(node) {
            for attr_decl in &effective_attrs {
                if attr_decl.required {
                    let found = elem
                        .attributes
                        .iter()
                        .any(|a| attr_decl_matches(a, attr_decl));
                    if !found {
                        let display = attr_decl
                            .namespace
                            .as_ref()
                            .map(|uri| format!("{{{}}}{}", uri, attr_decl.name))
                            .unwrap_or_else(|| attr_decl.name.clone());
                        errors.push(ValidationError {
                            message: format!("Required attribute '{}' is missing", display),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }

            // Validate attribute values against their declared types
            for attr_decl in &effective_attrs {
                debug_log!(
                    "effective_attr name={} type_ref={:?}",
                    attr_decl.name,
                    attr_decl.type_ref
                );
                if let Some(attr) = elem
                    .attributes
                    .iter()
                    .find(|a| attr_decl_matches(a, attr_decl))
                {
                    let value = &attr.value;
                    let (type_ref, fixed) = self.effective_attribute_use(attr_decl);
                    debug_log!(
                        "validating attr {}={} against {:?}",
                        attr_decl.name,
                        value,
                        type_ref
                    );
                    let start = errors.len();
                    self.validate_attribute_value(value, type_ref, doc, node, errors);
                    if errors.len() == start {
                        self.check_attribute_fixed_value(
                            &attr_decl.name,
                            value,
                            type_ref,
                            fixed,
                            doc,
                            node,
                            errors,
                        );
                    }
                }
            }

            // Compute the effective wildcard: for types derived by extension,
            // merge the base type's wildcard with the derived type's wildcard (union).
            let effective_wildcard = self.compute_effective_wildcard(ct);

            // Validate unmatched attributes against wildcard or reject if no wildcard
            if let Some(ref wildcard) = effective_wildcard {
                for attr in &elem.attributes {
                    if has_namespace_declaration_name(attr) {
                        errors.push(namespace_declaration_name_error(
                            attr,
                            &elem.name.local_name,
                            doc,
                            node,
                        ));
                        continue;
                    }
                    if is_exempt_xsi_attribute(attr) {
                        continue;
                    }
                    // Skip if already matched by an explicit attribute declaration
                    let already_declared =
                        effective_attrs.iter().any(|ad| attr_decl_matches(attr, ad));
                    if already_declared {
                        continue;
                    }

                    let attr_ns_str = attr.name.namespace_uri.as_deref().map(|s| s.to_string());

                    // Check namespace constraint
                    if !wildcard.allows_namespace(attr_ns_str.as_deref()) {
                        errors.push(ValidationError {
                            message: format!(
                                "Attribute '{}' in namespace '{}' is not allowed by wildcard constraint",
                                attr.name.local_name,
                                attr_ns_str.as_deref().unwrap_or("(no namespace)")
                            ),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                        continue;
                    }

                    // processContents validation
                    match wildcard.process_contents {
                        ProcessContents::Skip => {
                            // No validation needed
                        }
                        ProcessContents::Lax | ProcessContents::Strict => {
                            // Look up in global attribute declarations
                            let key = (attr_ns_str.clone(), attr.name.local_name.to_string());
                            let global_decl = self.global_attributes.get(&key);
                            match global_decl {
                                Some(decl) => {
                                    // Validate attribute value against its declared type
                                    let start = errors.len();
                                    self.validate_attribute_value(
                                        &attr.value,
                                        &decl.type_ref,
                                        doc,
                                        node,
                                        errors,
                                    );
                                    if errors.len() == start {
                                        self.check_attribute_fixed_value(
                                            &decl.name,
                                            &attr.value,
                                            &decl.type_ref,
                                            decl.fixed.as_deref(),
                                            doc,
                                            node,
                                            errors,
                                        );
                                    }
                                }
                                None => {
                                    // For strict: must find a declaration
                                    if wildcard.process_contents == ProcessContents::Strict {
                                        errors.push(ValidationError {
                                            message: format!(
                                                "Attribute '{}' in namespace '{}' has no global declaration (strict processContents)",
                                                attr.name.local_name,
                                                attr_ns_str.as_deref().unwrap_or("(no namespace)")
                                            ),
                                            line: Some(doc.node_line(node)),
                                            column: Some(doc.node_column(node)),
                                        });
                                    }
                                    // For lax: no declaration is OK
                                }
                            }
                        }
                    }
                }
            } else {
                // No wildcard: reject any undeclared attributes
                for attr in &elem.attributes {
                    if has_namespace_declaration_name(attr) {
                        errors.push(namespace_declaration_name_error(
                            attr,
                            &elem.name.local_name,
                            doc,
                            node,
                        ));
                        continue;
                    }
                    if is_exempt_xsi_attribute(attr) {
                        continue;
                    }
                    // Check if declared
                    let already_declared =
                        effective_attrs.iter().any(|ad| attr_decl_matches(attr, ad));
                    if !already_declared {
                        errors.push(ValidationError {
                            message: format!(
                                "Attribute '{}' is not allowed (no wildcard permits additional attributes)",
                                attribute_display(attr),
                            ),
                            line: Some(doc.node_line(node)),
                            column: Some(doc.node_column(node)),
                        });
                    }
                }
            }
        }
    }

    /// Validate the complex content of an element against a complex type definition.
    ///
    /// This handles:
    /// - Required/optional attribute checking
    /// - Attribute value validation against declared types
    /// - Attribute wildcard processing (processContents=skip/lax/strict)
    /// - Rejecting undeclared attributes when no wildcard is present
    /// - Element-only text content rejection (non-mixed types)
    /// - Content model validation (sequence/choice/all/empty/simpleContent/any)
    /// - Extension type particle merging
    fn validate_complex_content(
        &self,
        doc: &Document,
        node: NodeId,
        ct: &ComplexTypeDef,
        text: Option<&str>,
        errors: &mut Vec<ValidationError>,
    ) {
        if self.has_complex_derivation_cycle(ct) {
            errors.push(ValidationError {
                message: "Complex type derivation cycle detected".to_string(),
                line: Some(doc.node_line(node)),
                column: Some(doc.node_column(node)),
            });
            return;
        }

        self.validate_attributes(doc, node, ct, errors);

        // Validate content model
        let child_elements: Vec<NodeId> = doc
            .children(node)
            .into_iter()
            .filter(|&c| matches!(doc.node_kind(c), Some(NodeKind::Element(_))))
            .collect();

        let (model, mixed) = match self.content_type(ct) {
            ContentType::Model(model, mixed) => (model, mixed),
            ContentType::Simple(owner) => {
                self.validate_simple_content_of_complex(
                    doc,
                    node,
                    owner,
                    &child_elements,
                    text,
                    errors,
                );
                return;
            }
        };

        // An empty content type admits no character children, white space
        // included (cvc-complex-type.2.1); element-only content admits white
        // space only (cvc-complex-type.2.3). A sequence or choice without
        // particles keeps the element-only check: it is also what remains of
        // a model whose group references were not resolved when read, whose
        // content is not empty.
        if !mixed && matches!(model.as_ref(), ContentModel::Empty) {
            let has_character_child = doc
                .children(node)
                .into_iter()
                .any(|child| doc.text_content(child).is_some_and(|text| !text.is_empty()));
            if has_character_child {
                let name = doc
                    .element(node)
                    .map(|elem| elem.name.local_name.to_string())
                    .unwrap_or_default();
                errors.push(ValidationError {
                    message: format!(
                        "Element '{}' has an empty content type and must not contain \
                         character content, white space included",
                        name
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        } else if !mixed {
            let is_element_only = matches!(
                model.as_ref(),
                ContentModel::Sequence(..) | ContentModel::Choice(..) | ContentModel::All(..)
            );
            if is_element_only {
                for child in doc.children(node) {
                    if let Some(text) = doc.text_content(child) {
                        if !trim_xml_whitespace(text).is_empty() {
                            errors.push(ValidationError {
                                message:
                                    "Non-whitespace text content is not allowed in element-only content"
                                        .to_string(),
                                line: Some(doc.node_line(child)),
                                column: Some(doc.node_column(child)),
                            });
                            break; // report once
                        }
                    }
                }
            }
        }

        match model.as_ref() {
            ContentModel::Empty => {
                if !child_elements.is_empty() {
                    errors.push(ValidationError {
                        message: "Element should have empty content".to_string(),
                        line: Some(doc.node_line(node)),
                        column: Some(doc.node_column(node)),
                    });
                }
                // Character children are refused above.
            }
            ContentModel::Sequence(particles, min_occurs, max_occurs) => {
                let consumed = self.validate_sequence(
                    doc,
                    &child_elements,
                    particles,
                    *min_occurs,
                    max_occurs,
                    node,
                    errors,
                );
                // Report remaining children as unexpected
                for &remaining in &child_elements[consumed..] {
                    if let Some(elem) = doc.element(remaining) {
                        errors.push(ValidationError {
                            message: format!(
                                "Unexpected element '{}' in sequence",
                                elem.name.local_name
                            ),
                            line: Some(doc.node_line(remaining)),
                            column: Some(doc.node_column(remaining)),
                        });
                    }
                }
            }
            ContentModel::Choice(particles, min_occurs, max_occurs) => {
                let consumed = self.validate_choice(
                    doc,
                    &child_elements,
                    particles,
                    *min_occurs,
                    max_occurs,
                    node,
                    errors,
                );
                // Report remaining children as unexpected
                for &remaining in &child_elements[consumed..] {
                    if let Some(elem) = doc.element(remaining) {
                        errors.push(ValidationError {
                            message: format!(
                                "Unexpected element '{}' after choice",
                                elem.name.local_name
                            ),
                            line: Some(doc.node_line(remaining)),
                            column: Some(doc.node_column(remaining)),
                        });
                    }
                }
            }
            ContentModel::All(particles) => {
                self.validate_all(doc, &child_elements, particles, node, errors);
            }
            // Decided by `content_type`: a model never holds simple content.
            ContentModel::SimpleContent(_) => {}
            ContentModel::Any => {
                // The lax wildcard content of xs:anyType.
                self.validate_children_against_global_decls(doc, node, errors);
            }
        }
    }

    /// Validate the content of an element whose complex type has simple
    /// content, the content type of `owner`: no element children
    /// (cvc-complex-type.2.2), and text that satisfies the base's simple
    /// content type and, for a `simpleContent` restriction, the
    /// restriction's facets.
    fn validate_simple_content_of_complex(
        &self,
        doc: &Document,
        node: NodeId,
        owner: &ComplexTypeDef,
        child_elements: &[NodeId],
        text: Option<&str>,
        errors: &mut Vec<ValidationError>,
    ) {
        if let Some(&child) = child_elements.first() {
            let name = doc
                .element(child)
                .map(|e| e.name.local_name.to_string())
                .unwrap_or_default();
            errors.push(ValidationError {
                message: format!(
                    "Element '{}' is not allowed: the type has simple content (cvc-complex-type.2.2)",
                    name
                ),
                line: Some(doc.node_line(child)),
                column: Some(doc.node_column(child)),
            });
            return;
        }
        match self.simple_content_types(owner) {
            Ok(types) => {
                let content_text = match text {
                    Some(text) => text.to_string(),
                    None => doc.text_content_deep(node),
                };
                let mut memo = UnionMemo::new();
                for type_ref in &types {
                    self.check_type_ref_value(
                        &content_text,
                        type_ref,
                        doc,
                        node,
                        errors,
                        0,
                        &mut memo,
                    );
                }
            }
            Err(message) => errors.push(ValidationError {
                message,
                line: Some(doc.node_line(node)),
                column: Some(doc.node_column(node)),
            }),
        }
    }

    /// Find a global element declaration by local name and namespace.
    fn find_global_element(&self, name: &str, ns: Option<&str>) -> Option<ElementDecl> {
        let key = (ns.map(|s| s.to_string()), name.to_string());
        self.elements.get(&key).cloned()
    }

    /// Check if an instance element matches a declared element or is a member of
    /// its substitution group.
    ///
    /// Returns `Some(global_decl)` if the element is a substitution group member
    /// that should be validated against its own declaration.
    /// Returns `None` if it's a direct match (caller handles validation) or no match.
    fn element_matches_with_substitution(
        &self,
        elem_name: &str,
        elem_ns: Option<&str>,
        decl: &ElementDecl,
    ) -> Option<ElementDecl> {
        // Only a reference to a global declaration has a substitution group:
        // a local declaration with the same expanded name as a head is a
        // different declaration, which no element substitutes for.
        if !decl.is_ref {
            return None;
        }
        // Check if the instance element is a member of the substitution group
        // headed by decl.
        let head_key = (decl.namespace.clone(), decl.name.clone());
        if let Some(members) = self.substitution_groups.get(&head_key) {
            let elem_key = (elem_ns.map(|s| s.to_string()), elem_name.to_string());
            if members.contains(&elem_key) {
                // Found: the instance element substitutes for the declared element.
                // Return the member's own global declaration for type validation.
                return self.find_global_element(elem_name, elem_ns);
            }
        }
        None
    }

    /// Validate a sequence content model.
    ///
    /// Iterates through the sequence's particles in order, matching child elements.
    /// The sequence can repeat between `compositor_min` and `compositor_max` times.
    /// Each particle within the sequence has its own min/max occurs constraints.
    ///
    /// Returns the number of child elements consumed.
    #[allow(clippy::too_many_arguments)]
    fn validate_sequence(
        &self,
        doc: &Document,
        children: &[NodeId],
        particles: &[Particle],
        compositor_min: u64,
        compositor_max: &MaxOccurs,
        parent: NodeId,
        errors: &mut Vec<ValidationError>,
    ) -> usize {
        let max_reps = match compositor_max {
            MaxOccurs::Bounded(n) => *n,
            MaxOccurs::Unbounded => u64::MAX,
        };

        let mut child_idx = 0;
        let mut seq_reps = 0u64;

        // Outer loop: repeat the entire sequence up to max_reps times
        'outer: while seq_reps < max_reps {
            let start_idx = child_idx;

            for particle in particles {
                let mut count = 0u64;
                let max = match particle.max_occurs {
                    MaxOccurs::Bounded(n) => n,
                    MaxOccurs::Unbounded => u64::MAX,
                };

                match &particle.kind {
                    ParticleKind::Element(decl) => {
                        while child_idx < children.len() && count < max {
                            let child = children[child_idx];
                            if let Some(elem) = doc.element(child) {
                                let name_matches = elem.name.local_name == decl.name;
                                let ns_matches = match (&elem.name.namespace_uri, &decl.namespace) {
                                    (Some(a), Some(b)) => a == b,
                                    (None, None) => true,
                                    (Some(_), None) => false,
                                    (None, Some(_)) => false,
                                };
                                if name_matches && ns_matches {
                                    self.validate_element(doc, child, decl, errors);
                                    count += 1;
                                    child_idx += 1;
                                } else if let Some(subst_decl) = self
                                    .element_matches_with_substitution(
                                        &elem.name.local_name,
                                        elem.name.namespace_uri.as_deref(),
                                        decl,
                                    )
                                {
                                    // Element is a substitution group member;
                                    // validate against its own declaration.
                                    self.validate_element(doc, child, &subst_decl, errors);
                                    count += 1;
                                    child_idx += 1;
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                        if count < particle.min_occurs {
                            if seq_reps >= compositor_min {
                                // We've already met the minimum repetitions,
                                // so this is just the sequence not starting another rep.
                                // Roll back to start_idx for this failed rep.
                                child_idx = start_idx;
                                break 'outer;
                            }
                            errors.push(ValidationError {
                                message: format!(
                                    "Expected at least {} occurrence(s) of element '{}', found {}",
                                    particle.min_occurs, decl.name, count
                                ),
                                line: Some(doc.node_line(parent)),
                                column: Some(doc.node_column(parent)),
                            });
                            break 'outer;
                        }
                    }
                    ParticleKind::Sequence(sub_particles) => {
                        let sub_min = particle.min_occurs;
                        let sub_max = &particle.max_occurs;
                        let before = errors.len();
                        let consumed = self.validate_sequence(
                            doc,
                            &children[child_idx..],
                            sub_particles,
                            sub_min,
                            sub_max,
                            parent,
                            errors,
                        );
                        child_idx += consumed;
                        if errors.len() > before {
                            break 'outer;
                        }
                    }
                    ParticleKind::Choice(sub_particles) => {
                        let sub_min = particle.min_occurs;
                        let sub_max = &particle.max_occurs;
                        let consumed = self.validate_choice(
                            doc,
                            &children[child_idx..],
                            sub_particles,
                            sub_min,
                            sub_max,
                            parent,
                            errors,
                        );
                        child_idx += consumed;
                    }
                    ParticleKind::Any {
                        namespace_constraint,
                        process_contents,
                    } => {
                        // xs:any element wildcard: consume matching children
                        while child_idx < children.len() && count < max {
                            let child = children[child_idx];
                            if let Some(elem) = doc.element(child) {
                                if wildcard_allows_namespace(
                                    namespace_constraint,
                                    elem.name.namespace_uri.as_deref(),
                                ) {
                                    // Namespace matches; apply processContents
                                    match process_contents {
                                        ProcessContents::Skip => {
                                            // Accept unconditionally
                                        }
                                        ProcessContents::Lax => {
                                            // Try to find global element declaration; validate if found
                                            if let Some(global_decl) = self.find_global_element(
                                                &elem.name.local_name,
                                                elem.name.namespace_uri.as_deref(),
                                            ) {
                                                self.validate_element(
                                                    doc,
                                                    child,
                                                    &global_decl,
                                                    errors,
                                                );
                                            }
                                        }
                                        ProcessContents::Strict => {
                                            // Must find global element declaration
                                            if let Some(global_decl) = self.find_global_element(
                                                &elem.name.local_name,
                                                elem.name.namespace_uri.as_deref(),
                                            ) {
                                                self.validate_element(
                                                    doc,
                                                    child,
                                                    &global_decl,
                                                    errors,
                                                );
                                            } else {
                                                errors.push(ValidationError {
                                                    message: format!(
                                                        "No global element declaration found for '{}' (strict wildcard)",
                                                        elem.name.local_name
                                                    ),
                                                    line: Some(doc.node_line(child)),
                                                    column: Some(doc.node_column(child)),
                                                });
                                            }
                                        }
                                    }
                                    count += 1;
                                    child_idx += 1;
                                } else {
                                    break; // namespace doesn't match
                                }
                            } else {
                                break;
                            }
                        }
                        if count < particle.min_occurs {
                            if seq_reps >= compositor_min {
                                child_idx = start_idx;
                                break 'outer;
                            }
                            errors.push(ValidationError {
                                message: format!(
                                    "Expected at least {} element(s) matching wildcard, found {}",
                                    particle.min_occurs, count
                                ),
                                line: Some(doc.node_line(parent)),
                                column: Some(doc.node_column(parent)),
                            });
                            break 'outer;
                        }
                    }
                }
            }

            seq_reps += 1;

            // If no progress was made, stop looping
            if child_idx == start_idx {
                break;
            }

            // If all children consumed, stop
            if child_idx >= children.len() {
                break;
            }
        }

        if seq_reps < compositor_min {
            errors.push(ValidationError {
                message: format!(
                    "Sequence must occur at least {} time(s), found {}",
                    compositor_min, seq_reps
                ),
                line: Some(doc.node_line(parent)),
                column: Some(doc.node_column(parent)),
            });
        }

        child_idx
    }

    /// Validate a choice content model.
    ///
    /// Tries to match the current child element against one of the choice alternatives.
    /// The choice can repeat between `compositor_min` and `compositor_max` times.
    /// Each repetition picks one matching alternative and consumes its elements.
    ///
    /// Returns the number of child elements consumed.
    #[allow(clippy::too_many_arguments)]
    fn validate_choice(
        &self,
        doc: &Document,
        children: &[NodeId],
        particles: &[Particle],
        compositor_min: u64,
        compositor_max: &MaxOccurs,
        parent: NodeId,
        errors: &mut Vec<ValidationError>,
    ) -> usize {
        let max_reps = match compositor_max {
            MaxOccurs::Bounded(n) => *n,
            MaxOccurs::Unbounded => u64::MAX,
        };

        if children.is_empty() {
            if compositor_min > 0 {
                // The choice is satisfied with no content when any alternative is
                // nullable (optional, or nullable through its own content).
                let nullable = particles.iter().any(Self::particle_is_nullable);
                if !nullable && !particles.is_empty() {
                    errors.push(ValidationError {
                        message: "Expected one of the choice alternatives".to_string(),
                        line: Some(doc.node_line(parent)),
                        column: Some(doc.node_column(parent)),
                    });
                }
            }
            return 0;
        }

        let mut child_idx = 0;
        let mut choice_reps = 0u64;

        // Outer loop: repeat the choice up to max_reps times.
        // Each iteration picks one alternative and consumes matching children for it.
        while choice_reps < max_reps && child_idx < children.len() {
            let current_child = children[child_idx];
            let elem = match doc.element(current_child) {
                Some(e) => e,
                None => break,
            };

            // Try to match the current child against one of the choice alternatives
            let mut matched_any = false;

            for p in particles {
                match &p.kind {
                    ParticleKind::Element(decl) => {
                        let name_matches = decl.name == elem.name.local_name;
                        let ns_matches = match (&elem.name.namespace_uri, &decl.namespace) {
                            (Some(a), Some(b)) => a == b,
                            (None, None) => true,
                            (Some(_), None) => false,
                            (None, Some(_)) => false,
                        };
                        // Check for direct match or substitution group match
                        let subst_decl = if !(name_matches && ns_matches) {
                            self.element_matches_with_substitution(
                                &elem.name.local_name,
                                elem.name.namespace_uri.as_deref(),
                                decl,
                            )
                        } else {
                            None
                        };
                        if (name_matches && ns_matches) || subst_decl.is_some() {
                            // Consume as many consecutive elements as allowed by max_occurs
                            // (matching either the declared element or substitution group members)
                            let max = match p.max_occurs {
                                MaxOccurs::Bounded(n) => n as usize,
                                MaxOccurs::Unbounded => usize::MAX,
                            };
                            let mut count = 0usize;
                            while child_idx < children.len() && count < max {
                                let child = children[child_idx];
                                if let Some(child_elem) = doc.element(child) {
                                    let cn_matches = child_elem.name.local_name == decl.name;
                                    let cns_matches =
                                        match (&child_elem.name.namespace_uri, &decl.namespace) {
                                            (Some(a), Some(b)) => a == b,
                                            (None, None) => true,
                                            _ => false,
                                        };
                                    if cn_matches && cns_matches {
                                        self.validate_element(doc, child, decl, errors);
                                        child_idx += 1;
                                        count += 1;
                                    } else if let Some(child_subst) = self
                                        .element_matches_with_substitution(
                                            &child_elem.name.local_name,
                                            child_elem.name.namespace_uri.as_deref(),
                                            decl,
                                        )
                                    {
                                        self.validate_element(doc, child, &child_subst, errors);
                                        child_idx += 1;
                                        count += 1;
                                    } else {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }
                            if count < p.min_occurs as usize {
                                errors.push(ValidationError {
                                    message: format!(
                                        "Expected at least {} occurrence(s) of element '{}' in choice, found {}",
                                        p.min_occurs, decl.name, count
                                    ),
                                    line: Some(doc.node_line(current_child)),
                                    column: Some(doc.node_column(current_child)),
                                });
                            }
                            matched_any = true;
                            break;
                        }
                    }
                    ParticleKind::Sequence(sub_particles) => {
                        let sub_min = p.min_occurs;
                        let sub_max = &p.max_occurs;
                        let before_errors = errors.len();
                        let consumed = self.validate_sequence(
                            doc,
                            &children[child_idx..],
                            sub_particles,
                            sub_min,
                            sub_max,
                            parent,
                            errors,
                        );
                        if consumed > 0 {
                            child_idx += consumed;
                            matched_any = true;
                            // If the nested sequence produced errors, we still consumed
                            // elements — but we should stop the choice loop
                            if errors.len() > before_errors {
                                choice_reps += 1;
                                break;
                            }
                            break;
                        } else if errors.len() > before_errors {
                            // Sequence matched nothing but produced errors — try next alternative
                            // Roll back errors from this failed attempt
                            errors.truncate(before_errors);
                        }
                        // If consumed == 0 and no errors, this alternative didn't match; try next
                    }
                    ParticleKind::Choice(sub_particles) => {
                        let sub_min = p.min_occurs;
                        let sub_max = &p.max_occurs;
                        let before_errors = errors.len();
                        let consumed = self.validate_choice(
                            doc,
                            &children[child_idx..],
                            sub_particles,
                            sub_min,
                            sub_max,
                            parent,
                            errors,
                        );
                        if consumed > 0 {
                            child_idx += consumed;
                            matched_any = true;
                            break;
                        } else if errors.len() > before_errors {
                            // Sub-choice matched nothing but produced errors — try next alternative
                            errors.truncate(before_errors);
                        }
                    }
                    ParticleKind::Any {
                        namespace_constraint,
                        process_contents,
                    } => {
                        if wildcard_allows_namespace(
                            namespace_constraint,
                            elem.name.namespace_uri.as_deref(),
                        ) {
                            // Consume as many wildcard-matching elements as allowed
                            let max = match p.max_occurs {
                                MaxOccurs::Bounded(n) => n as usize,
                                MaxOccurs::Unbounded => usize::MAX,
                            };
                            let mut count = 0usize;
                            while child_idx < children.len() && count < max {
                                let child = children[child_idx];
                                if let Some(child_elem) = doc.element(child) {
                                    if wildcard_allows_namespace(
                                        namespace_constraint,
                                        child_elem.name.namespace_uri.as_deref(),
                                    ) {
                                        match process_contents {
                                            ProcessContents::Skip => {}
                                            ProcessContents::Lax => {
                                                if let Some(global_decl) = self.find_global_element(
                                                    &child_elem.name.local_name,
                                                    child_elem.name.namespace_uri.as_deref(),
                                                ) {
                                                    self.validate_element(
                                                        doc,
                                                        child,
                                                        &global_decl,
                                                        errors,
                                                    );
                                                }
                                            }
                                            ProcessContents::Strict => {
                                                if let Some(global_decl) = self.find_global_element(
                                                    &child_elem.name.local_name,
                                                    child_elem.name.namespace_uri.as_deref(),
                                                ) {
                                                    self.validate_element(
                                                        doc,
                                                        child,
                                                        &global_decl,
                                                        errors,
                                                    );
                                                } else {
                                                    errors.push(ValidationError {
                                                        message: format!(
                                                            "No global element declaration found for '{}' (strict wildcard)",
                                                            child_elem.name.local_name
                                                        ),
                                                        line: Some(doc.node_line(child)),
                                                        column: Some(doc.node_column(child)),
                                                    });
                                                }
                                            }
                                        }
                                        child_idx += 1;
                                        count += 1;
                                    } else {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }
                            matched_any = count > 0;
                            if matched_any {
                                break;
                            }
                        }
                    }
                }
            }

            if !matched_any {
                // Current child doesn't match any alternative. A choice is
                // *nullable* when at least one alternative can match the empty
                // sequence — either an optional particle (minOccurs=0) or one
                // whose content is itself nullable (e.g. a sequence of all
                // optional particles). Selecting that alternative and matching it
                // zero times satisfies the choice, so a non-matching child is not
                // an error: the choice consumes nothing and an enclosing sequence
                // continues with its next particle (e.g. KML's optional
                // `Snippet|snippet` choice preceding the feature list).
                let nullable = particles.iter().any(Self::particle_is_nullable);
                if choice_reps < compositor_min && !nullable {
                    errors.push(ValidationError {
                        message: format!(
                            "Element '{}' does not match any choice alternative",
                            elem.name.local_name
                        ),
                        line: Some(doc.node_line(current_child)),
                        column: Some(doc.node_column(current_child)),
                    });
                }
                break;
            }

            choice_reps += 1;
        }

        if choice_reps < compositor_min && child_idx == 0 && errors.is_empty() {
            // No children matched and no error was emitted yet. The choice is
            // satisfied without consuming anything when any alternative is
            // nullable (see `particle_is_nullable`).
            let nullable = particles.iter().any(Self::particle_is_nullable);
            if !nullable && !particles.is_empty() {
                errors.push(ValidationError {
                    message: "Expected one of the choice alternatives".to_string(),
                    line: Some(doc.node_line(parent)),
                    column: Some(doc.node_column(parent)),
                });
            }
        }

        child_idx
    }

    /// Whether a particle can match the empty sequence (is *nullable*).
    ///
    /// A particle with `minOccurs=0` is always nullable. Otherwise nullability
    /// depends on the kind:
    /// - element / wildcard with `minOccurs>=1`: not nullable (must match one),
    /// - sequence: nullable iff every sub-particle is nullable (an empty
    ///   sequence is vacuously nullable),
    /// - choice: nullable iff any sub-particle is nullable.
    fn particle_is_nullable(particle: &Particle) -> bool {
        if particle.min_occurs == 0 {
            return true;
        }
        match &particle.kind {
            ParticleKind::Element(_) | ParticleKind::Any { .. } => false,
            ParticleKind::Sequence(subs) => subs.iter().all(Self::particle_is_nullable),
            ParticleKind::Choice(subs) => subs.iter().any(Self::particle_is_nullable),
        }
    }

    /// Whether a content model accepts no element children.
    fn model_is_emptiable(&self, model: &ContentModel) -> bool {
        match model {
            ContentModel::Empty | ContentModel::Any => true,
            ContentModel::Sequence(particles, min, _) => {
                *min == 0 || particles.iter().all(Self::particle_is_nullable)
            }
            ContentModel::Choice(particles, min, _) => {
                *min == 0 || particles.iter().any(Self::particle_is_nullable)
            }
            ContentModel::All(particles) => particles.iter().all(Self::particle_is_nullable),
            ContentModel::SimpleContent(_) => false,
        }
    }

    /// Validate an `xs:all` content model.
    ///
    /// In an all group, each particle can appear at most once, and order doesn't matter.
    /// Required particles (min_occurs > 0) must all be present.
    /// Supports both element particles and wildcard particles.
    fn validate_all(
        &self,
        doc: &Document,
        children: &[NodeId],
        particles: &[Particle],
        parent: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        let mut matched = vec![false; particles.len()];

        for &child in children {
            if let Some(elem) = doc.element(child) {
                let mut found = false;
                for (i, particle) in particles.iter().enumerate() {
                    match &particle.kind {
                        ParticleKind::Element(decl) => {
                            let name_matches = decl.name == elem.name.local_name;
                            let ns_matches = match (&elem.name.namespace_uri, &decl.namespace) {
                                (Some(a), Some(b)) => a == b,
                                (None, None) => true,
                                (Some(_), None) => false,
                                (None, Some(_)) => false,
                            };
                            let subst_decl = if !(name_matches && ns_matches) {
                                self.element_matches_with_substitution(
                                    &elem.name.local_name,
                                    elem.name.namespace_uri.as_deref(),
                                    decl,
                                )
                            } else {
                                None
                            };
                            if (name_matches && ns_matches) || subst_decl.is_some() {
                                if matched[i] {
                                    errors.push(ValidationError {
                                        message: format!(
                                            "Duplicate element '{}' in all group",
                                            elem.name.local_name
                                        ),
                                        line: Some(doc.node_line(child)),
                                        column: Some(doc.node_column(child)),
                                    });
                                } else {
                                    matched[i] = true;
                                    if let Some(ref sd) = subst_decl {
                                        self.validate_element(doc, child, sd, errors);
                                    } else {
                                        self.validate_element(doc, child, decl, errors);
                                    }
                                }
                                found = true;
                                break;
                            }
                        }
                        ParticleKind::Any {
                            namespace_constraint,
                            process_contents,
                        } if wildcard_allows_namespace(
                            namespace_constraint,
                            elem.name.namespace_uri.as_deref(),
                        ) =>
                        {
                            matched[i] = true;
                            match process_contents {
                                ProcessContents::Skip => {}
                                ProcessContents::Lax => {
                                    if let Some(global_decl) = self.find_global_element(
                                        &elem.name.local_name,
                                        elem.name.namespace_uri.as_deref(),
                                    ) {
                                        self.validate_element(doc, child, &global_decl, errors);
                                    }
                                }
                                ProcessContents::Strict => {
                                    if let Some(global_decl) = self.find_global_element(
                                        &elem.name.local_name,
                                        elem.name.namespace_uri.as_deref(),
                                    ) {
                                        self.validate_element(doc, child, &global_decl, errors);
                                    } else {
                                        errors.push(ValidationError {
                                            message: format!(
                                                "No global element declaration found for '{}' (strict wildcard)",
                                                elem.name.local_name
                                            ),
                                            line: Some(doc.node_line(child)),
                                            column: Some(doc.node_column(child)),
                                        });
                                    }
                                }
                            }
                            found = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !found {
                    errors.push(ValidationError {
                        message: format!(
                            "Unexpected element '{}' in all group",
                            elem.name.local_name
                        ),
                        line: Some(doc.node_line(child)),
                        column: Some(doc.node_column(child)),
                    });
                }
            }
        }

        // Check required elements
        for (i, particle) in particles.iter().enumerate() {
            if particle.min_occurs > 0 && !matched[i] {
                if let ParticleKind::Element(decl) = &particle.kind {
                    errors.push(ValidationError {
                        message: format!(
                            "Required element '{}' is missing in all group",
                            decl.name
                        ),
                        line: Some(doc.node_line(parent)),
                        column: Some(doc.node_column(parent)),
                    });
                }
            }
        }
    }

    /// Refuse, by name, every attribute of an element whose type is simple
    /// (declared, built-in or given by `xsi:type`), except the four XSI
    /// attributes (cvc-type.3.1.1). Namespace declarations are not
    /// attributes, so none is exempt here.
    fn check_simple_type_attributes(
        &self,
        doc: &Document,
        node: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        let Some(elem) = doc.element(node) else {
            return;
        };
        for attr in &elem.attributes {
            if has_namespace_declaration_name(attr) {
                errors.push(namespace_declaration_name_error(
                    attr,
                    &elem.name.local_name,
                    doc,
                    node,
                ));
                continue;
            }
            if is_exempt_xsi_attribute(attr) {
                continue;
            }
            errors.push(ValidationError {
                message: format!(
                    "Attribute '{}' is not allowed on element '{}', which has a simple type",
                    attribute_display(attr),
                    elem.name.local_name
                ),
                line: Some(doc.node_line(node)),
                column: Some(doc.node_column(node)),
            });
        }
    }

    /// Validate simple (text) content of an element against a simple type definition.
    ///
    /// `text` replaces the element's text when given (an empty element with a
    /// fixed value takes that value). See [`XsdValidator::validate_simple_value`]
    /// for the derivation-chain, list and union semantics.
    fn validate_simple_content(
        &self,
        doc: &Document,
        node: NodeId,
        st: &SimpleTypeDef,
        text: Option<&str>,
        errors: &mut Vec<ValidationError>,
    ) {
        match text {
            Some(text) => self.validate_simple_value(text, st, doc, node, errors),
            None => {
                let raw_text = doc.text_content_deep(node);
                self.validate_simple_value(&raw_text, st, doc, node, errors);
            }
        }
    }

    /// Validate an attribute value against its declared type reference
    /// (built-in, anonymous or named simple type).
    fn validate_attribute_value(
        &self,
        value: &str,
        type_ref: &TypeRef,
        doc: &Document,
        node: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        debug_log!("validate_attribute_value type_ref={:?}", type_ref);
        self.check_type_ref_value(value, type_ref, doc, node, errors, 0, &mut UnionMemo::new());
    }

    /// The type and fixed value that govern an attribute use. A use that
    /// references a global attribute declaration takes that declaration's
    /// type, and its fixed value unless the use sets its own.
    fn effective_attribute_use<'a>(
        &'a self,
        decl: &'a AttributeDecl,
    ) -> (&'a TypeRef, Option<&'a str>) {
        if decl.is_ref {
            let key = (decl.namespace.clone(), decl.name.clone());
            if let Some(global) = self.global_attributes.get(&key) {
                return (
                    &global.type_ref,
                    decl.fixed.as_deref().or(global.fixed.as_deref()),
                );
            }
        }
        (&decl.type_ref, decl.fixed.as_deref())
    }

    /// Whether `value` equals the fixed value `fixed` as values of `type_ref`:
    /// both are whitespace-normalized and read by the type (for a union, by
    /// the member that accepts each), then compared as values. An invalid
    /// value never matches.
    fn matches_fixed_value(
        &self,
        value: &str,
        fixed: &str,
        type_ref: &TypeRef,
        doc: &Document,
        node: NodeId,
    ) -> bool {
        let mut scratch = Vec::new();
        let mut memo = UnionMemo::new();
        let actual =
            self.check_type_ref_value(value, type_ref, doc, node, &mut scratch, 0, &mut memo);
        let expected =
            self.check_type_ref_value(fixed, type_ref, doc, node, &mut scratch, 0, &mut memo);
        match (actual, expected) {
            (Some((a, a_type)), Some((b, b_type))) => values_equal(&a, &a_type, &b, &b_type),
            _ => false,
        }
    }

    /// Whether a non-empty element's text `value` equals its declaration's
    /// fixed value `fixed` (cvc-elt.5.2.2.2).
    ///
    /// The two are read in different types. The fixed value is the
    /// declaration's value constraint, a value of the **declared** type
    /// (XSD 1.0 Part 1 §3.3.1, {value constraint}), so its literal is
    /// normalized and read in `declared`. The element's value is read in
    /// the **actual** type, `actual` (the `xsi:type` type when one applies,
    /// cvc-elt.5.2.2.2.2). A type validly derived from the declared type
    /// shares its primitive, so the two compare by value: under `xs:token`
    /// the text `1` is not the `xs:string` fixed value ` 1 `.
    ///
    /// When the declared type is `xs:anySimpleType`, the literal's value is
    /// indeterminate: it may map to values of several primitives, and which
    /// one is meant is not decided (XSD 1.1 Part 2 §3.2.1). An `xsi:type`
    /// chooses the reading of an element's content, not of a schema's
    /// literal, so only the lexical form is compared: the element's value,
    /// normalized by the actual type, must equal the literal character for
    /// character. When the declared type is not a
    /// simple type and has no simple content (`xs:anyType`, which includes
    /// a declaration without a type, or any mixed type), the fixed value is
    /// an `xs:string` (XSD 1.0 Part 1 §3.3.2, {value constraint}), so only
    /// an actual type derived from `xs:string` can match it. When the actual
    /// type has mixed content, text and literal are compared character for
    /// character (cvc-elt.5.2.2.2.1). An invalid text or literal never
    /// matches.
    fn element_matches_fixed_value(
        &self,
        value: &str,
        fixed: &str,
        declared: &TypeRef,
        actual: &TypeRef,
        doc: &Document,
        node: NodeId,
    ) -> bool {
        let Some(actual_value_type) = self.value_type_of(actual) else {
            return value == fixed;
        };
        let mut scratch = Vec::new();
        let mut memo = UnionMemo::new();
        let Some((item, item_type)) = self.check_type_ref_value(
            value,
            &actual_value_type,
            doc,
            node,
            &mut scratch,
            0,
            &mut memo,
        ) else {
            return false;
        };
        let read_literal_in = |type_ref: &TypeRef, memo: &mut UnionMemo| {
            self.check_type_ref_value(fixed, type_ref, doc, node, &mut Vec::new(), 0, memo)
        };
        let constraint = match self.value_type_of(declared) {
            Some(declared_value_type) => match read_literal_in(&declared_value_type, &mut memo) {
                Some((_, BuiltInType::AnySimpleType)) => return item == fixed,
                other => other,
            },
            None => Some((fixed.to_string(), BuiltInType::String)),
        };
        match constraint {
            Some((literal, literal_type)) => {
                values_equal(&item, &item_type, &literal, &literal_type)
            }
            None => false,
        }
    }

    /// Check a present attribute against the fixed value of its use, if any.
    #[allow(clippy::too_many_arguments)]
    fn check_attribute_fixed_value(
        &self,
        attr_name: &str,
        value: &str,
        type_ref: &TypeRef,
        fixed: Option<&str>,
        doc: &Document,
        node: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        if let Some(fixed) = fixed {
            if self.is_id_type(type_ref) {
                // a-props-correct.3: an ID-typed attribute has no value constraint.
                errors.push(ValidationError {
                    message: format!(
                        "Attribute '{}' has type ID and cannot have a fixed value",
                        attr_name
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            } else if !self.matches_fixed_value(value, fixed, type_ref, doc, node) {
                errors.push(ValidationError {
                    message: format!(
                        "Attribute '{}' has fixed value '{}' but its value is '{}'",
                        attr_name, fixed, value
                    ),
                    line: Some(doc.node_line(node)),
                    column: Some(doc.node_column(node)),
                });
            }
        }
    }

    /// Whether a simple type is `xs:ID` or restricts it.
    fn is_id_type(&self, type_ref: &TypeRef) -> bool {
        let st = match type_ref {
            TypeRef::BuiltIn(bt) => return *bt == BuiltInType::ID,
            TypeRef::Inline(td) => match td.as_ref() {
                TypeDef::Simple(st) => st,
                TypeDef::Complex(_) => return false,
            },
            TypeRef::Named(ns, name) => match self.types.get(&(ns.clone(), name.clone())) {
                Some(TypeDef::Simple(st)) => st,
                Some(TypeDef::Complex(_)) => return false,
                None => {
                    return ns.as_deref() == Some(XS_NAMESPACE)
                        && parse_builtin_type(name) == Some(BuiltInType::ID)
                }
            },
            TypeRef::Unqualified(_) => return false,
        };
        self.simple_type_chain(st).is_ok_and(|chain| {
            let root = chain[chain.len() - 1];
            root.union_members.is_none() && !root.is_list && root.base == BuiltInType::ID
        })
    }

    /// The simple types an element's text must satisfy when the element has
    /// a complex type with simple content: the base's simple content type
    /// (following `simpleContent` derivations from complex types, and
    /// `complexContent` extensions without particles of such a type) and,
    /// for a `simpleContent` restriction, that base narrowed by the
    /// restriction's facets. Empty for element-only, mixed or empty content.
    fn simple_content_types(&self, ct: &ComplexTypeDef) -> Result<Vec<TypeRef>, String> {
        match self.content_type(ct) {
            ContentType::Simple(owner) => self.simple_content_types_inner(owner, 0),
            ContentType::Model(..) => Ok(Vec::new()),
        }
    }

    fn simple_content_types_inner(
        &self,
        ct: &ComplexTypeDef,
        depth: usize,
    ) -> Result<Vec<TypeRef>, String> {
        if depth > MAX_SIMPLE_TYPE_DEPTH {
            return Err(format!(
                "simpleContent derivation deeper than {} levels",
                MAX_SIMPLE_TYPE_DEPTH
            ));
        }
        let ContentModel::SimpleContent(base) = &ct.content else {
            return Ok(Vec::new());
        };
        let mut types = match base.as_ref() {
            TypeRef::BuiltIn(BuiltInType::AnyType) => Vec::new(),
            TypeRef::BuiltIn(_) => vec![base.as_ref().clone()],
            TypeRef::Inline(td) => match td.as_ref() {
                TypeDef::Simple(_) => vec![base.as_ref().clone()],
                TypeDef::Complex(inner) => self.simple_content_types_inner(inner, depth + 1)?,
            },
            TypeRef::Named(ns, name) => match self.types.get(&(ns.clone(), name.clone())) {
                Some(TypeDef::Simple(_)) => vec![base.as_ref().clone()],
                Some(TypeDef::Complex(base_ct)) => match self.content_type(base_ct) {
                    ContentType::Simple(owner) => self.simple_content_types_inner(owner, depth + 1)?,
                    // src-ct.2: the base of a simpleContent derivation has
                    // simple content, or, for a restriction with its own
                    // simpleType, mixed content that can be empty.
                    ContentType::Model(model, mixed)
                        if mixed
                            && self.model_is_emptiable(&model)
                            && ct
                                .simple_content_restriction
                                .as_ref()
                                .is_some_and(|r| r.base_ref.is_some()) =>
                    {
                        Vec::new()
                    }
                    ContentType::Model(..) => {
                        return Err(format!(
                            "Base type '{}' of a simpleContent derivation does not have simple content (src-ct.2)",
                            qname_display(ns, name)
                        ))
                    }
                },
                None => {
                    return Err(format!("Base type '{}' not found", qname_display(ns, name)))
                }
            },
            // Decided at build; never present in a built validator.
            TypeRef::Unqualified(name) => {
                return Err(format!("Base type '{}' not found", name.local))
            }
        };
        if let Some(restriction) = &ct.simple_content_restriction {
            // The restriction step: its facets on top of its anonymous
            // simpleType when it has one, else on top of the base's content.
            let step_base = match &restriction.base_ref {
                Some(inline) => Some(inline.clone()),
                None if types.is_empty() => None,
                None => Some(types.remove(0)),
            };
            let mut step = restriction.as_ref().clone();
            match step_base {
                Some(TypeRef::BuiltIn(bt)) => {
                    step.base = bt;
                    step.base_ref = None;
                }
                Some(other) => step.base_ref = Some(other),
                None => {}
            }
            if step.base_ref.is_some() || step.base != BuiltInType::AnySimpleType {
                types.insert(0, TypeRef::Inline(Box::new(TypeDef::Simple(step))));
            }
        }
        Ok(types)
    }

    /// The simple type an element's value is read with, for comparing it
    /// with the element's fixed value; `None` when the element has mixed,
    /// element-only or empty content (compared character for character).
    fn element_value_type(&self, decl: &ElementDecl) -> Option<TypeRef> {
        self.value_type_of(&decl.type_ref)
    }

    /// The simple type a value of `type_ref` is read with; see
    /// [`XsdValidator::element_value_type`].
    fn value_type_of(&self, type_ref: &TypeRef) -> Option<TypeRef> {
        match type_ref {
            TypeRef::BuiltIn(BuiltInType::AnyType) => None,
            TypeRef::BuiltIn(_) => Some(type_ref.clone()),
            _ => match self.resolve_type(type_ref)? {
                TypeDef::Simple(_) => Some(type_ref.clone()),
                TypeDef::Complex(ct) if !ct.mixed => {
                    self.simple_content_types(ct).ok()?.into_iter().next()
                }
                TypeDef::Complex(_) => None,
            },
        }
    }

    /// Validate a value against a simple type, following its whole derivation chain.
    ///
    /// XSD 1.0 Part 2 semantics:
    /// - an atomic value must be valid for the built-in type at the root of the
    ///   chain and satisfy the facets of **every** restriction step; range facets
    ///   compare in the root type's value space (dates, decimals, integers);
    /// - within one step, `pattern` facets are alternatives (ORed); across steps
    ///   they all apply (ANDed);
    /// - the `whiteSpace` facet of the most-derived step that sets one applies,
    ///   otherwise the root built-in type's whitespace handling; a step's
    ///   enumeration literals are values of that step's base type, so they are
    ///   normalized with the mode in effect for the base (not the most-derived
    ///   one), then compared with the value as values;
    /// - a list splits the collapsed value into items, validates each item
    ///   against the item type and applies each step's list facets;
    /// - a union accepts the value if any member type accepts it, tried in
    ///   declaration order; `pattern`/`enumeration` facets of restrictions of the
    ///   union then apply to the value as the accepting member normalized it.
    fn validate_simple_value(
        &self,
        value: &str,
        st: &SimpleTypeDef,
        doc: &Document,
        node: NodeId,
        errors: &mut Vec<ValidationError>,
    ) {
        self.check_simple_value(value, st, doc, node, errors, 0, &mut UnionMemo::new());
    }

    /// Validate `value` against `st`, pushing errors. On success returns the
    /// value as normalized by the type and the built-in type whose value space
    /// it belongs to (used to apply union facets). `memo` caches which member
    /// of a union accepts a value, so that unions sharing members are
    /// validated in time linear in the number of distinct types.
    #[allow(clippy::too_many_arguments)]
    fn check_simple_value(
        &self,
        value: &str,
        st: &SimpleTypeDef,
        doc: &Document,
        node: NodeId,
        errors: &mut Vec<ValidationError>,
        depth: usize,
        memo: &mut UnionMemo,
    ) -> Option<(String, BuiltInType)> {
        let error = |message: String| ValidationError {
            message,
            line: Some(doc.node_line(node)),
            column: Some(doc.node_column(node)),
        };
        if depth > MAX_SIMPLE_TYPE_DEPTH {
            errors.push(error(format!(
                "Simple type nesting deeper than {} levels",
                MAX_SIMPLE_TYPE_DEPTH
            )));
            return None;
        }
        let chain = match self.simple_type_chain(st) {
            Ok(chain) => chain,
            Err(message) => {
                errors.push(error(message));
                return None;
            }
        };
        let root = chain[chain.len() - 1];
        let start = errors.len();

        let (normalized, value_type) = if root.union_members.is_some() {
            let Some((normalized, member_type)) =
                self.union_member_value(value, root, doc, node, depth, memo)
            else {
                errors.push(error(format!(
                    "Value '{}' does not match any member type of the union",
                    value
                )));
                return None;
            };
            for step in &chain {
                self.validate_step_facets(
                    &normalized,
                    &step.facets,
                    FacetTarget::Union(&member_type, root, step),
                    doc,
                    node,
                    errors,
                    depth,
                    memo,
                );
            }
            (normalized, member_type)
        } else if root.is_list {
            let normalized = apply_whitespace_normalization(value, &WhiteSpaceHandling::Collapse);
            let items: Vec<&str> = split_xml_whitespace(&normalized).collect();
            if let Some(item_ref) = &root.item_ref {
                // User-defined or anonymous item type: validate each item
                // against the whole item type (its chain, a union, ...).
                for item in &items {
                    self.check_type_ref_value(item, item_ref, doc, node, errors, depth + 1, memo);
                }
            } else if let Some((item_bt, item_facets)) = [st, root]
                .into_iter()
                .find_map(|s| s.item_type.as_ref().map(|bt| (bt, &s.item_facets)))
            {
                for item in &items {
                    validate_builtin_value(item, item_bt, doc, node, errors, self.lenient);
                    // Also validate item-level facets (from user-defined item types)
                    for facet in item_facets {
                        validate_facet(
                            item,
                            facet,
                            item_bt,
                            None,
                            doc,
                            node,
                            errors,
                            self.enforce_qname_length_facets,
                        );
                    }
                }
            }
            // List-level facets (length counts items, not chars), for every step.
            for step in &chain {
                self.validate_step_facets(
                    &normalized,
                    &step.facets,
                    FacetTarget::List(&items),
                    doc,
                    node,
                    errors,
                    depth,
                    memo,
                );
            }
            (normalized, BuiltInType::String)
        } else {
            // `step_ws[k]`: the whitespace mode in effect for step k's
            // *base* type, that is the most-derived whiteSpace facet among
            // the steps after k, else the root built-in type's. Step k's
            // enumeration literals are values of that base (XSD 1.0 Part 2
            // 4.3.5), so they are read with it; the value itself is read
            // with the mode of the whole chain (`step_ws` of a virtual
            // step before the first).
            let mut step_ws = vec![whitespace_for_type(&root.base); chain.len()];
            let mut in_effect = whitespace_for_type(&root.base);
            for (k, step) in chain.iter().enumerate().rev() {
                step_ws[k] = in_effect.clone();
                if let Some(mode) = step.facets.iter().find_map(|facet| match facet {
                    Facet::WhiteSpace(mode) => Some(mode),
                    _ => None,
                }) {
                    in_effect = mode.clone();
                }
            }
            let normalized = apply_whitespace_normalization(value, &in_effect);
            validate_builtin_value(&normalized, &root.base, doc, node, errors, self.lenient);
            for (step, literal_ws) in chain.iter().zip(&step_ws) {
                self.validate_step_facets(
                    &normalized,
                    &step.facets,
                    FacetTarget::Atomic(&root.base, literal_ws),
                    doc,
                    node,
                    errors,
                    depth,
                    memo,
                );
            }
            (normalized, root.base.clone())
        };

        (errors.len() == start).then_some((normalized, value_type))
    }

    /// The first member of the union `root` (in declaration order) that
    /// accepts `value`, as that member's normalized value and built-in type.
    /// Results are cached in `memo` per union definition and value.
    fn union_member_value(
        &self,
        value: &str,
        root: &SimpleTypeDef,
        doc: &Document,
        node: NodeId,
        depth: usize,
        memo: &mut UnionMemo,
    ) -> Option<(String, BuiltInType)> {
        let key = (root as *const SimpleTypeDef as usize, value.to_string());
        if let Some(cached) = memo.get(&key) {
            return cached.clone();
        }
        test_counters::note(&test_counters::UNION_MEMBER_SELECTIONS);
        let mut accepted = None;
        for member in root.union_members.iter().flatten() {
            let mut member_errors = Vec::new();
            let result = self.check_type_ref_value(
                value,
                member,
                doc,
                node,
                &mut member_errors,
                depth + 1,
                memo,
            );
            if member_errors.is_empty() && result.is_some() {
                accepted = result;
                break;
            }
        }
        memo.insert(key, accepted.clone());
        accepted
    }

    /// Validate a value against a type reference used as a union member or a
    /// list item type. See [`XsdValidator::check_simple_value`].
    #[allow(clippy::too_many_arguments)]
    fn check_type_ref_value(
        &self,
        value: &str,
        type_ref: &TypeRef,
        doc: &Document,
        node: NodeId,
        errors: &mut Vec<ValidationError>,
        depth: usize,
        memo: &mut UnionMemo,
    ) -> Option<(String, BuiltInType)> {
        let error = |message: String| ValidationError {
            message,
            line: Some(doc.node_line(node)),
            column: Some(doc.node_column(node)),
        };
        let builtin = |bt: &BuiltInType, errors: &mut Vec<ValidationError>| {
            let start = errors.len();
            validate_builtin_value(value, bt, doc, node, errors, self.lenient);
            let normalized = apply_whitespace_normalization(value, &whitespace_for_type(bt));
            (errors.len() == start).then(|| (normalized, bt.clone()))
        };
        match type_ref {
            TypeRef::BuiltIn(bt) => builtin(bt, errors),
            // Decided at build; never present in a built validator.
            TypeRef::Unqualified(name) => {
                errors.push(error(format!("Type '{}' not found", name.local)));
                None
            }
            TypeRef::Inline(td) => match td.as_ref() {
                TypeDef::Simple(st) => {
                    self.check_simple_value(value, st, doc, node, errors, depth, memo)
                }
                TypeDef::Complex(_) => {
                    errors.push(error("Anonymous complex type used as a simple type".into()));
                    None
                }
            },
            TypeRef::Named(ns, name) => match self.types.get(&(ns.clone(), name.clone())) {
                Some(TypeDef::Simple(st)) => {
                    self.check_simple_value(value, st, doc, node, errors, depth, memo)
                }
                Some(TypeDef::Complex(_)) => {
                    errors.push(error(format!(
                        "Type '{}' is not a simple type",
                        qname_display(ns, name)
                    )));
                    None
                }
                None => match parse_builtin_type(name) {
                    Some(bt) if ns.as_deref() == Some(XS_NAMESPACE) => builtin(&bt, errors),
                    _ => {
                        errors.push(error(format!(
                            "Type '{}' not found",
                            qname_display(ns, name)
                        )));
                        None
                    }
                },
            },
        }
    }

    /// The restriction chain of a simple type: `st` first, then each base in
    /// turn, ending with the step whose base is a built-in type (or which is a
    /// list or union). Fails on a missing or non-simple base, and on a cyclic or
    /// overlong chain.
    pub(super) fn simple_type_chain<'a>(
        &'a self,
        st: &'a SimpleTypeDef,
    ) -> Result<Vec<&'a SimpleTypeDef>, String> {
        let mut chain = vec![st];
        let mut current = st;
        while let Some(base) = &current.base_ref {
            let (next, display) = match base {
                TypeRef::BuiltIn(_) => break,
                // Decided at build; never present in a built validator.
                TypeRef::Unqualified(name) => {
                    return Err(format!("Base type '{}' not found", name.local))
                }
                TypeRef::Inline(td) => match td.as_ref() {
                    TypeDef::Simple(base_st) => (base_st, "anonymous".to_string()),
                    TypeDef::Complex(_) => {
                        return Err("The base of a simple type is a complex type".to_string())
                    }
                },
                TypeRef::Named(ns, name) => {
                    let display = qname_display(ns, name);
                    match self.types.get(&(ns.clone(), name.clone())) {
                        Some(TypeDef::Simple(base_st)) => (base_st, display),
                        Some(TypeDef::Complex(_)) => {
                            return Err(format!(
                                "Base type '{}' of a simple type is not a simple type",
                                display
                            ))
                        }
                        None => return Err(format!("Base type '{}' not found", display)),
                    }
                }
            };
            if chain.len() >= MAX_SIMPLE_TYPE_DEPTH || chain.iter().any(|s| std::ptr::eq(*s, next))
            {
                return Err(format!(
                    "Simple type derivation through '{}' is circular or longer than {} steps",
                    display, MAX_SIMPLE_TYPE_DEPTH
                ));
            }
            chain.push(next);
            current = next;
        }
        Ok(chain)
    }

    /// Check one restriction step's facets. The step's `pattern` facets are
    /// alternatives: the value must match at least one of them. Every other
    /// facet must hold.
    #[allow(clippy::too_many_arguments)]
    fn validate_step_facets(
        &self,
        text: &str,
        facets: &[Facet],
        target: FacetTarget<'_>,
        doc: &Document,
        node: NodeId,
        errors: &mut Vec<ValidationError>,
        depth: usize,
        memo: &mut UnionMemo,
    ) {
        let error = |message: String| ValidationError {
            message,
            line: Some(doc.node_line(node)),
            column: Some(doc.node_column(node)),
        };
        let patterns: Vec<&String> = facets
            .iter()
            .filter_map(|facet| match facet {
                Facet::Pattern(p) => Some(p),
                _ => None,
            })
            .collect();
        if patterns.len() > 1 {
            let mut matched = false;
            for pattern in &patterns {
                match XsdRegex::compile(pattern) {
                    Ok(re) => matched |= re.is_match(text),
                    Err(e) => {
                        errors.push(error(format!(
                            "Pattern facet '{}' could not be compiled: {}",
                            pattern, e
                        )));
                        return;
                    }
                }
            }
            if !matched {
                errors.push(error(format!(
                    "Value '{}' does not match any of the patterns {:?}",
                    text, patterns
                )));
            }
        }
        for facet in facets {
            if patterns.len() > 1 && matches!(facet, Facet::Pattern(_)) {
                continue;
            }
            match target {
                FacetTarget::Atomic(bt, ws) => validate_facet(
                    text,
                    facet,
                    bt,
                    Some(ws),
                    doc,
                    node,
                    errors,
                    self.enforce_qname_length_facets,
                ),
                FacetTarget::List(items) => {
                    validate_list_facet(items, facet, text, doc, node, errors)
                }
                FacetTarget::Union(bt, root, step) => match facet {
                    Facet::Pattern(_) => validate_facet(
                        text,
                        facet,
                        bt,
                        None,
                        doc,
                        node,
                        errors,
                        self.enforce_qname_length_facets,
                    ),
                    Facet::Enumeration(values) => {
                        // Each literal is read by the union like a value: the
                        // member that accepts it gives its type and value.
                        // The readings are kept with the step, so they are
                        // made once per type, not once per value.
                        let mut read = |literal: &str| {
                            test_counters::note(&test_counters::UNION_LITERAL_READS);
                            self.union_member_value(literal, root, doc, node, depth, memo)
                        };
                        let cached = step.union_enumeration.get_or_init(|| UnionEnumeration {
                            lenient: self.lenient,
                            enforce_qname_length_facets: self.enforce_qname_length_facets,
                            literals: values
                                .iter()
                                .map(|literal| {
                                    if literal.contains(':') {
                                        UnionLiteral::Scoped
                                    } else {
                                        match read(literal) {
                                            Some((lit, lit_type)) => {
                                                UnionLiteral::Value(lit, lit_type)
                                            }
                                            None => UnionLiteral::Invalid,
                                        }
                                    }
                                })
                                .collect(),
                        });
                        let usable = cached.lenient == self.lenient
                            && cached.enforce_qname_length_facets
                                == self.enforce_qname_length_facets
                            && cached.literals.len() == values.len();
                        let found = values.iter().enumerate().any(|(i, literal)| {
                            let reading = match cached.literals.get(i) {
                                Some(UnionLiteral::Value(lit, lit_type)) if usable => {
                                    return values_equal(text, bt, lit, lit_type);
                                }
                                Some(UnionLiteral::Invalid) if usable => return false,
                                _ => read(literal),
                            };
                            reading.is_some_and(|(lit, lit_type)| {
                                values_equal(text, bt, &lit, &lit_type)
                            })
                        });
                        if !found {
                            errors.push(error(format!(
                                "'{}' is not one of the allowed values: {:?}",
                                text, values
                            )));
                        }
                    }
                    other => errors.push(error(format!(
                        "Facet {:?} does not apply to a union type",
                        other
                    ))),
                },
            }
        }
    }
}

/// Operation counters for the complexity tests; empty outside `cfg(test)`.
pub(crate) mod test_counters {
    #[cfg(test)]
    thread_local! {
        /// Union enumeration literals read by the union on this thread.
        pub(crate) static UNION_LITERAL_READS: std::cell::Cell<usize> =
            const { std::cell::Cell::new(0) };
        /// Union member selections made (not found in the memo) on this thread.
        pub(crate) static UNION_MEMBER_SELECTIONS: std::cell::Cell<usize> =
            const { std::cell::Cell::new(0) };
        /// Particles copied from the schema while building content types.
        pub(crate) static CONTENT_PARTICLE_COPIES: std::cell::Cell<usize> =
            const { std::cell::Cell::new(0) };
    }

    #[cfg(test)]
    pub(crate) fn note(counter: &'static std::thread::LocalKey<std::cell::Cell<usize>>) {
        counter.with(|count| count.set(count.get() + 1));
    }

    #[cfg(test)]
    pub(crate) fn note_many(
        counter: &'static std::thread::LocalKey<std::cell::Cell<usize>>,
        n: usize,
    ) {
        counter.with(|count| count.set(count.get() + n));
    }

    #[cfg(test)]
    pub(crate) fn take(counter: &'static std::thread::LocalKey<std::cell::Cell<usize>>) -> usize {
        counter.with(|count| count.replace(0))
    }

    #[cfg(not(test))]
    pub(crate) struct Counter;
    #[cfg(not(test))]
    pub(crate) const UNION_LITERAL_READS: Counter = Counter;
    #[cfg(not(test))]
    pub(crate) const UNION_MEMBER_SELECTIONS: Counter = Counter;
    #[cfg(not(test))]
    pub(crate) const CONTENT_PARTICLE_COPIES: Counter = Counter;

    #[cfg(not(test))]
    #[inline(always)]
    pub(crate) fn note(_counter: &Counter) {}

    #[cfg(not(test))]
    #[inline(always)]
    pub(crate) fn note_many(_counter: &Counter, _n: usize) {}
}

/// Cache of union member selection: (address of the union's root
/// definition, value) to the accepting member's normalized value and type.
type UnionMemo = HashMap<(usize, String), Option<(String, BuiltInType)>>;

/// Maximum number of restriction steps in a simple type's derivation chain,
/// and of nested union member / list item types, followed during validation.
pub(super) const MAX_SIMPLE_TYPE_DEPTH: usize = 64;

/// The base type of a built-in type in the XSD 1.0 Part 2 type hierarchy
/// (§3, the built-in datatype hierarchy); `None` for `xs:anyType`. The list
/// types are derived from `xs:anySimpleType`.
fn builtin_base_type(bt: &BuiltInType) -> Option<BuiltInType> {
    use BuiltInType as B;
    Some(match bt {
        B::AnyType => return None,
        B::AnySimpleType => B::AnyType,
        B::String
        | B::Boolean
        | B::Decimal
        | B::Float
        | B::Double
        | B::Duration
        | B::DateTime
        | B::Time
        | B::Date
        | B::GYearMonth
        | B::GYear
        | B::GMonthDay
        | B::GDay
        | B::GMonth
        | B::HexBinary
        | B::Base64Binary
        | B::AnyURI
        | B::QName
        | B::NOTATION
        | B::IDREFS
        | B::NMTOKENS
        | B::ENTITIES => B::AnySimpleType,
        B::NormalizedString => B::String,
        B::Token => B::NormalizedString,
        B::Language | B::NMTOKEN | B::Name => B::Token,
        B::NCName => B::Name,
        B::ID | B::IDREF | B::ENTITY => B::NCName,
        B::Integer => B::Decimal,
        B::NonPositiveInteger | B::Long | B::NonNegativeInteger => B::Integer,
        B::NegativeInteger => B::NonPositiveInteger,
        B::Int => B::Long,
        B::Short => B::Int,
        B::Byte => B::Short,
        B::UnsignedLong | B::PositiveInteger => B::NonNegativeInteger,
        B::UnsignedInt => B::UnsignedLong,
        B::UnsignedShort => B::UnsignedInt,
        B::UnsignedByte => B::UnsignedShort,
    })
}

/// How a derivation step's facets are applied: to an atomic value of the
/// given built-in type, whose step reads its enumeration literals with the
/// given whitespace mode (the one in effect for the step's base); to the
/// items of a list; or to a union value that the member of the given
/// built-in type accepted (the union's root definition reads enumeration
/// literals, and the step, the last field, keeps them read).
#[derive(Clone, Copy)]
enum FacetTarget<'a> {
    Atomic(&'a BuiltInType, &'a WhiteSpaceHandling),
    List(&'a [&'a str]),
    Union(&'a BuiltInType, &'a SimpleTypeDef, &'a SimpleTypeDef),
}

/// `{namespace}local` for messages, or just `local` without a namespace.
pub(super) fn qname_display(ns: &Option<String>, name: &str) -> String {
    match ns {
        Some(uri) => format!("{{{}}}{}", uri, name),
        None => name.to_string(),
    }
}
