//! XSD schema builder — constructs an `XsdValidator` from a parsed XSD document.
//!
//! Entry points are `XsdValidator::from_schema` (simple) and
//! `from_schema_with_base_path` (supports external `schemaLocation` resolution).
//!
//! The build proceeds in multiple passes:
//!   0. Schema composition (`xs:include`, `xs:redefine`, `xs:import`)
//!   0.5. Global attribute declarations (needed by attributeGroup parsing)
//!   1. Attribute-group and model-group definitions
//!   2. All other top-level declarations (elements, complex/simple types, attributes)
//!   3. Substitution-group map construction (direct + transitive membership)
//!   4. List-type resolution passes (base-type propagation, item-type facets,
//!      inline list-type facets in elements and content-model particles)

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::dom::{Document, NodeKind};
use crate::error::{XmlError, XmlResult};

use super::builtins::{instant_is_unrepresentable, trim_xml_whitespace, values_equal};
use super::composition::{
    process_schema_composition, resolve_unqualified_type_refs, CompositionState,
};
use super::facet_resolution::{
    resolve_content_model_list_item_facets, resolve_inline_list_item_facets, ListBasesMap,
};
use super::parser::{
    parse_attribute_group_def, parse_complex_type, parse_element_decl, parse_model_group_def,
    parse_simple_type, resolve_type_name,
};
use super::types::{
    AttributeDecl, BuiltInType, ComplexTypeDef, ContentModel, ElementDecl, Facet, Particle,
    ParticleKind, SimpleTypeDef, TypeDef, TypeRef, UnqualifiedTypeName, XsdValidator,
};
use super::validation::{qname_display, MAX_SIMPLE_TYPE_DEPTH};
use super::XS_NAMESPACE;

impl XsdValidator {
    /// Build a validator from a parsed XSD schema document.
    ///
    /// Equivalent to `from_schema_with_base_path(schema_doc, None)`.
    pub fn from_schema(schema_doc: &Document) -> XmlResult<Self> {
        Self::from_schema_with_base_path(schema_doc, None)
    }

    /// Set whether length/minLength/maxLength facets on QName and NOTATION types
    /// are enforced. Default is `true` (enforce). Set to `false` to ignore them,
    /// which matches the NIST test suite interpretation of W3C Bug #4009.
    pub fn set_enforce_qname_length_facets(&mut self, enforce: bool) {
        self.enforce_qname_length_facets = enforce;
    }

