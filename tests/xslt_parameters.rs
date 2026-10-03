//! Invocation-local XSLT parameters must preserve XPath values and sheet reuse.
use uppsala::{
    xslt::{ParameterValue, Stylesheet},
    Parser,
};

const STYLE: &str = r#"<xsl:stylesheet version="1.0"
 xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:p="urn:p" xmlns:q="urn:p">
 <xsl:output method="text"/>
 <xsl:variable name="derived" select="concat($p:value, '!')"/>
 <xsl:param name="p:value" select="'default'"/>
 <xsl:template match="/"><xsl:value-of select="$q:value"/><xsl:text>|</xsl:text><xsl:value-of select="$derived"/></xsl:template>
 </xsl:stylesheet>"#;

#[test]
fn defaults_aliases_duplicate_precedence_and_reuse() {
    let style = Parser::new().parse(STYLE).unwrap();
    let mut source = Parser::new().parse("<r/>").unwrap();
    source.prepare_xpath();
    let sheet = Stylesheet::compile(&style)
        .unwrap()
        .with_param("p:value", "builder");
    assert_eq!(sheet.transform(&source).unwrap(), "builder|builder!");
    let params = vec![
        ("q:value".into(), ParameterValue::String("first".into())),
        (
            "{urn:p}value".into(),
            ParameterValue::String("O'Brien & ö".into()),
        ),
    ];
    assert_eq!(
        sheet.transform_with_params(&source, &params).unwrap(),
        "O'Brien & ö|O'Brien & ö!"
    );
    assert_eq!(sheet.transform(&source).unwrap(), "builder|builder!");
    let bad = [("p:value".into(), ParameterValue::Expression("(".into()))];
    assert!(sheet.transform_with_params(&source, &bad).is_err());
    assert_eq!(sheet.transform(&source).unwrap(), "builder|builder!");
}

#[test]
fn xpath_values_keep_their_types() {
    let xml = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
      <xsl:output method="text"/><xsl:param name="nodes"/><xsl:param name="flag"/>
      <xsl:param name="number"/>
      <xsl:template match="/"><xsl:value-of select="count($nodes)"/>|<xsl:if test="$flag">wrong</xsl:if>|<xsl:value-of select="$number + 1"/></xsl:template>
    </xsl:stylesheet>"#;
    let style = Parser::new().parse(xml).unwrap();
    let mut source = Parser::new().parse("<r><i/><i/></r>").unwrap();
    source.prepare_xpath();
    let sheet = Stylesheet::compile(&style).unwrap();
    let params = [
        ("nodes".into(), ParameterValue::Expression("/r/i".into())),
        ("flag".into(), ParameterValue::Expression("false()".into())),
        ("number".into(), ParameterValue::Expression("5".into())),
    ];
    assert_eq!(
        sheet.transform_with_params(&source, &params).unwrap(),
        "2||6"
    );
}

#[test]
fn invalid_unused_expressions_and_names_fail() {
    let style = Parser::new().parse(STYLE).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    let sheet = Stylesheet::compile(&style).unwrap();
    for (name, expression) in [("unused", "("), ("bad:name", "'ok'"), ("{}", "'ok'")] {
        assert!(sheet
            .transform_with_params(
                &source,
                &[(name.into(), ParameterValue::Expression(expression.into()))]
            )
            .is_err());
    }
}

#[test]
fn global_body_forward_dependency_and_cycle() {
    let xml = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
    <xsl:output method="text"/>
    <xsl:variable name="first"><xsl:variable name="local" select="'a'"/><xsl:value-of select="concat($local, $later)"/></xsl:variable>
    <xsl:param name="later" select="'b'"/>
    <xsl:template match="/"><xsl:value-of select="$first"/></xsl:template>
    </xsl:stylesheet>"#;
    let source = Parser::new().parse("<r/>").unwrap();
    let style = Parser::new().parse(xml).unwrap();
    assert_eq!(
        Stylesheet::compile(&style)
            .unwrap()
            .transform(&source)
            .unwrap(),
        "ab"
    );
    let cyclic = xml.replace("select=\"'b'\"", "select=\"$first\"");
    let style = Parser::new().parse(&cyclic).unwrap();
    assert!(Stylesheet::compile(&style)
        .unwrap()
        .transform(&source)
        .is_err());
}

#[test]
fn external_expressions_reference_earlier_arguments() {
    let style = Parser::new().parse(STYLE).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    let sheet = Stylesheet::compile(&style).unwrap();
    let params = [
        (
            "earlier".into(),
            ParameterValue::Expression("'hello'".into()),
        ),
        (
            "p:value".into(),
            ParameterValue::Expression("concat($earlier, '!')".into()),
        ),
    ];
    assert_eq!(
        sheet.transform_with_params(&source, &params).unwrap(),
        "hello!|hello!!"
    );
    let reversed = [params[1].clone(), params[0].clone()];
    assert!(sheet.transform_with_params(&source, &reversed).is_err());
}
