use uppsala::{parse, XPathEvaluator};

#[test]
fn child_iteration_shares_exhaustion_from_both_ends() {
    // Exercise every interleaving on small lists, including empty/singleton.
    for count in 0..=5 {
        let xml = format!("<r>{}</r>", "<item/>".repeat(count));
        let doc = parse(&xml).unwrap();
        let root = doc.document_element().unwrap();
        let children = doc.children(root);
        for directions in 0..(1usize << count) {
            let mut iter = doc.children_iter(root);
            let mut expected = children.iter();
            for step in 0..count {
                if directions & (1 << step) == 0 {
                    assert_eq!(iter.next(), expected.next().copied());
                } else {
                    assert_eq!(iter.next_back(), expected.next_back().copied());
                }
            }
            assert_eq!(iter.next(), None);
            assert_eq!(iter.next_back(), None);
            assert_eq!(iter.next(), None);
        }
    }
}

#[test]
fn descendant_predicates_use_document_order() {
    let doc = parse("<r><group><item id='first'/></group><item id='second'/></r>").unwrap();
    let root = doc.document_element().unwrap();
    let eval = XPathEvaluator::new();
    for axis in ["descendant", "descendant-or-self"] {
        for (predicate, expected) in [
            ("1", "first"),
            ("last()", "second"),
            ("position() = 1", "first"),
        ] {
            let nodes = eval
                .select_nodes(&doc, root, &format!("{axis}::item[{predicate}]"))
                .unwrap();
            assert_eq!(nodes.len(), 1);
            assert_eq!(doc.get_attribute(nodes[0], "id"), Some(expected));
        }
    }
    let limited = XPathEvaluator::new().with_max_node_visits(1);
    assert!(limited
        .select_nodes(&doc, root, "descendant::item")
        .is_err());
}