    /// Enable libxml2-compatible **lenient** datatype validation. Default is
    /// `false` (strict, spec-faithful).
    ///
    /// # What it does
    ///
    /// Most of the XSD ecosystem — lxml, pyFF, and virtually every Python/C XML
    /// stack — is built on **libxml2**, which validates a few datatypes slightly
    /// more permissively than the letter of XSD 1.0 Part 2 / RFC 3987. Documents
    /// that libxml2 (and therefore every tool downstream of it) treats as valid
    /// can otherwise produce *spurious* errors under a strict validator. Calling
    /// `set_lenient(true)` relaxes exactly those checks so uppsala agrees with
    /// libxml2 on such documents.
    ///
    /// # Scope — one rule today
    ///
    /// Lenient mode currently affects a **single** check:
    ///
    /// - **`xs:anyURI`**: a value containing a space is **accepted**. uppsala's
    ///   only `anyURI` lexical check is "reject if the (whitespace-normalized)
    ///   value contains a space" — a space is invalid per RFC 3987. Strict mode
    ///   keeps that check; lenient mode drops it, matching libxml2. This applies
    ///   to an `anyURI` in **element content and in an attribute value** alike.
    ///
    /// Everything else is unchanged. `set_lenient(true)` does **not** weaken any
    /// other datatype, facet, or structural check: a malformed `xs:int` is still
    /// rejected, `xs:pattern`/`enumeration`/length facets still apply, required
    /// attributes and content models are still enforced. It only ever *accepts*
    /// `anyURI` values that strict mode would reject — it never rejects more.
    ///
    /// # The values that failed before (real SAML metadata)
    ///
    /// Both motivating failures (from pyFF's `swamid-2.0-test.xml`, see ADR 0012)
    /// are the **same** rule — a single `anyURI` value containing a space:
    ///
    /// 1. **`mdui:GeolocationHint` element** (`anyURI` content) with a
    ///    space after the comma:
    ///    ```text
    ///    <mdui:GeolocationHint>geo:40.6308255004333, 22.959268014038116</mdui:GeolocationHint>
    ///    ```
    ///
    /// 2. **`idpdisc:DiscoveryResponse/@Location`** (a *single* `anyURI`
    ///    attribute) with three space-separated tokens crammed into it —
    ///    malformed metadata (a `Location` is one URI), but libxml2 accepts it:
    ///    ```text
    ///    Location="urn:oasis:names:tc:SAML:2.0:protocol urn:oasis:names:tc:SAML:1.1:protocol http://docs.oasis-open.org/wsfed/federation/200706/secext"
    ///    ```
    ///
    /// # Not a list-typing bug
    ///
    /// Case 2 *looks* like SAML's `protocolSupportEnumeration` (a `list` of
    /// `anyURI`), which first suggested list typing was being lost across the
    /// cross-import `xsi:type` chain. It is not: `protocolSupportEnumeration`
    /// validates correctly **per item** everywhere — including when a
    /// `RoleDescriptor` is substituted via `xsi:type` to a WS-Fed type that
    /// extends the SAML base across a *different* imported schema. The failing
    /// value above is a `Location` (a lone `anyURI`), not the list attribute.
    /// (Pinned by
    /// `tests/xsd_conformance.rs::cross_import_xsi_type_list_attribute_validates_per_item`.)
    ///
    /// # When to enable
    ///
    /// Turn this on when validating documents authored for libxml2/Xerces (e.g.
    /// SAML metadata) where strict mode reports datatype errors those processors
    /// accept. Leave it **off** for spec-conformant validation — the default
    /// keeps uppsala at 100% on the W3C XML Schema Test Suite (NIST/MS/Sun).
    ///
    /// # Example
    ///
    /// ```
    /// use uppsala::{parse, XsdValidator};
    ///
    /// let schema = parse(
    ///     r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    ///          <xs:element name="loc" type="xs:anyURI"/>
    ///        </xs:schema>"#,
    /// )
    /// .unwrap();
    ///
    /// // A real mdui:GeolocationHint value: a "geo:" URI with a space.
    /// let doc = parse("<loc>geo:40.6308255004333, 22.959268014038116</loc>").unwrap();
    ///
    /// // Strict (default): the space makes it an invalid anyURI (RFC 3987).
    /// let strict = XsdValidator::from_schema(&schema).unwrap();
    /// assert!(!strict.validate(&doc).is_empty());
    ///
    /// // Lenient: accepted, matching libxml2 / lxml / pyFF.
    /// let mut lenient = XsdValidator::from_schema(&schema).unwrap();
    /// lenient.set_lenient(true);
    /// assert!(lenient.validate(&doc).is_empty());
    ///
    /// // Leniency is scoped: a malformed int is still rejected.
    /// let int_schema = parse(
    ///     r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
    ///          <xs:element name="n" type="xs:int"/>
    ///        </xs:schema>"#,
    /// )
    /// .unwrap();
    /// let mut v = XsdValidator::from_schema(&int_schema).unwrap();
    /// v.set_lenient(true);
    /// assert!(!v.validate(&parse("<n>not-an-int</n>").unwrap()).is_empty());
    /// ```
    ///
    /// See ADR 0012 (`docs/adr/0012-libxml2-lenient-datatype-mode.md`) for the
    /// full rationale and regression tests (`anyuri_space_strict_rejected_lenient_accepted`,
    /// `anyuri_multitoken_value_lenient`, `lenient_mode_keeps_other_datatype_checks`).
    pub fn set_lenient(&mut self, lenient: bool) {
        self.lenient = lenient;
    }

    /// Build a validator from a parsed XSD schema document, with a base path
    /// for resolving `schemaLocation` attributes in `xs:include` and `xs:redefine`.
    ///
    /// # Passes
    ///
    /// 1. **Pass 0** — schema composition: `xs:include` / `xs:redefine` / `xs:import`
    /// 2. **Pass 0.5** — global attribute declarations (needed before attributeGroup parsing)
    /// 3. **Pass 1** — attribute-group and model-group definitions
    /// 4. **Pass 2** — top-level elements, complex types, simple types, remaining attributes
    /// 5. **Substitution-group construction** — direct + transitive membership map
    /// 6. **List-type resolution** — three sub-passes to propagate `is_list`, `item_type`,
    ///    and `item_facets` through the type graph and into inline declarations
    pub fn from_schema_with_base_path(
        schema_doc: &Document,
        base_path: Option<&Path>,
    ) -> XmlResult<Self> {
        // Public entry creates fresh composition state (visited paths +
        // depth counter) and delegates to the internal variant. The
        // state is what lets `xs:include` / `xs:import` chains detect
        // cycles and enforce a nesting cap.
        let mut state = CompositionState::new(base_path);
        let validator =
            Self::from_schema_with_composition_state(schema_doc, base_path, &mut state)?;
        // Included and imported declarations are merged by now, so this is
        // the first point where every simple type reference can be checked.
        validator.check_simple_type_references()?;
        Ok(validator)
    }

