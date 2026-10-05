//! Schema composition — `xs:include`, `xs:redefine`, and `xs:import`.
//!
//! Handles loading external schema documents referenced by `schemaLocation`
//! attributes, merging their declarations into the main validator, and
//! performing "chameleon include" namespace fixup when a no-namespace schema
//! is included into a target-namespace schema.
//!
//! ## Composition flow
//!
//! 1. **`process_schema_composition`** iterates top-level children of the
//!    `<xs:schema>` element looking for `include`, `redefine`, and `import`.
//! 2. For each, the external schema is loaded from disk, parsed, and built
//!    into a sub-`XsdValidator` via `from_schema_with_base_path`.
//! 3. **`merge_external_declarations`** copies every declaration from the
//!    external validator into the main one.  For a chameleon include, the
//!    `None`-namespace declarations of the included documents themselves are
//!    re-keyed to the main schema's target namespace; the declarations they
//!    reached through `xs:import` are merged unchanged.  A document that
//!    cannot be loaded is recorded, so that names needing it are refused.
//! 4. For `xs:redefine`, **`process_redefine_children`** then processes the
//!    inline redefinition elements (simpleType, complexType, group,
//!    attributeGroup) and replaces the previously-merged declarations.
//! 5. **`reresolve_types_after_redefine`** updates complex types whose
//!    group or attributeGroup references may have changed.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::dom::{Document, NodeId, NodeKind};
use crate::error::{XmlError, XmlResult};

use super::parser::{
    builtin_list_item_type, group_ref_content, parse_attribute_group_def, parse_builtin_type,
    parse_complex_type, parse_model_group_def, parse_simple_type,
};
use super::types::{
    AttributeDecl, ComplexTypeDef, ComponentKey, ContentModel, ElementDecl, Particle, ParticleKind,
    Provenance, SimpleTypeDef, TypeDef, TypeRef, UnloadedDocument, UnqualifiedTypeName,
    XsdValidator,
};
use super::XS_NAMESPACE;

/// Maximum depth of `xs:include` / `xs:redefine` / `xs:import` nesting.
///
/// Real-world schemas rarely nest more than 2-3 levels (`A` imports `B`
/// which imports `C`). 16 gives generous headroom while preventing the
/// stack overflow a circular-include chain would otherwise trigger. Used
/// in combination with a per-build visited-paths set so self-referential
/// cycles short-circuit even earlier.
pub(super) const MAX_INCLUDE_DEPTH: u8 = 16;

/// State carried through recursive schema composition to detect cycles
/// and enforce depth limits.
pub(super) struct CompositionState {
    /// Canonicalized absolute paths that have already been loaded during
    /// this `from_schema_with_base_path` call. Reloads short-circuit so
    /// `a.xsd` including `b.xsd` including `a.xsd` terminates cleanly.
    pub(super) visited: HashSet<PathBuf>,
    /// Current recursion depth. Incremented on each external schema
    /// build; errors out when it reaches [`MAX_INCLUDE_DEPTH`].
    pub(super) depth: u8,
}

struct ResolvedSchemaPath {
    path: PathBuf,
    identity: Option<FileIdentity>,
}

/// What a `schemaLocation` resolves to.
enum SchemaLocation {
    /// A file to load, through `read_resolved_schema`.
    Found(ResolvedSchemaPath),
    /// A file already loaded earlier in this build (a cycle, or a document
    /// reached twice): it is not loaded again. Its components reach this
    /// document only if the first load merges them into a validator this
    /// document's components are composed with, and only in the namespace
    /// context of that first load (an import, or a chameleon include into
    /// one namespace). A document used both ways, or needed by a document
    /// read before the first load is merged, lacks the second copy.
    AlreadyLoaded,
    /// No file: its components are absent.
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<FileIdentity> {
    Some(FileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        ctime: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    })
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> Option<FileIdentity> {
    None
}

impl CompositionState {
    /// Fresh state, seeded with the top-level schema's canonical path so
    /// the very first include won't try to re-load the outer document.
    pub(super) fn new(base_path: Option<&Path>) -> Self {
        let mut visited = HashSet::new();
        if let Some(p) = base_path {
            if let Ok(c) = p.canonicalize() {
                visited.insert(c);
            }
        }
        CompositionState { visited, depth: 0 }
    }
}

/// Resolve a `schemaLocation` attribute to a filesystem path, applying
/// F-10 containment, F-11 cycle detection, and the first half of the
/// F-12 TOCTOU defense.
///
/// Returns:
/// * `Ok(SchemaLocation::Found(resolved))` — load this path through
///   `read_resolved_schema`, not directly. The path has been canonicalized
///   and paired with the file identity observed during resolution so the
///   read side can detect a later symlink swap before trusting bytes from
///   the opened handle.
/// * `Ok(SchemaLocation::Missing)` — skip: the target doesn't exist (matches
///   pre-fix behaviour for relative `schemaLocation` typos). The caller
///   records the document as unloaded.
/// * `Ok(SchemaLocation::AlreadyLoaded)` — skip: the file was already loaded
///   earlier in this build (cycle short-circuit); see
///   `SchemaLocation::AlreadyLoaded` for what the caller then lacks.
/// * `Err(...)` — reject: the target escapes the schema's base directory,
///   or the attribute value is an absolute URI with a scheme we don't
///   support (`http://`, `ftp://`, ...).
fn resolve_include_path(
    schema_location: &str,
    base_dir: Option<&Path>,
    canonical_base: Option<&Path>,
    state: &mut CompositionState,
    kind: &str,
) -> XmlResult<SchemaLocation> {
    let resolved_path = match base_dir {
        Some(dir) => dir.join(schema_location),
        None => PathBuf::from(schema_location),
    };
    let canonical = resolved_path.canonicalize().ok();

    // F-10 containment check. When both canonicalized paths exist we
    // require the resolved path to live under the base directory. When
    // the target canonicalize fails we treat it as missing; the old
    // `is_absolute_uri` error is still surfaced for http/ftp/... values
    // that would never have loaded.
    match (canonical_base, canonical.as_ref()) {
        (Some(cb), Some(c)) if !c.starts_with(cb) => {
            return Err(XmlError::validation(format!(
                "Cannot resolve {} schemaLocation '{}': path escapes the schema's base directory",
                kind, schema_location
            )));
        }
        (Some(_), None) => {
            if is_absolute_uri(schema_location) {
                return Err(XmlError::validation(format!(
                    "Cannot resolve {} schemaLocation '{}': absolute URI not supported",
                    kind, schema_location
                )));
            }
            return Ok(SchemaLocation::Missing);
        }
        _ => {}
    }

    // F-11 cycle detection keyed on the canonical path.
    if let Some(ref c) = canonical {
        if !state.visited.insert(c.clone()) {
            return Ok(SchemaLocation::AlreadyLoaded);
        }
    }

    let path = canonical.unwrap_or(resolved_path);
    let identity = fs::metadata(&path).ok().and_then(|m| file_identity(&m));
    Ok(SchemaLocation::Found(ResolvedSchemaPath { path, identity }))
}

fn read_resolved_schema(
    resolved: &ResolvedSchemaPath,
    schema_location: &str,
    kind: &str,
) -> XmlResult<Option<String>> {
    let mut file = match File::open(&resolved.path) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };

