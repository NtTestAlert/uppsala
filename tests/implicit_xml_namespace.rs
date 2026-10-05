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
