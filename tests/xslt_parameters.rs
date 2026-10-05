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

/// Literals are installed before expressions even when listed after them. An
/// expression therefore wins a mixed duplicate, not necessarily the last tuple.
#[test]
fn literals_bind_first_and_expression_duplicates_follow_input_order() {
    let style = Parser::new().parse(STYLE).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    let sheet = Stylesheet::compile(&style).unwrap();
    let params = [
        (
            "p:value".into(),
            ParameterValue::Expression("concat($q:value, '-first')".into()),
        ),
        (
            "{urn:p}value".into(),
            ParameterValue::Expression("concat($p:value, '-last')".into()),
        ),
        ("q:value".into(), ParameterValue::String("literal".into())),
    ];
    assert_eq!(
        sheet.transform_with_params(&source, &params).unwrap(),
        "literal-first-last|literal-first-last!"
    );
    assert_eq!(sheet.transform(&source).unwrap(), "default|default!");
}

/// Unused external bindings can feed later external expressions, but must not
/// override variables or introduce bindings into the stylesheet's global scope.
#[test]
fn undeclared_and_variable_arguments_do_not_override_declarations() {
    let style = Parser::new().parse(STYLE).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    let sheet = Stylesheet::compile(&style).unwrap();
    let params = [
        ("unused".into(), ParameterValue::String("ignored".into())),
        (
            "derived".into(),
            ParameterValue::Expression("'wrong'".into()),
        ),
    ];
    assert_eq!(
        sheet.transform_with_params(&source, &params).unwrap(),
        "default|default!"
    );
    let xml = STYLE.replace("select=\"'default'\"", "select=\"$unused\"");
    let style = Parser::new().parse(&xml).unwrap();
    let sheet = Stylesheet::compile(&style).unwrap();
    assert!(sheet.transform_with_params(&source, &params).is_err());
}

/// The default XML namespace does not qualify unprefixed parameter names.
#[test]
fn unprefixed_names_are_distinct_from_qualified_names() {
    let xml = STYLE
        .replace("xmlns:p=\"urn:p\"", "xmlns=\"urn:p\" xmlns:p=\"urn:p\"")
        .replace(
            "<xsl:param name=\"p:value\"",
            "<xsl:param name=\"value\" select=\"'plain'\"/><xsl:param name=\"p:value\"",
        )
        .replace(
            "select=\"$q:value\"",
            "select=\"concat($value, ':', $q:value)\"",
        );
    let style = Parser::new().parse(&xml).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    let sheet = Stylesheet::compile(&style).unwrap();
    let params = [
        ("value".into(), ParameterValue::String("unqualified".into())),
        (
            "{urn:p}value".into(),
            ParameterValue::Expression("'qualified'".into()),
        ),
    ];
    assert_eq!(
        sheet.transform_with_params(&source, &params).unwrap(),
        "unqualified:qualified|qualified!"
    );
}

/// A failed evaluation must leave the compiled sheet reusable; an override can
/// break a default dependency cycle, but must not persist into the next call.
#[test]
fn stylesheet_reuse_after_dependency_and_expression_failures() {
    let xml = STYLE.replace("select=\"'default'\"", "select=\"$derived\"");
    let style = Parser::new().parse(&xml).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    let sheet = Stylesheet::compile(&style).unwrap();
    assert!(sheet
        .transform(&source)
        .unwrap_err()
        .to_string()
        .contains("circular"));
    for text in ["first", "second"] {
        let params = [("p:value".into(), ParameterValue::String(text.into()))];
        assert_eq!(
            sheet.transform_with_params(&source, &params).unwrap(),
            format!("{text}|{text}!")
        );
        assert!(sheet.transform(&source).is_err());
        let invalid = [(
            "p:value".into(),
            ParameterValue::Expression("$missing".into()),
        )];
        assert!(sheet.transform_with_params(&source, &invalid).is_err());
    }
}

