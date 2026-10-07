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

#[test]
fn step_predicates_remain_local_to_each_parent() {
    let xml = "<r><g><item id='a'/><item id='b'/><item id='c'/></g><g><item id='d'/><item id='e'/></g></r>";
    for prepare in [false, true] {
        let mut doc = parse(xml).unwrap();
        if prepare {
            doc.prepare_xpath();
        }
        let root = doc.document_element().unwrap();
        let eval = XPathEvaluator::new();
        for (query, expected) in [
            ("g/item[position() mod 2 = 1][last()]", vec!["c", "d"]),
            ("g/item[last()][1]", vec!["c", "e"]),
            ("g/item/preceding-sibling::item[1]", vec!["a", "b", "d"]),
        ] {
            let nodes = eval.select_nodes(&doc, root, query).unwrap();
            let ids: Vec<_> = nodes
                .iter()
                .map(|&n| doc.get_attribute(n, "id").unwrap())
                .collect();
            assert_eq!(ids, expected, "{query}, prepared={prepare}");
        }
        if prepare {
            let attrs = eval.select_nodes(&doc, root, "g/item/@*[last()]").unwrap();
            assert_eq!(attrs.len(), 5);
        }
        // Multiple contexts produce an initially unordered stream; duplicates
        // from the union must also disappear before returning the node-set.
        let nodes = eval
            .select_nodes(&doc, root, "descendant-or-self::*/child::* | g/item")
            .unwrap();
        assert_eq!(nodes, doc.descendants(root));
    }
}

#[test]
fn streamed_axis_filtering_preserves_visit_budgets() {
    let mut doc = parse("<r x='1' y='2' z='3'><a/><b/><a/></r>").unwrap();
    doc.prepare_xpath();
    let root = doc.document_element().unwrap();
    // Unmatched nodes still cost a visit; filtering must not bypass accounting.
    for query in ["missing", "@missing", "*", "@*"] {
        assert!(XPathEvaluator::new()
            .with_max_node_visits(2)
            .select_nodes(&doc, root, query)
            .is_err());
        assert!(XPathEvaluator::new()
            .with_max_node_visits(3)
            .select_nodes(&doc, root, query)
            .is_ok());
    }
    let doc = parse("<r><g><item/><item/></g><g><item/><item/></g></r>").unwrap();
    let root = doc.document_element().unwrap();
    let query = "g/item[position() <= 2][last()]";
    // Two g visits, then two item visits and two scans of two predicates per g.
    assert!(XPathEvaluator::new()
        .with_max_node_visits(13)
        .select_nodes(&doc, root, query)
        .is_err());
    let nodes = XPathEvaluator::new()
        .with_max_node_visits(14)
        .select_nodes(&doc, root, query)
        .unwrap();
    assert_eq!(nodes.len(), 2);
}