    /// Refuse a schema whose simple types reference a missing or non-simple
    /// type (as a restriction base or list item type), whose references form
    /// a cycle or nest deeper than `MAX_SIMPLE_TYPE_DEPTH`, or which gives a
    /// facet declared `fixed` in a base type another value. Validation could
    /// otherwise only fail every value of such a type.
    ///
    /// Named simple types are checked first. The anonymous simple types of
    /// element and attribute declarations are checked the same way, and the
    /// named types used by attribute declarations and as `simpleContent`
    /// bases must exist; an attribute's type must be simple.
    fn check_simple_type_references(&self) -> XmlResult<()> {
        let mut keys: Vec<_> = self.types.keys().cloned().collect();
        keys.sort();
        // Height of each named simple type checked so far: the longest chain
        // of base / item / member references below it. Memoizing the height
        // (not just "checked") makes the nesting limit independent of the
        // order in which types are visited.
        let mut heights = HashMap::new();
        for key in &keys {
            if let Some(TypeDef::Simple(st)) = self.types.get(key) {
                if heights.contains_key(key) {
                    continue;
                }
                let mut visiting = vec![key.clone()];
                let height = self.check_simple_type_refs(st, &mut visiting, &mut heights, 0)?;
                heights.insert(key.clone(), height);
            }
        }

        let mut elements: Vec<_> = self.elements.iter().collect();
        elements.sort_by(|a, b| a.0.cmp(b.0));
        for (_, decl) in elements {
            self.check_element_decl_types(decl, &mut heights)?;
        }
        for key in &keys {
            if let Some(TypeDef::Complex(ct)) = self.types.get(key) {
                self.check_complex_type_refs(ct, &mut heights)?;
            }
        }
        let mut attributes: Vec<_> = self.global_attributes.iter().collect();
        attributes.sort_by(|a, b| a.0.cmp(b.0));
        for (_, decl) in attributes {
            self.check_attribute_type(decl, &mut heights)?;
        }
        let mut groups: Vec<_> = self.attribute_groups.iter().collect();
        groups.sort_by(|a, b| a.0.cmp(b.0));
        for (_, group) in groups {
            for decl in &group.attributes {
                self.check_attribute_type(decl, &mut heights)?;
            }
        }
        let mut model_groups: Vec<_> = self.model_groups.iter().collect();
        model_groups.sort_by(|a, b| a.0.cmp(b.0));
        for (_, group) in model_groups {
            self.check_content_model_refs(&group.content, &mut heights)?;
        }
        Ok(())
    }

    /// Depth-first walk of the types a simple type references, for
    /// `check_simple_type_references`. `visiting` holds the named types on the
    /// current path; `heights` the named types already checked, with their
    /// height. Returns the height of `st`.
    fn check_simple_type_refs(
        &self,
        st: &SimpleTypeDef,
        visiting: &mut Vec<(Option<String>, String)>,
        heights: &mut HashMap<(Option<String>, String), usize>,
        depth: usize,
    ) -> XmlResult<usize> {
        let too_deep = || {
            XmlError::validation(format!(
                "Simple type definitions nest deeper than {} levels",
                MAX_SIMPLE_TYPE_DEPTH
            ))
        };
        if depth > MAX_SIMPLE_TYPE_DEPTH {
            return Err(too_deep());
        }
        let refs = st.base_ref.iter().chain(st.item_ref.iter());
        let mut height = 0;
        for type_ref in refs {
            let below = match type_ref {
                TypeRef::BuiltIn(_) => continue,
                TypeRef::Unqualified(name) => return Err(unresolved_type_name(name)),
                TypeRef::Inline(td) => match td.as_ref() {
                    TypeDef::Simple(inner) => {
                        self.check_simple_type_refs(inner, visiting, heights, depth + 1)?
                    }
                    TypeDef::Complex(_) => {
                        return Err(XmlError::validation(
                            "A simple type definition contains a complex type",
                        ))
                    }
                },
                TypeRef::Named(ns, name) => {
                    let key = (ns.clone(), name.clone());
                    if let Some(&known) = heights.get(&key) {
                        known
                    } else if visiting.contains(&key) {
                        // XSD 1.0 src-simple-type.4 (union) and st-props-correct.2
                        // (restriction, list): no definition may reference itself.
                        return Err(XmlError::validation(format!(
                            "Circular simple type definition involving '{}'",
                            qname_display(ns, name)
                        )));
                    } else {
                        match self.types.get(&key) {
                            Some(TypeDef::Simple(target)) => {
                                visiting.push(key.clone());
                                let below = self.check_simple_type_refs(
                                    target,
                                    visiting,
                                    heights,
                                    depth + 1,
                                )?;
                                visiting.pop();
                                heights.insert(key, below);
                                below
                            }
                            Some(TypeDef::Complex(_)) => {
                                return Err(XmlError::validation(format!(
                                    "Type '{}' is used as a simple type but is a complex type",
                                    qname_display(ns, name)
                                )))
                            }
                            None => {
                                return Err(XmlError::validation(format!(
                                    "Type '{}' referenced by simple type '{}' is not defined",
                                    qname_display(ns, name),
                                    visiting
                                        .last()
                                        .map(|k| qname_display(&k.0, &k.1))
                                        .unwrap_or_else(|| "(anonymous)".to_string())
                                )))
                            }
                        }
                    }
                }
            };
            height = height.max(below + 1);
        }
        if height > MAX_SIMPLE_TYPE_DEPTH {
            return Err(too_deep());
        }
        self.check_simple_type_steps(st, visiting.last())?;
        Ok(height)
    }