/// Declarations and references resolve prefixes at their own stylesheet nodes.
#[test]
fn locally_declared_prefixes_work_in_expressions_and_avts() {
    let xml = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
      <xsl:output omit-xml-declaration="yes"/>
      <xsl:param xmlns:p="urn:local" name="p:v" select="'global'"/>
      <xsl:variable xmlns:q="urn:local" name="derived" select="concat($q:v, '!')"/>
      <xsl:template match="/" xmlns:p="urn:local">
        <out value="{$p:v}"><xsl:value-of select="$derived"/>
          <xsl:if test="$p:v = 'global'"><yes/></xsl:if>
          <xsl:choose><xsl:when xmlns:q="urn:local" test="$q:v = 'global'"><chosen/></xsl:when></xsl:choose>
        </out>
      </xsl:template>
    </xsl:stylesheet>"#;
    let style = Parser::new().parse(xml).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    let sheet = Stylesheet::compile(&style).unwrap();
    assert_eq!(
        sheet.transform(&source).unwrap(),
        "<out value=\"global\">global!<yes/><chosen/></out>"
    );
    assert_eq!(
        sheet
            .transform_with_params(
                &source,
                &[(
                    "{urn:local}v".into(),
                    ParameterValue::String("override".into())
                )]
            )
            .unwrap(),
        "<out value=\"override\">override!</out>"
    );
}

/// Rebinding a prefix must not change a previously declared variable's name;
/// aliases must also work for template arguments and result-tree fragments.
#[test]
fn local_bindings_and_fragment_copy_preserve_namespace_identity() {
    let xml = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:p="urn:global">
      <xsl:output omit-xml-declaration="yes"/>
      <xsl:param name="p:v" select="'global'"/>
      <xsl:template match="/">
        <xsl:variable xmlns:p="urn:local" name="p:v"><item>local</item></xsl:variable>
        <out><xsl:value-of select="$p:v"/>
          <xsl:copy-of xmlns:q="urn:local" select="$q:v"/>
          <xsl:call-template name="named"><xsl:with-param xmlns:q="urn:local" name="q:arg" select="$q:v"/></xsl:call-template>
        </out>
      </xsl:template>
      <xsl:template name="named"><xsl:param xmlns:p="urn:local" name="p:arg"/>
        <xsl:value-of xmlns:q="urn:local" select="$q:arg"/>
      </xsl:template>
    </xsl:stylesheet>"#;
    let style = Parser::new().parse(xml).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    assert_eq!(
        Stylesheet::compile(&style)
            .unwrap()
            .transform(&source)
            .unwrap(),
        "<out>global<item>local</item>local</out>"
    );
}

/// The builder API historically ignores unmatched names; the per-call API is
/// deliberately strict about undefined prefixes and malformed parameter names.
#[test]
fn unresolved_builder_defaults_are_ignored_but_call_names_are_strict() {
    let style = Parser::new().parse(STYLE).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    let sheet = Stylesheet::compile(&style)
        .unwrap()
        .with_param("missing:arg", "ignored")
        .with_param("{}", "ignored")
        .with_param("p:value", "valid");
    assert_eq!(sheet.transform(&source).unwrap(), "valid|valid!");
    for name in ["missing:arg", "{}"] {
        assert!(sheet
            .transform_with_params(
                &source,
                &[(name.into(), ParameterValue::String("bad".into()))]
            )
            .is_err());
        assert_eq!(sheet.transform(&source).unwrap(), "valid|valid!");
    }
}

/// Namespace context applies to source paths and template patterns as well as
/// variable references, even when the prefix has no stylesheet-root binding.
#[test]
fn local_namespaces_apply_to_paths_and_patterns() {
    let xml = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
      <xsl:output method="text"/>
      <xsl:template match="/">
        <xsl:apply-templates xmlns:s="urn:source" select="/s:root/s:item"/>
        <xsl:for-each xmlns:s="urn:source" select="/s:root/s:item"><xsl:value-of select="@id"/></xsl:for-each>
      </xsl:template>
      <xsl:template xmlns:t="urn:source" match="t:item"><xsl:value-of select="@id"/></xsl:template>
    </xsl:stylesheet>"#;
    let style = Parser::new().parse(xml).unwrap();
    let mut source = Parser::new()
        .parse("<root xmlns=\"urn:source\"><item id=\"1\"/><item id=\"2\"/></root>")
        .unwrap();
    source.prepare_xpath();
    assert_eq!(
        Stylesheet::compile(&style)
            .unwrap()
            .transform(&source)
            .unwrap(),
        "1212"
    );
}

