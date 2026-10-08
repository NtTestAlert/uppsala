# ADR 0019: Build the DOM directly from a shared tokenizer, not from pull events

## Status

Accepted (2026-10-08). Amends [ADR 0018](0018-pull-parser-event-contract.md).

## Context

### History

| Date | Commit | What happened |
|---|---|---|
| 2026-07-04 | `1ec10feb` "feat: optimize XML parsing and add libxml2 benchmark workflow" | Added `performance-harness/` and `just bench-libxml2`. `Parser::parse` was a recursive-descent DOM builder (`parse_element`/`parse_content` in `parser.rs`) that allocated each arena node in place. The libxml2 table in `docs/performance.md` was measured here: Uppsala led libxml2 by 1.6x to 1.9x on text-heavy inputs and by 2.3x to 4.7x on SAML-shaped inputs. |
| 2026-07-05 | `c7117e6d` "feat: add pull parser event API and differential coverage" (released in 0.9.0) | Added the public `PullParser`/`PullEvent` API and `document_from_pull`. As the commit message says, it also "refactor[ed] `Parser::parse` to consume `PullParser` events through `document_from_pull`", deleting the direct builder. ADR 0018 recorded `PullParser` as the parser event contract and asked for one active implementation of element/content/prolog parsing. The benchmark table was not re-measured. |
| 2026-10-07 | `d52fbdd7` merge of PR #53 (`fix/perf`) and follow-up `8571ac70` | XPath result construction, traversal, and attribute-preparation work. Did not touch the parse path. |
| 2026-10-07 | benchmark re-run | On the same libxml2 build, the DOM path had fallen to parity with libxml2 on Atom, SOAP, and `comps_0.xml` (0.9x to 1.0x) and to 1.4x on the SAML metadata aggregate. A bisect over the 41 commits between `1ec10feb` and `main`, using `comps_0.xml` DOM parse time as the signal, named `c7117e6d` as the first bad commit. |

### Diagnosis

Allocation counts per parse were identical before and after `c7117e6d`
(17,189 versus 17,193 on `comps_0.xml`), so the cost was not allocation. A
`perf` profile of a DOM-only parse loop showed the time in block memory moves
of the 144-byte `PullEvent`:

- `document_from_pull` carried 34% of self time, 70% of it on 32-byte vector
  moves of the event.
- `PullParser::next_event` carried 22%, with a quarter of that on one store
  writing the popped event back to the stack.

Per node the event was built on the stack, pushed into a `VecDeque`, popped
back out, returned through two `Result` layers, destructured, rebuilt as an
`Element`, and finally moved into the arena: about six copies of 144 bytes.
The DOM builder also received an `EndElement` carrying a cloned 72-byte `QName`
it never read, and `StartNamespace`/`EndNamespace` events it ignored.

The overhead is per node, so node-dense inputs (about 20 bytes per node for
`comps_0.xml`, Atom, SOAP) lost half their speed while SAML inputs with large
attribute values and certificate text amortized it. Even the scan-only pull
pass with no DOM at all (7.1 ms on `comps_0.xml`) was slower than the July
full DOM parse (5.8 ms), so trimming the event path could not reach parity.

## Decision

One tokenizer, two sinks, no events on the DOM path.

- `pull::Tokenizer` (crate-private) owns the prolog/content/trailing state
  machine, start-tag scanning with namespace checks and QName resolution,
  end-tag matching, text-run scanning, the open-element stack, the namespace
  resolver, and the entity map and budget. `Tokenizer::step` consumes one
  syntactic unit and reports it to a `TokenSink` through plain method calls
  with the resolved name, attributes, namespace declarations, and byte range
  passed by value. A self-closing tag reports `start_element` then
  `end_element`.
- `DomSink` (crate-private) implements `TokenSink` by calling
  `Document::alloc_node` and `append_child_unchecked` directly. The `Element`
  is built once in the sink and moved once into the arena. `Parser::parse`
  constructs a `Tokenizer` from its options and drives it into a `DomSink`
  (`pull::build_document`).
- `PullParser` implements the public event API with an `EventSink` that
  buffers `PullEvent`s in a `VecDeque` and keeps its own stack of resolved
  names and namespace counts for `EndElement` and `EndNamespace`. The public
  `PullParser`, `PullEvent`, `NamespaceDeclaration`, `document_from_pull`, and
  `parse_document` signatures and semantics are unchanged, including fusing to
  `Ok(None)` after an error.