    /// The per-definition checks of `check_simple_type_references`: the
    /// derivation chain resolves, and no step changes a facet a base step
    /// fixed.
    fn check_simple_type_steps(
        &self,
        st: &SimpleTypeDef,
        key: Option<&(Option<String>, String)>,
    ) -> XmlResult<()> {
        let display = || {
            key.map(|k| qname_display(&k.0, &k.1))
                .or_else(|| st.name.clone())
                .unwrap_or_else(|| "(anonymous)".to_string())
        };
        let chain = self.simple_type_chain(st).map_err(XmlError::validation)?;
        let root = chain[chain.len() - 1];
        for (i, step) in chain.iter().enumerate() {
            for facet in &step.facets {
                for base in &chain[i + 1..] {
                    if !base.fixed_facets.contains(&facet.name()) {
                        continue;
                    }
                    let fixed = base.facets.iter().find(|f| f.name() == facet.name());
                    if let Some(fixed) = fixed {
                        let unplaceable = [facet, fixed]
                            .into_iter()
                            .find_map(|f| unplaceable_range_value(f, &root.base));
                        if let Some(value) = unplaceable {
                            return Err(XmlError::validation(format!(
                                "Simple type '{}' cannot compare facet '{}' with the value a base type fixes: '{}' has a year too far from zero to place on the timeline",
                                display(),
                                facet.name(),
                                value
                            )));
                        }
                        if !facet_values_equal(facet, fixed, &root.base) {
                            return Err(XmlError::validation(format!(
                                "Simple type '{}' changes the value of facet '{}', which a base type fixes",
                                display(),
                                facet.name()
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Check the anonymous type of an element declaration (named element
    /// types are resolved at validation, where a missing one is reported).
    fn check_element_decl_types(
        &self,
        decl: &ElementDecl,
        heights: &mut HashMap<(Option<String>, String), usize>,
    ) -> XmlResult<()> {
        if decl.is_ref {
            return Ok(());
        }
        match &decl.type_ref {
            TypeRef::Inline(td) => match td.as_ref() {
                TypeDef::Simple(st) => {
                    self.check_simple_type_refs(st, &mut Vec::new(), heights, 0)?;
                }
                TypeDef::Complex(ct) => self.check_complex_type_refs(ct, heights)?,
            },
            TypeRef::Unqualified(name) => return Err(unresolved_type_name(name)),
            TypeRef::Named(..) | TypeRef::BuiltIn(_) => {}
        }
        Ok(())
    }

    /// Check the attribute types and local element declarations of a
    /// complex type.
    fn check_complex_type_refs(
        &self,
        ct: &ComplexTypeDef,
        heights: &mut HashMap<(Option<String>, String), usize>,
    ) -> XmlResult<()> {
        for decl in &ct.attributes {
            self.check_attribute_type(decl, heights)?;
        }
        self.check_content_model_refs(&ct.content, heights)
    }

    /// Check the local element declarations of a content model.
    fn check_content_model_refs(
        &self,
        content: &ContentModel,
        heights: &mut HashMap<(Option<String>, String), usize>,
    ) -> XmlResult<()> {
        match content {
            ContentModel::Sequence(particles, ..)
            | ContentModel::Choice(particles, ..)
            | ContentModel::All(particles) => self.check_particle_refs(particles, heights),
            _ => Ok(()),
        }
    }

    fn check_particle_refs(
        &self,
        particles: &[Particle],
        heights: &mut HashMap<(Option<String>, String), usize>,
    ) -> XmlResult<()> {
        for particle in particles {
            match &particle.kind {
                ParticleKind::Element(decl) => self.check_element_decl_types(decl, heights)?,
                ParticleKind::Sequence(inner) | ParticleKind::Choice(inner) => {
                    self.check_particle_refs(inner, heights)?
                }
                ParticleKind::Any { .. } => {}
            }
        }
        Ok(())
    }

    /// An attribute's named type must be a defined simple type; an anonymous
    /// one is checked like a named simple type.
    fn check_attribute_type(
        &self,
        decl: &AttributeDecl,
        heights: &mut HashMap<(Option<String>, String), usize>,
    ) -> XmlResult<()> {
        match &decl.type_ref {
            TypeRef::BuiltIn(_) => Ok(()),
            TypeRef::Unqualified(name) => Err(unresolved_type_name(name)),
            TypeRef::Named(ns, name) => match self.types.get(&(ns.clone(), name.clone())) {
                Some(TypeDef::Simple(_)) => Ok(()),
                Some(TypeDef::Complex(_)) => Err(XmlError::validation(format!(
                    "Type '{}' of attribute '{}' is a complex type",
                    qname_display(ns, name),
                    decl.name
                ))),
                None => Err(XmlError::validation(format!(
                    "Type '{}' of attribute '{}' is not defined",
                    qname_display(ns, name),
                    decl.name
                ))),
            },
            TypeRef::Inline(td) => match td.as_ref() {
                TypeDef::Simple(st) => self
                    .check_simple_type_refs(st, &mut Vec::new(), heights, 0)
                    .map(|_| ()),
                TypeDef::Complex(_) => Err(XmlError::validation(format!(
                    "Attribute '{}' has an anonymous complex type",
                    decl.name
                ))),
            },
        }
    }

    /// Internal entry used by `from_schema_with_base_path` and by
    /// recursive composition. Threads the `CompositionState` so visited
    /// paths and depth survive across nested `xs:include` / `xs:import`
    /// calls.
    pub(super) fn from_schema_with_composition_state(
        schema_doc: &Document,
        base_path: Option<&Path>,
        state: &mut CompositionState,
    ) -> XmlResult<Self> {
        let mut validator = XsdValidator {
            elements: HashMap::new(),
            types: HashMap::new(),
            global_attributes: HashMap::new(),
            attribute_groups: HashMap::new(),
            model_groups: HashMap::new(),
            target_namespace: None,
            block_default_extension: false,
            block_default_restriction: false,
            enforce_qname_length_facets: true,
            substitution_groups: HashMap::new(),
            lenient: false,
        };

        let schema_elem = schema_doc
            .document_element()
            .ok_or_else(|| XmlError::validation("Schema document has no root element"))?;

        // Get target namespace and elementFormDefault
        let mut element_form_qualified = false;
        if let Some(elem) = schema_doc.element(schema_elem) {
            validator.target_namespace =
                elem.get_attribute("targetNamespace").map(|s| s.to_string());
            element_form_qualified = elem.get_attribute("elementFormDefault") == Some("qualified");
            // Parse blockDefault
            if let Some(block_default) = elem.get_attribute("blockDefault") {
                for token in block_default.split_whitespace() {
                    match token {
                        "extension" => validator.block_default_extension = true,
                        "restriction" => validator.block_default_restriction = true,
                        "#all" => {
                            validator.block_default_extension = true;
                            validator.block_default_restriction = true;
                        }
                        _ => {}
                    }
                }
            }
        }

        // Determine the effective namespace for local element declarations:
        // If elementFormDefault="qualified", local elements inherit the target namespace.
        let local_elem_ns = if element_form_qualified {
            validator.target_namespace.clone()
        } else {
            None
        };

        // Pass 0: Process xs:include and xs:redefine to merge external schema declarations
        if base_path.is_some() {
            process_schema_composition(schema_doc, schema_elem, &mut validator, base_path, state)?;
        }

        // Pass 0.5: Parse global attribute declarations first, since attributeGroup
        // definitions may reference them via <attribute ref="..."/>.
        for child in schema_doc.children(schema_elem) {
            if let Some(NodeKind::Element(elem)) = schema_doc.node_kind(child) {
                let is_xs = elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                    || elem.name.prefix.as_deref() == Some("xs")
                    || elem.name.prefix.as_deref() == Some("xsd");
                if !is_xs {
                    continue;
                }
                if elem.name.local_name == "attribute" {
                    if let Some(attr_elem) = schema_doc.element(child) {
                        if let Some(name) = attr_elem.get_attribute("name") {
                            let type_ref = if let Some(type_attr) = attr_elem.get_attribute("type")
                            {
                                resolve_type_name(
                                    schema_doc,
                                    child,
                                    type_attr,
                                    &validator.target_namespace,
                                )?
                            } else {
                                // Check for inline simpleType child
                                let mut inline_type = None;
                                for gc in schema_doc.children(child) {
                                    if let Some(NodeKind::Element(ge)) = schema_doc.node_kind(gc) {
                                        if ge.name.local_name == "simpleType" {
                                            let td = parse_simple_type(schema_doc, gc)?;
                                            inline_type = Some(TypeRef::Inline(Box::new(td)));
                                        }
                                    }
                                }
                                inline_type.unwrap_or(TypeRef::BuiltIn(BuiltInType::String))
                            };
                            let required = attr_elem.get_attribute("use") == Some("required");
                            let default = attr_elem.get_attribute("default").map(|s| s.to_string());
                            let decl = AttributeDecl {
                                name: name.to_string(),
                                namespace: validator.target_namespace.clone(),
                                type_ref,
                                required,
                                default,
                                prohibited: false,
                                is_ref: false,
                                qualified: true,
                            };
                            let key = (validator.target_namespace.clone(), name.to_string());
                            validator.global_attributes.insert(key, decl);
                        }
                    }
                }
            }
        }

        // Pass 1: Parse attribute group and model group definitions
        // (both needed by complexType parsing in Pass 2)
        for child in schema_doc.children(schema_elem) {
            if let Some(NodeKind::Element(elem)) = schema_doc.node_kind(child) {
                let is_xs = elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                    || elem.name.prefix.as_deref() == Some("xs")
                    || elem.name.prefix.as_deref() == Some("xsd");
                if !is_xs {
                    continue;
                }
                if elem.name.local_name == "attributeGroup" {
                    if let Some(ag_elem) = schema_doc.element(child) {
                        if let Some(name) = ag_elem.get_attribute("name") {
                            let key = (validator.target_namespace.clone(), name.to_string());
                            // Top-level attribute-group names are unique within
                            // a schema namespace. Reject duplicates before
                            // parsing the replacement body so a later duplicate
                            // cannot clone or expand the earlier definition.
                            if validator.attribute_groups.contains_key(&key) {
                                return Err(XmlError::validation(format!(
                                    "Duplicate attributeGroup definition: {}",
                                    name
                                )));
                            }
                            let ag_def = parse_attribute_group_def(
                                schema_doc,
                                child,
                                &validator.target_namespace,
                                &validator.global_attributes,
                                &validator.attribute_groups,
                            )?;
                            validator.attribute_groups.insert(key, ag_def);
                        }
                    }
                }
                if elem.name.local_name == "group" {
                    if let Some(g_elem) = schema_doc.element(child) {
                        if let Some(name) = g_elem.get_attribute("name") {
                            let key = (validator.target_namespace.clone(), name.to_string());
                            // Top-level model-group names are unique within a
                            // schema namespace. Fuzzing found that overwriting a
                            // group after parsing refs to the previous version
                            // could amplify Particle clones before the final map
                            // insert. Fail closed before parsing the duplicate.
                            if validator.model_groups.contains_key(&key) {
                                return Err(XmlError::validation(format!(
                                    "Duplicate model group definition: {}",
                                    name
                                )));
                            }
                            let mg_def = parse_model_group_def(
                                schema_doc,
                                child,
                                &local_elem_ns,
                                &validator.target_namespace,
                                &validator.attribute_groups,
                                &validator.model_groups,
                                validator.block_default_extension,
                                validator.block_default_restriction,
                            )?;
                            validator.model_groups.insert(key, mg_def);
                        }
                    }
                }
            }
        }

        // Pass 2: Process all other top-level children
        for child in schema_doc.children(schema_elem) {
            if let Some(NodeKind::Element(elem)) = schema_doc.node_kind(child) {
                let local = &elem.name.local_name;
                let is_xs = elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                    || elem.name.prefix.as_deref() == Some("xs")
                    || elem.name.prefix.as_deref() == Some("xsd");

                if !is_xs {
                    continue;
                }

                match &**local {
                    "element" => {
                        let decl = parse_element_decl(
                            schema_doc,
                            child,
                            &validator.target_namespace,
                            &local_elem_ns,
                            &validator.target_namespace,
                            &validator.attribute_groups,
                            &validator.model_groups,
                            validator.block_default_extension,
                            validator.block_default_restriction,
                        )?;
                        let key = (validator.target_namespace.clone(), decl.name.clone());
                        validator.elements.insert(key, decl);
                    }
                    "complexType" => {
                        let type_def = parse_complex_type(
                            schema_doc,
                            child,
                            &local_elem_ns,
                            &validator.target_namespace,
                            &validator.target_namespace,
                            &validator.attribute_groups,
                            &validator.model_groups,
                            validator.block_default_extension,
                            validator.block_default_restriction,
                        )?;
                        if let TypeDef::Complex(ref ct) = type_def {
                            if let Some(name) = &ct.name {
                                let key = (validator.target_namespace.clone(), name.clone());
                                validator.types.insert(key, type_def);
                            }
                        }
                    }
                    "simpleType" => {
                        let type_def = parse_simple_type(schema_doc, child)?;
                        if let TypeDef::Simple(ref st) = type_def {
                            if let Some(name) = &st.name {
                                let key = (validator.target_namespace.clone(), name.clone());
                                validator.types.insert(key, type_def);
                            }
                        }
                    }
                    "attribute" => {
                        // Parse global attribute declarations
                        if let Some(attr_elem) = schema_doc.element(child) {
                            if let Some(name) = attr_elem.get_attribute("name") {
                                let type_ref = if let Some(type_attr) =
                                    attr_elem.get_attribute("type")
                                {
                                    resolve_type_name(
                                        schema_doc,
                                        child,
                                        type_attr,
                                        &validator.target_namespace,
                                    )?
                                } else {
                                    // Check for inline simpleType child
                                    let mut inline_type = None;
                                    for gc in schema_doc.children(child) {
                                        if let Some(NodeKind::Element(ge)) =
                                            schema_doc.node_kind(gc)
                                        {
                                            if ge.name.local_name == "simpleType" {
                                                let td = parse_simple_type(schema_doc, gc)?;
                                                inline_type = Some(TypeRef::Inline(Box::new(td)));
                                            }
                                        }
                                    }
                                    inline_type.unwrap_or(TypeRef::BuiltIn(BuiltInType::String))
                                };
                                let required = attr_elem.get_attribute("use") == Some("required");
                                let default =
                                    attr_elem.get_attribute("default").map(|s| s.to_string());
                                let decl = AttributeDecl {
                                    name: name.to_string(),
                                    namespace: validator.target_namespace.clone(),
                                    type_ref,
                                    required,
                                    default,
                                    prohibited: false,
                                    is_ref: false,
                                    qualified: true,
                                };
                                let key = (validator.target_namespace.clone(), name.to_string());
                                validator.global_attributes.insert(key, decl);
                            }
                        }
                    }
                    _ => {
                        // Ignore other top-level declarations for now
                    }
                }
            }
        }

        // Every document is composed by now at the top level: decide the
        // unprefixed type names read without a default namespace. Nested
        // documents keep them until their including schema is composed.
        if state.depth == 0 {
            resolve_unqualified_type_refs(&mut validator)?;
        }

        // Build substitution group map from element declarations.
        // First, collect direct memberships: member -> head.
        let mut direct_head: HashMap<(Option<String>, String), (Option<String>, String)> =
            HashMap::new();
        for (key, decl) in &validator.elements {
            if let Some(ref sg_head) = decl.substitution_group {
                direct_head.insert(key.clone(), sg_head.clone());
            }
        }
        // Build transitive map: for each element that is a substitution group head,
        // collect all (direct and transitive) members.
        // An element E is a member of head H if:
        //   - E.substitutionGroup == H (direct), or
        //   - E.substitutionGroup == M where M is a member of H (transitive)
        for member_key in direct_head.keys() {
            // Walk up the chain from member to find all heads
            let mut current = member_key.clone();
            let mut chain = vec![member_key.clone()];
            while let Some(head) = direct_head.get(&current) {
                // Add member_key as a member of head
                validator
                    .substitution_groups
                    .entry(head.clone())
                    .or_default()
                    .push(member_key.clone());
                current = head.clone();
                // Prevent infinite loops
                if chain.contains(&current) {
                    break;
                }
                chain.push(current.clone());
            }
        }
        // Deduplicate members
        for members in validator.substitution_groups.values_mut() {
            members.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            members.dedup();
        }
        debug_log!("substitution_groups: {:?}", validator.substitution_groups);

        // Resolution pass: propagate list type info from base types to derived types.
        // Types that restrict a list type inherit is_list and item_type.
        let type_keys: Vec<_> = validator.types.keys().cloned().collect();
        for key in &type_keys {
            let base_key = {
                if let Some(TypeDef::Simple(st)) = validator.types.get(key) {
                    st.named_base_key()
                } else {
                    None
                }
            };
            if let Some(base_key) = base_key {
                // Look up the base type under the namespace its QName resolved to
                let (is_list, item_type) = {
                    if let Some(TypeDef::Simple(base_st)) = validator.types.get(&base_key) {
                        (base_st.is_list, base_st.item_type.clone())
                    } else {
                        (false, None)
                    }
                };
                if is_list {
                    if let Some(TypeDef::Simple(st)) = validator.types.get_mut(key) {
                        st.is_list = true;
                        if st.item_type.is_none() {
                            st.item_type = item_type;
                        }
                    }
                }
            }
        }

        // Resolution pass 2: resolve item type facets for list types whose item type
        // is a user-defined simple type (not a built-in).
        let type_keys2: Vec<_> = validator.types.keys().cloned().collect();
        for key in &type_keys2 {
            let item_key = {
                if let Some(TypeDef::Simple(st)) = validator.types.get(key) {
                    st.named_item_key()
                } else {
                    None
                }
            };
            if let Some(item_key) = item_key {
                // Look up the item type under the namespace its QName resolved to
                let resolved = {
                    if let Some(TypeDef::Simple(item_st)) = validator.types.get(&item_key) {
                        Some((item_st.base.clone(), item_st.facets.clone()))
                    } else {
                        None
                    }
                };
                if let Some((item_base, item_facets)) = resolved {
                    if let Some(TypeDef::Simple(st)) = validator.types.get_mut(key) {
                        st.item_type = Some(item_base);
                        st.item_facets = item_facets;
                    }
                }
            }
        }

        // Resolution pass 2b: derived list types (a `<restriction base="SomeList">`)
        // inherit the item type and item-level facets of the list they restrict.
        // Pass 1 propagates `is_list`/`item_type` but runs before pass 2 resolves
        // the base's item facets, so item-level constraints (e.g. a pattern on a
        // user-defined item type) would otherwise be silently dropped on a derived
        // list type — accepting items the base would reject. Walk each restriction
        // chain to the underlying `<list>` declaration (whose item meta pass 2 has
        // fully resolved) and copy it down.
        let derived_meta = type_keys2
            .iter()
            .filter_map(|key| {
                // Only derived list types: a list reached via restriction, not
                // a direct `<list>` declaration (which pass 2 already handles).
                let is_derived_list = matches!(
                    validator.types.get(key),
                    Some(TypeDef::Simple(st))
                        if st.is_list
                            && st._base_type_local.is_some()
                            && st._item_type_local.is_none()
                );
                if !is_derived_list {
                    return None;
                }
                // Follow the base chain to the root list; its resolved item
                // meta is the deepest list reached.
                let mut cur = key.clone();
                let mut seen = HashSet::new();
                let mut meta = None;
                while seen.insert(cur.clone()) {
                    match validator.types.get(&cur) {
                        Some(TypeDef::Simple(st)) if st.is_list => {
                            meta = Some((st.item_type.clone(), st.item_facets.clone()));
                            match st.named_base_key() {
                                Some(base_key) => cur = base_key,
                                None => break,
                            }
                        }
                        _ => break,
                    }
                }
                meta.map(|(it, ifac)| (key.clone(), it, ifac))
            })
            .collect::<Vec<_>>();
        for (key, item_type, item_facets) in derived_meta {
            if let Some(TypeDef::Simple(st)) = validator.types.get_mut(&key) {
                if item_type.is_some() {
                    st.item_type = item_type;
                }
                st.item_facets = item_facets;
            }
        }

        // Resolution pass 3: resolve item type facets for inline list types embedded in
        // element declarations (both global elements and particles inside complex types).
        // Collect resolved item types from the types map first.
        let resolved_items: HashMap<(Option<String>, String), (BuiltInType, Vec<Facet>)> =
            validator
                .types
                .iter()
                .filter_map(|(k, td)| {
                    if let TypeDef::Simple(st) = td {
                        Some((k.clone(), (st.base.clone(), st.facets.clone())))
                    } else {
                        None
                    }
                })
                .collect();

        // Named simple types that are list types, so an inline type restricting
        // one can inherit `is_list`/`item_type`/`item_facets` (issue #12). Built
        // after the named-type list-resolution passes above, so the entries
        // already carry resolved item types.
        let list_bases: ListBasesMap = validator
            .types
            .iter()
            .filter_map(|(k, td)| match td {
                TypeDef::Simple(st) if st.is_list => {
                    Some((k.clone(), (st.item_type.clone(), st.item_facets.clone())))
                }
                _ => None,
            })
            .collect();

        // Resolve inline list types in global element declarations
        for elem_decl in validator.elements.values_mut() {
            resolve_inline_list_item_facets(
                &mut elem_decl.type_ref,
                &resolved_items,
                &list_bases,
                &validator.target_namespace,
            );
        }

        // Resolve inline list types in complex type content models
        let type_keys3: Vec<_> = validator.types.keys().cloned().collect();
        for key in type_keys3 {
            if let Some(TypeDef::Complex(ct)) = validator.types.get_mut(&key) {
                resolve_content_model_list_item_facets(
                    &mut ct.content,
                    &resolved_items,
                    &list_bases,
                    &validator.target_namespace,
                );
            }
        }

        Ok(validator)
    }
}

/// A type reference left undecided after composition; unreachable, since
/// `resolve_unqualified_type_refs` decides every one, but refused rather
/// than assumed.
fn unresolved_type_name(name: &UnqualifiedTypeName) -> XmlError {
    XmlError::validation(format!(
        "Type name '{}' was not resolved",
        qname_display(&name.absent_ns, &name.local)
    ))
}

/// The value of a range facet that has no place on the timeline of `base`
/// (a date or time type whose year is too far from zero). Such a value is
/// never compared, so it can neither equal nor differ from a fixed value.
fn unplaceable_range_value<'a>(facet: &'a Facet, base: &BuiltInType) -> Option<&'a str> {
    match facet {
        Facet::MinInclusive(v)
        | Facet::MaxInclusive(v)
        | Facet::MinExclusive(v)
        | Facet::MaxExclusive(v) => {
            let v = trim_xml_whitespace(v);
            instant_is_unrepresentable(v, base).then_some(v)
        }
        _ => None,
    }
}

/// Whether two facets of the same kind have the same value; range facets
/// compare in the value space of `base`.
fn facet_values_equal(a: &Facet, b: &Facet, base: &BuiltInType) -> bool {
    match (a, b) {
        (Facet::MinLength(x), Facet::MinLength(y))
        | (Facet::MaxLength(x), Facet::MaxLength(y))
        | (Facet::Length(x), Facet::Length(y))
        | (Facet::TotalDigits(x), Facet::TotalDigits(y))
        | (Facet::FractionDigits(x), Facet::FractionDigits(y)) => x == y,
        (Facet::MinInclusive(x), Facet::MinInclusive(y))
        | (Facet::MaxInclusive(x), Facet::MaxInclusive(y))
        | (Facet::MinExclusive(x), Facet::MinExclusive(y))
        | (Facet::MaxExclusive(x), Facet::MaxExclusive(y)) => {
            values_equal(trim_xml_whitespace(x), base, trim_xml_whitespace(y), base)
        }
        (Facet::WhiteSpace(x), Facet::WhiteSpace(y)) => x == y,
        _ => false,
    }
}