/// Ignored builder defaults must not become a temporary external-expression scope.
#[test]
fn ignored_builder_defaults_are_invisible_to_call_expressions() {
    let style = Parser::new().parse(STYLE).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    for name in ["helper", "derived"] {
        let sheet = Stylesheet::compile(&style)
            .unwrap()
            .with_param(name, "hidden");
        let params = [(
            "p:value".into(),
            ParameterValue::Expression(format!("${name}")),
        )];
        assert!(sheet
            .transform_with_params(&source, &params)
            .unwrap_err()
            .to_string()
            .contains("Undefined variable"));
        assert_eq!(sheet.transform(&source).unwrap(), "default|default!");
    }
    // Declared builder parameters remain visible, including namespace aliases.
    let sheet = Stylesheet::compile(&style)
        .unwrap()
        .with_param("q:value", "builder");
    let params = [(
        "p:value".into(),
        ParameterValue::Expression("concat($p:value, '!')".into()),
    )];
    assert_eq!(
        sheet.transform_with_params(&source, &params).unwrap(),
        "builder!|builder!!"
    );
}

/// Run in a child test process so real stderr can be checked without changing
/// the public API or depending on the Rust test harness's output capture.
#[test]
fn global_message_child() {
    let Ok(case) = std::env::var("UPPSALA_GLOBAL_MESSAGE_CASE") else {
        return;
    };
    let later = match case.as_str() {
        "success" => "'ready'",
        "cycle" => "$first",
        "failure" => "unknown-function()",
        _ => panic!("unexpected test case"),
    };
    let xml = format!(
        r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
      <xsl:output method="text"/>
      <xsl:variable name="first">
        <xsl:message>outer-once</xsl:message>
        <xsl:call-template name="build"/>
      </xsl:variable>
      <xsl:variable name="middle" select="$later"/>
      <xsl:variable name="later" select="{later}"/>
      <xsl:template name="build">
        <xsl:variable name="local" select="'private'"/>
        <xsl:message>nested-once</xsl:message>
        <xsl:value-of select="$middle"/>
      </xsl:template>
      <xsl:template match="/"><xsl:message>template-once</xsl:message><xsl:value-of select="$first"/></xsl:template>
    </xsl:stylesheet>"#
    );
    let style = Parser::new().parse(&xml).unwrap();
    let source = Parser::new().parse("<r/>").unwrap();
    let result = Stylesheet::compile(&style).unwrap().transform(&source);
    if case == "success" {
        assert_eq!(result.unwrap(), "ready");
    } else {
        assert!(result.is_err());
    }
}

#[test]
fn deferred_globals_publish_messages_only_after_success() {
    for case in ["success", "cycle", "failure"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "global_message_child", "--nocapture"])
            .env("UPPSALA_GLOBAL_MESSAGE_CASE", case)
            .output()
            .unwrap();
        assert!(output.status.success(), "child failed: {:?}", output);
        let stderr = String::from_utf8(output.stderr).unwrap();
        let messages: Vec<_> = stderr
            .lines()
            .filter(|line| line.starts_with("xsl:message:"))
            .collect();
        if case == "success" {
            assert_eq!(
                messages,
                [
                    "xsl:message: outer-once",
                    "xsl:message: nested-once",
                    "xsl:message: template-once"
                ]
            );
        } else {
            assert!(
                messages.is_empty(),
                "failed initializer leaked messages: {stderr}"
            );
        }
    }
}