- `document_from_pull` stays as the explicit event-stream-to-DOM path and now
  feeds a `DomSink` from events. It is the subject of the ADR 0018 differential
  tests, not the way `Parser::parse` works. It requires an unconsumed
  `PullParser` and returns an error otherwise, since events already taken
  (declaration, DOCTYPE, open ancestors) cannot be reconstructed.
- `parser::parse_doctype` no longer takes a `Document`; the caller captures the
  raw DOCTYPE text from the cursor range. This removes the throwaway
  `Document` the pull parser allocated per parse.

The sink is a generic parameter, so each path is monomorphized with no dynamic
dispatch. ADR 0018's rule that element/content/prolog parsing has one active
implementation still holds: that implementation is `Tokenizer`.

## Results

`Ratio` is libxml2 time divided by Uppsala time; above 1.0 means Uppsala is
faster. "Before" is `main` at `d52fbdd7`, "after" is this change, both built
with `-C target-cpu=native` against the same static libxml2 (`c8eaf223`,
2026-07-02, `-O3 -march=native`), CPU-pinned, medians.

Laptop (Intel Core Ultra 7 155H, 101 to 301 samples):

| Input | Size | July `1ec10feb` | Before | After | libxml2 | Ratio after |
|---|---:|---:|---:|---:|---:|---:|
| SAML small | 3.4 KB | 16.8 us | 29.1 us | 17.0 us | 64.0 us | 3.77x |
| SAML metadata aggregate | 666 KB | 3.80 ms | 6.50 ms | 3.60 ms | 8.50 ms | 2.36x |
| Atom feed archive | 848 KB | 6.40 ms | 13.71 ms | 6.24 ms | 11.82 ms | 1.89x |
| SOAP invoice batch | 715 KB | 6.70 ms | 13.64 ms | 6.20 ms | 10.61 ms | 1.71x |
| libxml2 `comps_0.xml` | 608 KB | 5.79 ms | 11.68 ms | 5.86 ms | 10.66 ms | 1.82x |
| saml-response.xml (real) | 11.5 KB | 36.9 us | 53.5 us | 38.4 us | 126.0 us | 3.28x |
| eduGAIN aggregate (real) | 91.9 MB | n/a | 859 ms | 711 ms | 1060 ms | 1.49x |

Old pyFF server (AMD EPYC 7551 VM, 301 samples; small-input timings on this
shared host swing by up to 2x between runs, so read the large rows):

| Input | Size | July `1ec10feb` | Before | After | libxml2 | Ratio after |
|---|---:|---:|---:|---:|---:|---:|
| SAML metadata aggregate | 666 KB | 4.10 ms | 4.63 ms | 3.99 ms | 9.00 ms | 2.26x |
| Atom feed archive | 848 KB | 6.83 ms | 9.38 ms | 6.81 ms | 13.43 ms | 1.97x |
| SOAP invoice batch | 715 KB | 6.52 ms | 10.46 ms | 5.87 ms | 11.73 ms | 2.00x |
| libxml2 `nvdcve_0.xml` | 287 KB | 2.64 ms | 3.45 ms | 2.71 ms | 4.91 ms | 1.81x |
| libxml2 `comps_0.xml` | 608 KB | 5.75 ms | 9.63 ms | 5.47 ms | 10.49 ms | 1.92x |

The DOM path is back to the July numbers on both hosts. The harness's
`Pull DOM` column (`document_from_pull`) remains about 1.6x slower than the
direct path on node-dense inputs; that is the price of the event
representation and is now confined to callers who ask for events.

## Consequences

- Do not route `Parser::parse` through `PullEvent` again. If a future change
  needs the DOM and pull paths to share new behaviour, add it to `Tokenizer`
  or to `TokenSink`, not to the event stream.
- `just bench-libxml2` (or the manual steps in `docs/performance.md`) must be
  re-run whenever the parse path changes, and the table in
  `docs/performance.md` updated. The regression went unnoticed for three
  months because the table was not re-measured after `c7117e6d`.
- The pull-event path still copies events through a `VecDeque`. It can be made
  cheaper (return events by reference into sink-owned storage, drop the
  `QName` clone for `EndElement`) without touching the DOM path.
- Allocation counts are not a sufficient performance signal for this parser;
  a `perf` profile or the harness is needed to see copy overhead.
