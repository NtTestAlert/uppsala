//! The reserved xml prefix needs no explicit declaration in XPath or XSLT.
use uppsala::{Parser, Stylesheet, XPathEvaluator};

#[test]
fn xpath_resolves_xml_attributes_without_registration() {
    let mut doc = Parser::new()
        .parse(r#"<root xml:id="keep" xml:base="https://example.org/" xml:lang="en" id="plain"/>"#)
        .unwrap();
    doc.prepare_xpath();
    let evaluator = XPathEvaluator::new();
    assert_eq!(
        evaluator
            .evaluate(&doc, doc.root(), "string(/root/@xml:id)")
            .unwrap()
            .to_string_value(&doc),
        "keep"
    );
    assert_eq!(
        evaluator
            .evaluate(&doc, doc.root(), "count(/root/@xml:*)")
            .unwrap()
            .to_string_value(&doc),
        "3"
    );
}

#[test]
fn xslt_selects_and_matches_implicit_xml_attributes() {
    let sheet = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
      <xsl:output omit-xml-declaration="yes"/>
      <xsl:param name="language" select="string(/root/@xml:lang)"/>
      <xsl:template match="/"><out language="{$language}"><xsl:apply-templates/></out></xsl:template>
      <xsl:template match="@xml:id|@xml:base"/>
      <xsl:template match="@*|node()"><xsl:copy><xsl:apply-templates select="@*|node()"/></xsl:copy></xsl:template>
    </xsl:stylesheet>"#;
    let mut source = Parser::new().parse(r#"<root xml:id="old" xml:base="https://example.org/" xml:lang="en" id="plain"><child xml:id="nested" keep="yes"/></root>"#).unwrap();
    source.prepare_xpath();
    for xml in [
        sheet.to_string(),
        sheet.replace(
            "<xsl:stylesheet ",
            "<xsl:stylesheet xmlns:xml=\"http://www.w3.org/XML/1998/namespace\" ",
        ),
    ] {
        let style = Parser::new().parse(&xml).unwrap();
        let output = Stylesheet::compile(&style)
            .unwrap()
            .transform(&source)
            .unwrap();
        assert_eq!(
            output,
            r#"<out language="en"><root xml:lang="en" id="plain"><child keep="yes"/></root></out>"#
        );
    }
}

#[test]
fn xpath_ignores_reserved_prefix_rebinding() {
    let mut doc = Parser::new()
        .parse(r#"<root xmlns:other="urn:other" xml:id="real" other:id="other"/>"#)
        .unwrap();
    doc.prepare_xpath();
    let mut evaluator = XPathEvaluator::new();
    for uri in [
        "urn:other",
        "",
        "http://www.w3.org/XML/1998/namespace",
        "urn:other",
    ] {
        evaluator.add_namespace("xml", uri);
        assert_eq!(
            evaluator
                .evaluate(&doc, doc.root(), "string(/root/@xml:id)")
                .unwrap()
                .to_string_value(&doc),
            "real"
        );
        assert_eq!(
            evaluator
                .evaluate(&doc, doc.root(), "count(/root/@xml:*)")
                .unwrap()
                .to_string_value(&doc),
            "1"
        );
    }
    // Ordinary application prefixes can still be registered and rebound.
    evaluator.add_namespace("custom", "urn:first");
    evaluator.add_namespace("custom", "urn:other");
    assert_eq!(
        evaluator
            .evaluate(&doc, doc.root(), "string(/root/@custom:id)")
            .unwrap()
            .to_string_value(&doc),
        "other"
    );
}

#[test]
fn mutated_stylesheet_cannot_rebind_xml_in_expressions_or_patterns() {
    use uppsala::xslt::ParameterValue;
    let xml = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
      <xsl:output method="text"/>
      <xsl:param name="picked" select="string(/root/@xml:id)"/>
      <xsl:template match="/"><xsl:value-of select="$picked"/>|<xsl:apply-templates select="/root/@*"/></xsl:template>
      <xsl:template match="@*"/>
      <xsl:template match="@xml:id"><xsl:value-of select="."/>;</xsl:template>
    </xsl:stylesheet>"#;
    let mut source = Parser::new()
        .parse(r#"<root xmlns:other="urn:other" xml:id="real" other:id="other"/>"#)
        .unwrap();
    source.prepare_xpath();
    for uri in ["urn:other", "", "http://www.w3.org/XML/1998/namespace"] {
        let mut style = Parser::new().parse(xml).unwrap();
        let root = style.document_element().unwrap();
        assert!(style.declare_namespace(root, Some("xml"), uri));
        // Also exercise local declarations on parameters and match templates.
        for child in style.children(root) {
            style.declare_namespace(child, Some("xml"), uri);
        }
        let sheet = Stylesheet::compile(&style).unwrap();
        assert_eq!(sheet.transform(&source).unwrap(), "real|real;");
        let params = [(
            "picked".into(),
            ParameterValue::Expression("string(/root/@xml:id)".into()),
        )];
        assert_eq!(
            sheet.transform_with_params(&source, &params).unwrap(),
            "real|real;"
        );
    }
}
