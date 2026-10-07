//! Performance harness for uppsala.
//!
//! Covers the operations every consumer hits: parsing (namespace-aware and
//! not), DOM traversal (children/descendants), repeated XPath evaluation,
//! attribute-axis preparation, serialization, and pull parsing.
//!
//! Run:    cargo bench --bench uppsala
//! Save:   cargo bench --bench uppsala -- --save-baseline main

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};

/// SAML-shaped, namespace-heavy response (the realistic uppsala consumer case).
fn saml_shaped(assertions: usize) -> String {
    let mut s = String::with_capacity(16 * 1024 * (1 + assertions / 40));
    s.push_str(
        r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" ID="_response" Version="2.0" IssueInstant="2026-06-29T12:00:00Z">"#,
    );
    s.push_str(r#"<saml:Issuer>https://idp.example.org/metadata</saml:Issuer>"#);
    for i in 0..assertions {
        s.push_str(&format!(
            r#"<saml:Assertion ID="_assertion{i}" Version="2.0" IssueInstant="2026-06-29T12:00:00Z"><saml:Issuer>https://idp.example.org/metadata</saml:Issuer><saml:Subject><saml:NameID Format="urn:oasis:names:tc:SAML:2.0:nameid-format:transient">user-{i}@example.org</saml:NameID><saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer"><saml:SubjectConfirmationData NotOnOrAfter="2026-06-29T12:05:00Z" Recipient="https://sp.example.org/acs"/></saml:SubjectConfirmation></saml:Subject><saml:AttributeStatement><saml:Attribute Name="urn:oid:1.2.3.4" NameFormat="urn:oasis:names:tc:SAML:2.0:attrname-format:uri"><saml:AttributeValue>value &amp; {i} &lt; threshold</saml:AttributeValue></saml:Attribute></saml:AttributeStatement></saml:Assertion>"#,
        ));
    }
    s.push_str("</samlp:Response>");
    s
}

/// Attribute-heavy document (stresses attribute collection and prep).
fn attr_heavy() -> String {
    let mut s = String::with_capacity(64 * 1024);
    s.push_str("<root>");
    for i in 0..400 {
        s.push_str(&format!(
            r#"<cell id="c{i}" class="data-cell highlighted" data-row="{i}" data-col="3" title="value &amp; label &lt; {i}&gt;" style="color:red" lang="en" role="gridcell"/>"#,
        ));
    }
    s.push_str("</root>");
    s
}

/// Text-heavy document (stresses the text scanning loops).
fn text_heavy() -> String {
    let mut s = String::with_capacity(64 * 1024);
    s.push_str("<doc>");
    for i in 0..200 {
        s.push_str("<p>");
        for _ in 0..10 {
            s.push_str("the quick brown fox jumps over the lazy dog ");
        }
        s.push_str(&format!("item {i} a &amp; b &lt; c &gt; d end."));
        s.push_str("</p>");
    }
    s.push_str("</doc>");
    s
}

fn bench_parse(c: &mut Criterion) {
    let inputs = [
        ("saml", saml_shaped(40)),
        ("attr_heavy", attr_heavy()),
        ("text_heavy", text_heavy()),
    ];
    let mut group = c.benchmark_group("parse");
    for (name, xml) in &inputs {
        group.throughput(Throughput::Bytes(xml.len() as u64));
        group.bench_function(*name, |b| {
            b.iter(|| {
                let doc = uppsala::parse(black_box(xml)).unwrap();
                black_box(doc.root());
            });
        });
    }
    group.finish();
}

fn bench_traverse(c: &mut Criterion) {
    let xml = saml_shaped(40);
    let doc = uppsala::parse(&xml).unwrap();
    let root = doc.root();

    let mut group = c.benchmark_group("traverse");
    group.bench_function("children_root", |b| {
        b.iter(|| black_box(doc.children(black_box(root))));
    });
    group.bench_function("descendants", |b| {
        b.iter(|| black_box(doc.descendants(black_box(root))));
    });
    group.finish();
}

fn bench_xpath(c: &mut Criterion) {
    let xml = saml_shaped(40);
    let mut doc = uppsala::parse(&xml).unwrap();
    doc.prepare_xpath();
    let root = doc.root();

    let mut group = c.benchmark_group("xpath");

    // Fresh evaluator per call: measures parse+tokenize+eval (one-shot usage).
    group.bench_function("oneshot_axis", |b| {
        b.iter(|| {
            let mut ev = uppsala::XPathEvaluator::new();
            ev.add_namespace("saml", "urn:oasis:names:tc:SAML:2.0:assertion");
            black_box(
                ev.select_nodes(
                    black_box(&doc),
                    root,
                    "//saml:Assertion/saml:Subject/saml:NameID",
                )
                .unwrap(),
            );
        });
    });

    // Repeated evaluation of the same expression through one evaluator:
    // measures whether the tokenzier/AST cost is paid per evaluate() call.
    let mut ev = uppsala::XPathEvaluator::new();
    ev.add_namespace("saml", "urn:oasis:names:tc:SAML:2.0:assertion");
    group.bench_function("repeat_axis", |b| {
        b.iter(|| {
            black_box(
                ev.select_nodes(
                    black_box(&doc),
                    root,
                    "//saml:Assertion/saml:Subject/saml:NameID",
                )
                .unwrap(),
            );
        });
    });
    group.bench_function("repeat_cheap", |b| {
        b.iter(|| {
            black_box(ev.evaluate(black_box(&doc), root, "1 + 1").unwrap());
        });
    });
    group.bench_function("repeat_position", |b| {
        b.iter(|| {
            black_box(
                ev.select_nodes(
                    black_box(&doc),
                    root,
                    "//saml:Assertion/saml:AttributeStatement/saml:Attribute/@Name",
                )
                .unwrap(),
            );
        });
    });
    group.finish();
}

fn bench_prepare(c: &mut Criterion) {
    let xml = attr_heavy();
    let mut group = c.benchmark_group("prepare");
    group.bench_function("attribute_nodes_attr_heavy", |b| {
        // Each timed call prepares a fresh document; parsing and destruction
        // are untimed, and no iteration measures the clean-cache early return.
        b.iter_batched_ref(
            || uppsala::Parser::new().parse(black_box(&xml)).unwrap(),
            |doc| {
                doc.prepare_xpath();
                black_box(doc);
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_xpath_shapes(c: &mut Criterion) {
    // Wide sibling sets expose traversal/filtering costs hidden by tiny steps.
    let xml = format!("<r>{}</r>", "<item id='x'/>".repeat(1024));
    let mut doc = uppsala::parse(&xml).unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    let ev = uppsala::XPathEvaluator::new();
    let mut group = c.benchmark_group("xpath_shapes");
    for (name, expression, expected) in [
        ("child_all", "*", 1024),
        ("child_no_match", "missing", 0),
        ("attributes", "item/@id", 1024),
        (
            "predicates",
            "*[position() mod 2 = 0][position() <= 16]",
            16,
        ),
    ] {
        assert_eq!(
            ev.select_nodes(&doc, root, expression).unwrap().len(),
            expected
        );
        group.bench_function(name, |b| {
            b.iter(|| black_box(ev.select_nodes(black_box(&doc), root, expression).unwrap()));
        });
    }
    let unprepared = uppsala::parse(&xml).unwrap();
    assert_eq!(ev.select_nodes(&unprepared, root, "*").unwrap().len(), 1024);
    group.bench_function("unprepared_children", |b| {
        b.iter(|| black_box(ev.select_nodes(black_box(&unprepared), root, "*").unwrap()));
    });
    group.finish();
}

fn bench_serialize(c: &mut Criterion) {
    let inputs = [("saml", saml_shaped(40)), ("text_heavy", text_heavy())];
    let mut group = c.benchmark_group("serialize");
    for (name, xml) in &inputs {
        let doc = uppsala::parse(xml).unwrap();
        group.throughput(Throughput::Bytes(xml.len() as u64));
        group.bench_function(*name, |b| {
            b.iter(|| black_box(doc.to_xml()));
        });
    }
    group.finish();
}

fn bench_pull(c: &mut Criterion) {
    let xml = saml_shaped(40);
    let mut group = c.benchmark_group("pull");
    group.bench_function("scan_saml", |b| {
        b.iter(|| {
            let mut p = uppsala::PullParser::new(black_box(&xml));
            while let Some(ev) = p.next_event().unwrap() {
                black_box(ev);
            }
        });
    });
    group.bench_function("to_dom_saml", |b| {
        b.iter(|| {
            black_box(
                uppsala::pull::document_from_pull(
                    black_box(&xml),
                    uppsala::PullParser::new(black_box(&xml)),
                )
                .unwrap(),
            );
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_parse,
    bench_traverse,
    bench_xpath,
    bench_xpath_shapes,
    bench_prepare,
    bench_serialize,
    bench_pull
);
criterion_main!(benches);