    if let Some(expected) = resolved.identity {
        let actual = file
            .metadata()
            .ok()
            .and_then(|metadata| file_identity(&metadata));
        if actual != Some(expected) {
            return Err(XmlError::validation(format!(
                "Cannot resolve {} schemaLocation '{}': file changed during resolution",
                kind, schema_location
            )));
        }
    }

    let mut contents = String::new();
    match file.read_to_string(&mut contents) {
        Ok(_) => Ok(Some(contents)),
        Err(_) => Ok(None),
    }
}

/// Process `xs:include`, `xs:redefine`, and `xs:import` elements in a schema
/// document, loading external schemas and merging their declarations into the
/// validator.
///
/// Called during pass 0 of `from_schema_with_base_path` (only when a base path
/// is available for resolving relative `schemaLocation` URIs).
///
/// `state` carries the visited-paths set and depth counter so circular
/// includes terminate cleanly and pathological chains cannot stack-overflow.
pub(super) fn process_schema_composition(
    schema_doc: &Document,
    schema_elem: NodeId,
    validator: &mut XsdValidator,
    base_path: Option<&Path>,
    state: &mut CompositionState,
) -> XmlResult<()> {
    if state.depth >= MAX_INCLUDE_DEPTH {
        return Err(XmlError::validation(format!(
            "Schema include/import/redefine nesting exceeds maximum depth of {}",
            MAX_INCLUDE_DEPTH
        )));
    }

    // `base_path` is either the schema *directory* or the schema *file*:
    //   * the public entry (`from_file`, and the etree `XMLSchema(file=...)`
    //     facade, which passes `os.path.dirname(file)`) supplies a directory,
    //     since the top-level schema is handed in as a string with no file of
    //     its own;
    //   * recursive `xs:import`/`xs:include`/`xs:redefine` loads pass the
    //     resolved schema *file* path (see the `from_schema_with_composition_state`
    //     calls below).
    // `schemaLocation` is resolved relative to the directory in both cases.
    //
    // Only a path that is *known to be a regular file* (the recursive loads) is
    // reduced to its parent directory. A directory, or any path that cannot be
    // stat'd (missing or unreadable), is kept as the base directory itself. This
    // matters for the fail-closed contract below: a bad base directory must
    // still fail to canonicalize and be rejected, rather than silently resolving
    // against its parent -- which a `!is_dir()` test would do, since `is_dir()`
    // also returns false for a missing/unreadable directory.
    //
    // (The previous unconditional `.parent()` treated the public entry's
    // directory as a file and stripped one level, so every import/include
    // silently failed to resolve and all imported declarations -- types *and*
    // elements -- went missing, surfacing as "No element declaration found" /
    // "Type not found".)
    let base_dir: Option<PathBuf> = base_path.map(|p| {
        if p.is_file() {
            // A file always has a parent; an empty parent denotes the current
            // directory, so fall back to "." rather than the file path itself
            // (joining onto the file would yield e.g. `schema.xsd/inner.xsd`).
            match p.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                _ => PathBuf::from("."),
            }
        } else {
            p.to_path_buf()
        }
    });
    // Canonicalize the base once per call; reused as the containment
    // anchor for every schemaLocation resolved in the loop below.
    //
    // Fail closed when the caller supplied a base directory we cannot
    // canonicalize (permission denied, missing, race with `rm -rf`,
    // etc.). Falling through to `canonical_base = None` would skip the
    // containment check inside `resolve_include_path` and re-open the
    // arbitrary-file-read window F-10 closed.
    let canonical_base = match &base_dir {
        Some(b) => Some(b.canonicalize().map_err(|e| {
            XmlError::validation(format!(
                "Failed to canonicalize schema base directory '{}': {}",
                b.display(),
                e
            ))
        })?),
        None => None,
    };

    for child in schema_doc.children(schema_elem) {
        if let Some(NodeKind::Element(elem)) = schema_doc.node_kind(child) {
            let is_xs = elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                || elem.name.prefix.as_deref() == Some("xs")
                || elem.name.prefix.as_deref() == Some("xsd");
            if !is_xs {
                continue;
            }

            match elem.name.local_name.as_ref() {
                "include" | "redefine" => {
                    let is_redefine = elem.name.local_name == "redefine";
                    let schema_location = match elem.get_attribute("schemaLocation") {
                        Some(loc) => loc,
                        None => continue, // No schemaLocation, skip
                    };

                    // Resolve the schemaLocation to a contained canonical path
                    // and remember the resolved file identity. The subsequent
                    // read verifies the opened handle still has that identity;
                    // the race is closed by the resolve/read pair, not by this
                    // path helper alone.
                    let kind = if is_redefine { "redefine" } else { "include" };
                    // A document that is not loaded leaves its components
                    // absent; the build refuses names that would need them.
                    let unloaded = UnloadedDocument {
                        namespace: validator.target_namespace.clone(),
                        location: schema_location.to_string(),
                        by_import: false,
                    };
                    let resolved_schema = match resolve_include_path(
                        schema_location,
                        base_dir.as_deref(),
                        canonical_base.as_deref(),
                        state,
                        kind,
                    )? {
                        SchemaLocation::Found(p) => p,
                        SchemaLocation::AlreadyLoaded => continue,
                        SchemaLocation::Missing => {
                            validator.unloaded_documents.push(unloaded);
                            continue;
                        }
                    };

                    // Load through the resolved descriptor. Canonicalization
                    // proves the path was contained at check time; on Unix the
                    // stored file identity also proves the opened handle is the
                    // same file, closing the symlink-swap race. Other platforms
                    // retain canonical containment but need platform handle APIs
                    // for the same post-open identity guarantee.
                    let ext_str =
                        match read_resolved_schema(&resolved_schema, schema_location, kind)? {
                            Some(s) => s,
                            None => {
                                validator.unloaded_documents.push(unloaded);
                                continue;
                            }
                        };
                    let ext_doc = match crate::parse(&ext_str) {
                        Ok(d) => d,
                        Err(_) => {
                            validator.unloaded_documents.push(unloaded);
                            continue;
                        }
                    };

                    // Build a sub-validator from the external schema,
                    // propagating the visited set and incrementing depth.
                    // Decrement on every exit path (Ok or Err) so a
                    // failure deep in the include tree cannot leave
                    // `state.depth` desynced for any later sibling
                    // includes.
                    state.depth += 1;
                    let ext_validator_res = XsdValidator::from_schema_with_composition_state(
                        &ext_doc,
                        Some(&resolved_schema.path),
                        state,
                    );
                    state.depth -= 1;
                    let ext_validator = ext_validator_res?;

                    // Determine the effective namespace for included declarations.
                    // "Chameleon include": if the external schema has no targetNamespace
                    // but the including schema does, the included declarations adopt
                    // the including schema's targetNamespace.
                    let chameleon = ext_validator.target_namespace.is_none()
                        && validator.target_namespace.is_some();

                    // Merge declarations from external schema into our validator
                    merge_external_declarations(
                        validator,
                        &ext_validator,
                        Merge::Include { chameleon },
                    );

                    // For xs:redefine, process inline redefinition children
                    if is_redefine {
                        process_redefine_children(schema_doc, child, validator)?;
                    }
                }
                // xs:import — load an external schema with a different targetNamespace.
                // Unlike xs:include, no chameleon fixup is needed: the imported schema
                // keeps its own targetNamespace and its declarations are merged as-is.
                // (Sun tests: xsd004)
                "import" => {
                    let schema_location = match elem.get_attribute("schemaLocation") {
                        Some(loc) => loc,
                        None => continue, // No schemaLocation, skip (namespace-only import)
                    };

                    // `xs:import/@schemaLocation` is only a *hint* (XSD 1.0 Part 1
                    // §4.2.3): a processor may ignore it and is not obliged to
                    // resolve it. Unlike `xs:include`/`xs:redefine` (which still treat
                    // absolute-URI schemes and base-directory escapes as hard errors),
                    // an import whose location cannot be resolved — unsupported schemes,
                    // a missing file, or a path outside the base directory — is skipped
                    // rather than aborting the whole build. This matches
                    // libxml2/Xerces and is what lets composite schemas (e.g.
                    // pyFF's `schema.xsd`) build: their imported schemas carry
                    // redundant absolute/classpath import hints for namespaces
                    // already supplied by a sibling, resolvable import.
                    //
                    // Only an *unresolvable* location is skipped. Once the hint
                    // resolves to a real, readable file the imported schema is
                    // genuinely present, so a malformed (non-well-formed) or
                    // semantically broken target is a real error and is surfaced,
                    // not silently dropped.
                    //
                    // A skipped document's components are absent, as the
                    // specification has it: the build refuses a type name
                    // that would need them (`resolve_unqualified_type_refs`),
                    // so a skipped hint cannot turn such a name into another
                    // type. An import nothing refers to stays harmless.
                    let unloaded = UnloadedDocument {
                        namespace: elem.get_attribute("namespace").map(str::to_string),
                        location: schema_location.to_string(),
                        by_import: true,
                    };
                    let resolved_schema = match resolve_include_path(
                        schema_location,
                        base_dir.as_deref(),
                        canonical_base.as_deref(),
                        state,
                        "import",
                    ) {
                        Ok(SchemaLocation::Found(p)) => p,
                        Ok(SchemaLocation::AlreadyLoaded) => continue,
                        Ok(SchemaLocation::Missing) | Err(_) => {
                            validator.unloaded_documents.push(unloaded);
                            continue;
                        }
                    };

                    // Load the external schema, verifying after open that the
                    // handle still matches the resolved file identity where the
                    // platform exposes one through std. A post-resolution open or
                    // read failure (`Ok(None)` — the file vanished or became
                    // unreadable after resolving) is treated as the hint failing
                    // to resolve and is skipped, consistent with the hint
                    // semantics above; only a file-identity mismatch is surfaced
                    // (the `?`), since that signals a TOCTOU swap rather than an
                    // absent hint.
                    let ext_str =
                        match read_resolved_schema(&resolved_schema, schema_location, "import")? {
                            Some(s) => s,
                            None => {
                                validator.unloaded_documents.push(unloaded);
                                continue;
                            }
                        };
                    // The file resolved and its bytes were read: a parse failure
                    // is a real broken-schema error, surfaced with context rather
                    // than skipped.
                    let ext_doc = crate::parse(&ext_str).map_err(|e| {
                        XmlError::validation(format!(
                            "imported schema '{schema_location}' (resolved to {}) is not well-formed: {e}",
                            resolved_schema.path.display()
                        ))
                    })?;

                    // Build a sub-validator from the external schema.
                    // Same balanced-decrement pattern as the include /
                    // redefine branch above: decrement runs on Ok and
                    // Err alike, so a failure inside the import chain
                    // cannot desync `state.depth` for sibling imports.
                    state.depth += 1;
                    let ext_validator_res = XsdValidator::from_schema_with_composition_state(
                        &ext_doc,
                        Some(&resolved_schema.path),
                        state,
                    );
                    state.depth -= 1;
                    let ext_validator = ext_validator_res?;

                    // Import never uses chameleon fixup — the imported schema
                    // has its own targetNamespace which is preserved as-is.
                    merge_external_declarations(validator, &ext_validator, Merge::Import);
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// How an external schema document's declarations are merged.
#[derive(Clone, Copy)]
enum Merge {
    /// `xs:include` / `xs:redefine`. With `chameleon`, the included
    /// no-namespace declarations take the including target namespace.
    Include { chameleon: bool },
    /// `xs:import`: every declaration keeps its namespace.
    Import,
}

/// Merge declarations from an external schema validator into the main validator.
///
/// A chameleon include (XSD 1.0 Part 1 §4.2.1) re-keys the `None`-namespace
/// declarations of the included documents to the main validator's target
/// namespace and moves their no-namespace references with them. It changes
/// only those documents' own components: the components they reached through
/// `xs:import` keep their namespaces and their references, since an imported
/// document keeps its own target namespace, and its unprefixed names without
/// a default namespace are `{absent}local`.
fn merge_external_declarations(validator: &mut XsdValidator, ext: &XsdValidator, how: Merge) {
    let target_ns = validator.target_namespace.clone();
    let by_import = matches!(how, Merge::Import);
    let chameleon_ns = match how {
        Merge::Include { chameleon: true } => Some(&target_ns),
        _ => None,
    };

    merge_components(
        &mut validator.elements,
        &mut validator.imported.elements,
        &ext.elements,
        &ext.imported.elements,
        by_import,
        chameleon_ns,
        // Also re-namespaces elements inside content models.
        chameleon_fixup_element_decl,
    );
    merge_components(
        &mut validator.types,
        &mut validator.imported.types,
        &ext.types,
        &ext.imported.types,
        by_import,
        chameleon_ns,
        chameleon_fixup_type_def,
    );
    merge_components(
        &mut validator.global_attributes,
        &mut validator.imported.global_attributes,
        &ext.global_attributes,
        &ext.imported.global_attributes,
        by_import,
        chameleon_ns,
        |attr, target_ns| {
            // Global attributes take on the including schema's target
            // namespace, matching their re-keyed lookup entry.
            if attr.namespace.is_none() {
                attr.namespace = target_ns.clone();
            }
            chameleon_fixup_type_ref(&mut attr.type_ref, target_ns);
        },
    );
    merge_components(
        &mut validator.attribute_groups,
        &mut validator.imported.attribute_groups,
        &ext.attribute_groups,
        &ext.imported.attribute_groups,
        by_import,
        chameleon_ns,
        |group, target_ns| chameleon_fixup_attribute_decls(&mut group.attributes, target_ns),
    );
    merge_components(
        &mut validator.model_groups,
        &mut validator.imported.model_groups,
        &ext.model_groups,
        &ext.imported.model_groups,
        by_import,
        chameleon_ns,
        |group, target_ns| chameleon_fixup_content_model(&mut group.content, target_ns),
    );

    for unloaded in &ext.unloaded_documents {
        let mut unloaded = unloaded.clone();
        if by_import {
            unloaded.by_import = true;
        } else if let Some(target_ns) = chameleon_ns {
            if !unloaded.by_import && unloaded.namespace.is_none() {
                unloaded.namespace = target_ns.clone();
            }
        }
        validator.unloaded_documents.push(unloaded);
    }
}

/// Merge one kind of component for `merge_external_declarations`. A
/// component the external schema reached by import (or every component,
/// `by_import`) is merged as it is; with `chameleon_ns`, the others are
/// re-keyed and fixed up.
fn merge_components<V: Clone>(
    map: &mut HashMap<ComponentKey, V>,
    provenance: &mut Provenance<V>,
    ext_map: &HashMap<ComponentKey, V>,
    ext_provenance: &Provenance<V>,
    by_import: bool,
    chameleon_ns: Option<&Option<String>>,
    fixup: impl Fn(&mut V, &Option<String>),
) {
    for (key, value) in ext_map {
        let imported = by_import || ext_provenance.imported.contains(key);
        let mut key = key.clone();
        let mut value = value.clone();
        if let (Some(target_ns), false) = (chameleon_ns, imported) {
            if key.0.is_none() {
                key.0 = target_ns.clone();
            }
            fixup(&mut value, target_ns);
        }
        merge_component(map, provenance, key, value, imported);
    }
    for (key, value) in &ext_provenance.displaced {
        merge_component(map, provenance, key.clone(), value.clone(), true);
    }
}

/// Insert a component merged from another schema document; `by_import`
/// says whether it arrived through `xs:import`. Of two components with one
/// key the first stays, as before, except for a no-namespace name held by
/// one of the schema's own components and an imported one: the own one
/// takes the key and the imported one is kept as displaced, so that a
/// chameleon include of this schema can separate them again.
fn merge_component<V>(
    map: &mut HashMap<ComponentKey, V>,
    provenance: &mut Provenance<V>,
    key: ComponentKey,
    value: V,
    by_import: bool,
) {
    let held_by_import = provenance.imported.contains(&key);
    match map.entry(key) {
        Entry::Vacant(slot) => {
            if by_import {
                provenance.imported.insert(slot.key().clone());
            }
            slot.insert(value);
        }
        Entry::Occupied(mut slot) => {
            if slot.key().0.is_some() || by_import == held_by_import {
                return;
            }
            let key = slot.key().clone();
            if by_import {
                provenance.displaced.push((key, value));
            } else {
                let old = slot.insert(value);
                provenance.imported.remove(&key);
                provenance.displaced.push((key, old));
            }
        }
    }
}

/// Declare one of a schema document's own components. It replaces an
/// earlier component with the same key, as before; an imported one is kept
/// as displaced (see `merge_component`).
pub(super) fn declare_own<V>(
    map: &mut HashMap<ComponentKey, V>,
    provenance: &mut Provenance<V>,
    key: ComponentKey,
    value: V,
) {
    let was_imported = provenance.imported.remove(&key);
    if let Some(old) = map.insert(key.clone(), value) {
        if was_imported {
            provenance.displaced.push((key, old));
        }
    }
}

/// Fix up an element declaration's namespace for chameleon include:
/// Set the element's namespace and recursively fix up inline type defs.
fn chameleon_fixup_element_decl(decl: &mut ElementDecl, target_ns: &Option<String>) {
    if decl.namespace.is_none() {
        decl.namespace = target_ns.clone();
    }
    chameleon_fixup_type_ref(&mut decl.type_ref, target_ns);
}

/// Fix up a type reference for chameleon include.
/// Named references with `None` namespace are re-pointed to the target namespace.
fn chameleon_fixup_type_ref(type_ref: &mut TypeRef, target_ns: &Option<String>) {
    match type_ref {
        TypeRef::Named(ref mut ns, _) => {
            if ns.is_none() {
                *ns = target_ns.clone();
            }
        }
        TypeRef::Inline(ref mut td) => {
            chameleon_fixup_type_def(td, target_ns);
        }
        TypeRef::Unqualified(ref mut name) => chameleon_fixup_unqualified(name, target_ns),
        TypeRef::BuiltIn(_) => {}
    }
}

/// An unprefixed name of a no-namespace module: `{absent}local` becomes a
/// name in the including schema's target namespace.
fn chameleon_fixup_unqualified(name: &mut UnqualifiedTypeName, target_ns: &Option<String>) {
    if name.absent_ns.is_none() {
        name.absent_ns = target_ns.clone();
    }
    if name.target_ns.is_none() {
        name.target_ns = target_ns.clone();
    }
}

/// Fix up a type definition for chameleon include.
/// For complex types, fixes the `base_type` reference, the attribute uses,
/// and recurses into the content model.
fn chameleon_fixup_type_def(td: &mut TypeDef, target_ns: &Option<String>) {
    match td {
        TypeDef::Complex(ref mut ct) => {
            // Fix base_type reference
            if let Some((ref mut ns, _)) = ct.base_type {
                if ns.is_none() {
                    *ns = target_ns.clone();
                }
            }
            if let Some(name) = ct.unqualified_base.as_mut() {
                chameleon_fixup_unqualified(name, target_ns);
            }
            chameleon_fixup_attribute_decls(&mut ct.attributes, target_ns);
            chameleon_fixup_content_model(&mut ct.content, target_ns);
            if let Some(restriction) = ct.simple_content_restriction.as_mut() {
                if let Some(ref mut base) = restriction.base_ref {
                    chameleon_fixup_type_ref(base, target_ns);
                }
            }
        }
        TypeDef::Simple(ref mut st) => {
            // Unprefixed base, item and member type names of a no-namespace
            // module move to the including schema's target namespace too.
            if let Some(ref mut base) = st.base_ref {
                chameleon_fixup_type_ref(base, target_ns);
            }
            if let Some(ref mut item) = st.item_ref {
                chameleon_fixup_type_ref(item, target_ns);
            }
            for member in st.union_members.iter_mut().flatten() {
                chameleon_fixup_type_ref(member, target_ns);
            }
        }
    }
}

/// Fix up attribute uses for chameleon include: references to the module's
/// global attributes follow those globals into the including schema's target
/// namespace, and local uses qualified via `form`/`attributeFormDefault`
/// (whose namespace was None because the module had no targetNamespace) take
/// the target namespace as well. Local unqualified declarations stay in no
/// namespace.
fn chameleon_fixup_attribute_decls(attributes: &mut [AttributeDecl], target_ns: &Option<String>) {
    for attr in attributes {
        if (attr.is_ref || attr.qualified) && attr.namespace.is_none() {
            attr.namespace = target_ns.clone();
        }
        // Unprefixed type names of a no-namespace module move as well.
        chameleon_fixup_type_ref(&mut attr.type_ref, target_ns);
    }
}

/// Fix up a content model for chameleon include.
/// Recurses into sequences, choices, all groups, and simple content.
fn chameleon_fixup_content_model(content: &mut ContentModel, target_ns: &Option<String>) {
    match content {
        ContentModel::Sequence(ref mut particles, _, _)
        | ContentModel::Choice(ref mut particles, _, _) => {
            chameleon_fixup_particles(particles, target_ns);
        }
        ContentModel::All(ref mut particles) => {
            chameleon_fixup_particles(particles, target_ns);
        }
        ContentModel::SimpleContent(ref mut type_ref) => {
            chameleon_fixup_type_ref(type_ref, target_ns);
        }
        _ => {}
    }
}

/// Fix up particles for chameleon include.
/// Recurses into element declarations and nested sequence/choice particles.
fn chameleon_fixup_particles(particles: &mut [Particle], target_ns: &Option<String>) {
    for particle in particles {
        match &mut particle.kind {
            ParticleKind::Element(ref mut decl) => {
                chameleon_fixup_element_decl(decl, target_ns);
            }
            ParticleKind::Sequence(ref mut sub) | ParticleKind::Choice(ref mut sub) => {
                chameleon_fixup_particles(sub, target_ns);
            }
            ParticleKind::Any { .. } => {}
        }
    }
}

/// Decide every unprefixed type QName read without a default namespace
/// (`TypeRef::Unqualified`), once the schema is composed: every included,
/// redefined and imported document has been merged, and chameleon includes
/// have moved their names. In order:
///
/// 1. `{absent}local` (XSD: an unprefixed QName with no default namespace
///    has no namespace), when the composed schema defines that type. After
///    a chameleon include this is the including target namespace.
/// 2. Otherwise, as legacy leniencies for schemas that are invalid under
///    rule 1 (src-resolve): the type `local` of the reading document's target
///    namespace, when the composed schema defines it; else the built-in type
///    of that name.
/// 3. Otherwise `{target namespace}local`, which does not exist and is
///    reported as a missing type.
///
/// Rule 1 is what the XSD specification and libxml2 do; rule 2 keeps
/// schemas that reference their own target namespace types, or built-in
/// types, without a prefix building as before. The decision looks names up
/// in the set of defined types, so it is linear in the number of references.
///
/// A namespace one of whose schema documents was not loaded (a missing file
/// or a skipped import hint) has absent components. A name that rule 1 or
/// rule 2 would look for in such a namespace and does not find is refused
/// (src-resolve): the missing document may define it, so no fallback may
/// stand in for it.
pub(super) fn resolve_unqualified_type_refs(validator: &mut XsdValidator) -> XmlResult<()> {
    let defined: HashSet<(Option<String>, String)> = validator.types.keys().cloned().collect();
    let mut unloaded: HashMap<Option<String>, String> = HashMap::new();
    for document in &validator.unloaded_documents {
        unloaded
            .entry(document.namespace.clone())
            .or_insert_with(|| document.location.clone());
    }
    let refused: std::cell::RefCell<Option<XmlError>> = std::cell::RefCell::new(None);
    let refuse = |name: &UnqualifiedTypeName, namespace: &Option<String>, location: &str| {
        let mut refused = refused.borrow_mut();
        if refused.is_none() {
            let namespace = match namespace {
                Some(ns) => format!("namespace '{}'", ns),
                None => "no namespace".to_string(),
            };
            *refused = Some(XmlError::validation(format!(
                "Type '{}' is not defined in {} (src-resolve): the schema document \
                 '{}' for that namespace could not be loaded",
                name.local, namespace, location
            )));
        }
    };
    let resolve = |name: &UnqualifiedTypeName| -> TypeRef {
        let absent = (name.absent_ns.clone(), name.local.clone());
        if defined.contains(&absent) {
            return TypeRef::Named(absent.0, absent.1);
        }
        let in_target = (name.target_ns.clone(), name.local.clone());
        if let Some(location) = unloaded.get(&name.absent_ns) {
            refuse(name, &name.absent_ns, location);
            return TypeRef::Named(absent.0, absent.1);
        }
        if defined.contains(&in_target) {
            return TypeRef::Named(in_target.0, in_target.1);
        }
        if let Some(location) = unloaded.get(&name.target_ns) {
            refuse(name, &name.target_ns, location);
            return TypeRef::Named(in_target.0, in_target.1);
        }
        match parse_builtin_type(&name.local) {
            Some(bt) => TypeRef::BuiltIn(bt),
            None => TypeRef::Named(in_target.0, in_target.1),
        }
    };
    walk_unqualified_type_refs(validator, &resolve);
    match refused.into_inner() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Replace every `TypeRef::Unqualified` of the validator's components with
/// `resolve`'s decision.
fn walk_unqualified_type_refs(
    validator: &mut XsdValidator,
    resolve: &dyn Fn(&UnqualifiedTypeName) -> TypeRef,
) {
    let resolver = UnqualifiedResolver { resolve };
    for td in validator.types.values_mut() {
        resolver.type_def(td);
    }
    for decl in validator.elements.values_mut() {
        resolver.type_ref(&mut decl.type_ref);
    }
    for decl in validator.global_attributes.values_mut() {
        resolver.type_ref(&mut decl.type_ref);
    }
    for group in validator.attribute_groups.values_mut() {
        resolver.attribute_decls(&mut group.attributes);
    }
    for group in validator.model_groups.values_mut() {
        resolver.content_model(&mut group.content);
    }
}

/// Walks every type reference of a schema component for
/// `resolve_unqualified_type_refs`.
struct UnqualifiedResolver<'a> {
    resolve: &'a dyn Fn(&UnqualifiedTypeName) -> TypeRef,
}

impl UnqualifiedResolver<'_> {
    fn type_ref(&self, type_ref: &mut TypeRef) {
        match type_ref {
            TypeRef::Unqualified(name) => *type_ref = (self.resolve)(name),
            TypeRef::Inline(td) => self.type_def(td),
            TypeRef::Named(..) | TypeRef::BuiltIn(_) => {}
        }
    }

    fn type_def(&self, td: &mut TypeDef) {
        match td {
            TypeDef::Simple(st) => self.simple_type(st),
            TypeDef::Complex(ct) => self.complex_type(ct),
        }
    }

    /// A simple type's base, item and member types. A base or item type
    /// that turns out to be built-in is stored the way the parser stores a
    /// built-in one (`base` / `item_type`, no reference).
    fn simple_type(&self, st: &mut SimpleTypeDef) {
        if let Some(TypeRef::Unqualified(name)) = &st.base_ref {
            match (self.resolve)(name) {
                TypeRef::BuiltIn(bt) => {
                    if let Some(item) = builtin_list_item_type(&bt) {
                        st.item_type = Some(item);
                        st.is_list = true;
                    }
                    st.base = bt;
                    st.base_ref = None;
                    st._base_type_local = None;
                }
                other => st.base_ref = Some(other),
            }
        } else if let Some(TypeRef::Inline(inner)) = st.base_ref.as_mut() {
            self.type_def(inner);
            // A restriction of an anonymous simple type copies these from
            // it at parse time; copy them again now that it is decided.
            if let TypeDef::Simple(inner) = inner.as_ref() {
                st.base = inner.base.clone();
                st.is_list = inner.is_list;
                st.item_type = inner.item_type.clone();
                st._item_type_local = inner._item_type_local.clone();
            }
        }
        if let Some(TypeRef::Unqualified(name)) = &st.item_ref {
            match (self.resolve)(name) {
                TypeRef::BuiltIn(bt) => {
                    st.item_type = Some(bt);
                    st.item_ref = None;
                    st._item_type_local = None;
                }
                other => st.item_ref = Some(other),
            }
        } else if let Some(item) = st.item_ref.as_mut() {
            self.type_ref(item);
        }
        for member in st.union_members.iter_mut().flatten() {
            self.type_ref(member);
        }
    }

    fn complex_type(&self, ct: &mut ComplexTypeDef) {
        if let Some(name) = ct.unqualified_base.take() {
            ct.base_type = match (self.resolve)(&name) {
                TypeRef::Named(ns, local) => Some((ns, local)),
                _ => None,
            };
        }
        self.attribute_decls(&mut ct.attributes);
        self.content_model(&mut ct.content);
        if let Some(restriction) = ct.simple_content_restriction.as_mut() {
            // A synthetic step: only its anonymous base can hold references.
            if let Some(base) = restriction.base_ref.as_mut() {
                self.type_ref(base);
            }
        }
    }

    fn attribute_decls(&self, attributes: &mut [AttributeDecl]) {
        for attr in attributes {
            self.type_ref(&mut attr.type_ref);
        }
    }

    fn content_model(&self, content: &mut ContentModel) {
        match content {
            ContentModel::Sequence(particles, _, _)
            | ContentModel::Choice(particles, _, _)
            | ContentModel::All(particles) => self.particles(particles),
            ContentModel::SimpleContent(type_ref) => self.type_ref(type_ref),
            ContentModel::Empty | ContentModel::Any => {}
        }
    }

    fn particles(&self, particles: &mut [Particle]) {
        for particle in particles {
            match &mut particle.kind {
                ParticleKind::Element(decl) => self.type_ref(&mut decl.type_ref),
                ParticleKind::Sequence(sub) | ParticleKind::Choice(sub) => self.particles(sub),
                ParticleKind::Any { .. } => {}
            }
        }
    }
}

/// A type key, not yet used, under which a redefinition keeps the definition
/// it redefines. A redefined schema can itself redefine the same type, so the
/// key gets a numeric suffix when `__redefine_base_<name>` is already taken;
/// reusing it would make the saved definition its own base.
fn unused_redefine_key(
    validator: &XsdValidator,
    target_ns: &Option<String>,
    name: &str,
) -> (Option<String>, String) {
    let mut key = (target_ns.clone(), format!("__redefine_base_{}", name));
    let mut level = 1usize;
    while validator.types.contains_key(&key) {
        level += 1;
        key.1 = format!("__redefine_base_{}_{}", name, level);
    }
    key
}

/// Process inline redefinition children within an `xs:redefine` element.
///
/// Handles `simpleType`, `complexType`, `group`, and `attributeGroup` redefinitions.
/// For complex types with self-referencing base types (the common redefine pattern),
/// the old definition is saved under a `__redefine_base_` prefixed key and the new
/// definition's `base_type` is updated to point to it.
fn process_redefine_children(
    doc: &Document,
    redefine_node: NodeId,
    validator: &mut XsdValidator,
) -> XmlResult<()> {
    let target_ns = validator.target_namespace.clone();

    for child in doc.children(redefine_node) {
        if let Some(NodeKind::Element(child_elem)) = doc.node_kind(child) {
            let is_xs = child_elem.name.namespace_uri.as_deref() == Some(XS_NAMESPACE)
                || child_elem.name.prefix.as_deref() == Some("xs")
                || child_elem.name.prefix.as_deref() == Some("xsd");
            if !is_xs {
                continue;
            }

            match child_elem.name.local_name.as_ref() {
                "simpleType" => {
                    let mut type_def = parse_simple_type(doc, child)?;
                    if let TypeDef::Simple(ref mut st) = type_def {
                        if let Some(name) = st.name.clone() {
                            let key = (target_ns.clone(), name.clone());
                            // A redefinition restricts the original of the same
                            // name: keep the old definition under a unique key
                            // and point the new one's base at it, as for complex
                            // types below.
                            // An unprefixed base without a default namespace
                            // is read in the target namespace here, as before
                            // composition decides such names.
                            let self_reference = st.named_base_key().as_ref() == Some(&key)
                                || matches!(&st.base_ref, Some(TypeRef::Unqualified(base))
                                    if base.local == name && base.target_ns == target_ns);
                            if self_reference {
                                let old_key = unused_redefine_key(validator, &target_ns, &name);
                                if let Some(old_td) = validator.types.get(&key).cloned() {
                                    validator.types.insert(old_key.clone(), old_td);
                                }
                                st.base_ref = Some(TypeRef::Named(old_key.0, old_key.1.clone()));
                                st._base_type_local = Some(old_key.1);
                            }
                            declare_own(
                                &mut validator.types,
                                &mut validator.imported.types,
                                key,
                                type_def,
                            );
                        }
                    }
                }
                "complexType" => {
                    // For redefine, self-references (base="X" where X is the name
                    // being redefined) should resolve to the OLD definition.
                    // We rename the old definition to a unique key and update the
                    // new definition's base_type to reference the renamed key.
                    let local_elem_ns = target_ns.clone(); // qualified by default in redefined types
                    let type_def = parse_complex_type(
                        doc,
                        child,
                        &local_elem_ns,
                        &target_ns,
                        &target_ns,
                        &validator.attribute_groups,
                        &validator.model_groups,
                        validator.block_default_extension,
                        validator.block_default_restriction,
                    )?;
                    if let TypeDef::Complex(ref ct) = type_def {
                        if let Some(name) = &ct.name {
                            let key = (target_ns.clone(), name.clone());
                            // If the base_type references itself (same name), it's a
                            // self-referencing redefine: save old def under a unique key.
                            if let Some(ref base) = ct.base_type {
                                if base.1 == *name && base.0 == target_ns {
                                    let old_key = unused_redefine_key(validator, &target_ns, name);
                                    if let Some(old_td) = validator.types.get(&key).cloned() {
                                        validator.types.insert(old_key.clone(), old_td);
                                    }
                                    // Update the new definition's base_type to point to the renamed old def
                                    let mut new_td = type_def.clone();
                                    if let TypeDef::Complex(ref mut new_ct) = new_td {
                                        new_ct.base_type =
                                            Some((old_key.0.clone(), old_key.1.clone()));
                                        new_ct.unqualified_base = None;
                                        // The value reference must follow the same
                                        // original definition as the derivation link.
                                        // An unprefixed base is still deferred here;
                                        // it matches under the same target-namespace
                                        // reading that made `base_type` a self-reference.
                                        if let ContentModel::SimpleContent(value_ref) =
                                            &mut new_ct.content
                                        {
                                            let self_ref = match value_ref.as_ref() {
                                                TypeRef::Named(ns, name) => {
                                                    ns == &base.0 && name == &base.1
                                                }
                                                TypeRef::Unqualified(u) => {
                                                    u.target_ns == base.0 && u.local == base.1
                                                }
                                                _ => false,
                                            };
                                            if self_ref {
                                                **value_ref = TypeRef::Named(
                                                    old_key.0.clone(),
                                                    old_key.1.clone(),
                                                );
                                            }
                                        }
                                    }
                                    declare_own(
                                        &mut validator.types,
                                        &mut validator.imported.types,
                                        key,
                                        new_td,
                                    );
                                } else {
                                    declare_own(
                                        &mut validator.types,
                                        &mut validator.imported.types,
                                        key,
                                        type_def,
                                    );
                                }
                            } else {
                                declare_own(
                                    &mut validator.types,
                                    &mut validator.imported.types,
                                    key,
                                    type_def,
                                );
                            }
                        }
                    }
                }
                "group" => {
                    // Redefine a model group: the self-reference inside should
                    // resolve to the OLD group definition.
                    if let Some(g_elem) = doc.element(child) {
                        if let Some(name) = g_elem.get_attribute("name") {
                            // Save the old definition before overwriting
                            let key = (target_ns.clone(), name.to_string());
                            let old_mg = validator.model_groups.get(&key).cloned();

                            // Parse with a temporary model_groups that has the old
                            // definition available for self-reference resolution.
                            // (The current model_groups already has it from the merge.)
                            let local_elem_ns = target_ns.clone();
                            let mg_def = parse_model_group_def(
                                doc,
                                child,
                                &local_elem_ns,
                                &target_ns,
                                &validator.attribute_groups,
                                &validator.model_groups,
                                validator.block_default_extension,
                                validator.block_default_restriction,
                            )?;
                            let _ = old_mg; // suppress unused warning
                            declare_own(
                                &mut validator.model_groups,
                                &mut validator.imported.model_groups,
                                key,
                                mg_def,
                            );
                        }
                    }
                }
                "attributeGroup" => {
                    if let Some(ag_elem) = doc.element(child) {
                        if let Some(name) = ag_elem.get_attribute("name") {
                            let ag_def = parse_attribute_group_def(
                                doc,
                                child,
                                &target_ns,
                                &validator.global_attributes,
                                &validator.attribute_groups,
                            )?;
                            let key = (target_ns.clone(), name.to_string());
                            declare_own(
                                &mut validator.attribute_groups,
                                &mut validator.imported.attribute_groups,
                                key,
                                ag_def,
                            );
                        }
                    }
                }
                _ => {} // annotation, etc.
            }
        }
    }

    // After all redefine children are processed, re-resolve complex types
    // that reference the (possibly updated) model groups and attribute groups.
    reresolve_types_after_redefine(validator);

    Ok(())
}

/// After `xs:redefine` processing, re-resolve any complex types whose group or
/// attributeGroup references may have been updated by the redefinitions.
///
/// This is necessary because the external schema's types were parsed with the
/// OLD group/attributeGroup definitions eagerly inlined; after redefine replaces
/// those definitions, we need to update the types to reflect the new definitions.
fn reresolve_types_after_redefine(validator: &mut XsdValidator) {
    // Collect keys that need re-resolution to avoid borrow issues
    let keys_to_update: Vec<(Option<String>, String)> = validator
        .types
        .iter()
        .filter_map(|(key, td)| {
            if let TypeDef::Complex(ct) = td {
                if ct.group_ref.is_some() || !ct.attribute_group_refs.is_empty() {
                    return Some(key.clone());
                }
            }
            None
        })
        .collect();

    for key in keys_to_update {
        let td = match validator.types.get(&key) {
            Some(td) => td.clone(),
            None => continue,
        };
        if let TypeDef::Complex(mut ct) = td {
            // Re-resolve model group reference
            if let Some(ref mg_key) = ct.group_ref {
                if let Some(mg) = validator.model_groups.get(mg_key) {
                    ct.content = group_ref_content(&mg.content, &ct.group_ref_occurs);
                }
            }
            // Re-resolve attribute group references
            if !ct.attribute_group_refs.is_empty() {
                // Rebuild attributes: start with non-attributeGroup attributes.
                // For simplicity, we re-derive all attributes from the attribute
                // group refs. Any directly declared attributes on the complexType
                // that aren't from group refs would need to be preserved, but
                // in practice the external schema complexTypes only get attributes
                // from attributeGroup refs (which are what we're re-resolving).
                let mut new_attrs = Vec::new();
                let mut new_wildcard = ct.attribute_wildcard.clone();
                for ag_key in &ct.attribute_group_refs {
                    if let Some(ag) = validator.attribute_groups.get(ag_key) {
                        new_attrs.extend(ag.attributes.iter().cloned());
                        if let Some(ref ag_wc) = ag.wildcard {
                            new_wildcard = match new_wildcard {
                                Some(existing_wc) => existing_wc.intersect(ag_wc),
                                None => Some(ag_wc.clone()),
                            };
                        }
                    }
                }
                ct.attributes = new_attrs;
                ct.attribute_wildcard = new_wildcard;
            }
            validator.types.insert(key, TypeDef::Complex(ct));
        }
    }
}

/// Check if a string looks like an absolute URI (starts with a scheme per RFC 3986:
/// `ALPHA *(ALPHA / DIGIT / "+" / "-" / ".") ":"`).
fn is_absolute_uri(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    for &b in &bytes[1..] {
        if b == b':' {
            return true;
        }
        if !b.is_ascii_alphanumeric() && b != b'+' && b != b'-' && b != b'.' {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn read_resolved_schema_detects_stale_file_identity() {
        let dir = std::env::temp_dir().join(format!(
            "uppsala-composition-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("include.xsd");
        fs::write(&path, "old").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let resolved = ResolvedSchemaPath {
            path: path.clone(),
            identity: file_identity(&metadata),
        };
        let replacement = dir.join("replacement.xsd");
        fs::write(&replacement, "new").unwrap();
        fs::rename(&replacement, &path).unwrap();

        let err = read_resolved_schema(&resolved, "include.xsd", "include")
            .expect_err("changed file identity must be rejected");
        assert!(err.to_string().contains("file changed during resolution"));

        fs::remove_dir_all(&dir).ok();
    }
}
