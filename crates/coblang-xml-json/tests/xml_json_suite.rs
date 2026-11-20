//! Exhaustive unit, integration, and branch-coverage suite for `coblang-xml-json` (`Milestone M20`).

use coblang_common::{CoblangError, Result};
use coblang_xml_json::{
    BufferFit, ByteCount, CobolFieldName, DataNode, FieldAssignState, JsonBooleanMapping, JsonCode,
    JsonDetailMode, JsonGenerateOptions, JsonParseCompletion, JsonParseOptions, JsonStatus,
    JsonValue, NameMapping, NameOverride, ParseTargetNode, SuppressCategoryFilter,
    SuppressCondition, SuppressRule, SuppressTarget, ValueCategory, XML_EVENT_REGISTER_LEN,
    XmlAttributeMode, XmlCode, XmlDeclarationMode, XmlEventKind, XmlGenerateOptions,
    XmlNamespaceMode, XmlNodePlacement, XmlParseCompletion, XmlParseOptions, XmlParseSession,
    copy_into_cobol_buffer, encode_utf16_units, escape_json_string_into, is_valid_xml_char,
    is_valid_xml_ncname, is_xml_name_char, is_xml_name_start, is_zero_numeric_str, json_generate,
    json_generate_into_buffer, json_parse, normalize_cobol_number, parse_json_document,
    resolve_name_override, should_suppress_item, trim_trailing_spaces, xml_generate,
    xml_generate_into_buffer, xml_parse,
};

/// Helper constructing a validated [`CobolFieldName`].
fn fname(raw: &str) -> Result<CobolFieldName> {
    CobolFieldName::try_new(raw)
}

/// Verifies `XmlEventKind` round-trip string/register conversions, register newtypes, and field names.
#[test]
fn types_registers_events_and_identifiers() -> Result<()> {
    let all_events = [
        XmlEventKind::StartOfDocument,
        XmlEventKind::VersionInformation,
        XmlEventKind::EncodingDeclaration,
        XmlEventKind::StandaloneDeclaration,
        XmlEventKind::DocumentTypeDeclaration,
        XmlEventKind::StartOfElement,
        XmlEventKind::AttributeName,
        XmlEventKind::AttributeCharacters,
        XmlEventKind::StartOfCdataSection,
        XmlEventKind::ContentCharacters,
        XmlEventKind::EndOfCdataSection,
        XmlEventKind::Comment,
        XmlEventKind::ProcessingInstructionTarget,
        XmlEventKind::ProcessingInstructionData,
        XmlEventKind::EndOfElement,
        XmlEventKind::Exception,
        XmlEventKind::EndOfDocument,
    ];

    for kind in all_events {
        let token = kind.as_cobol_str();
        assert_eq!(XmlEventKind::from_cobol_str(token)?, kind);
        let reg = kind.to_register_bytes();
        assert_eq!(reg.len(), XML_EVENT_REGISTER_LEN);
        let padded = String::from_utf8_lossy(&reg);
        assert_eq!(XmlEventKind::from_cobol_str(&padded)?, kind);
    }
    assert!(XmlEventKind::from_cobol_str("NOT-AN-EVENT   ").is_err());

    assert!(XmlCode::ZERO.is_ok());
    assert!(!XmlCode::ZERO.is_user_terminate());
    assert!(XmlCode::USER_TERMINATE.is_user_terminate());
    assert!(!XmlCode::USER_TERMINATE.is_ok());
    assert_eq!(XmlCode::new(42).raw(), 42);

    assert!(JsonCode::ZERO.is_ok());
    assert!(!JsonCode::SYNTAX_ERROR.is_ok());
    assert_eq!(JsonCode::new(7).raw(), 7);

    assert!(JsonStatus::ZERO.is_ok());
    assert!(!JsonStatus::UNMAPPED_KEY.is_ok());
    assert_eq!(JsonStatus::new(9).raw(), 9);

    assert_eq!(ByteCount::ZERO.get(), 0);
    assert_eq!(ByteCount::new(128).get(), 128);

    let f = fname("  CUSTOMER-ID  ")?;
    assert_eq!(f.as_str(), "CUSTOMER-ID");
    assert!(CobolFieldName::try_new("").is_err());
    assert!(CobolFieldName::try_new("   ").is_err());

    let ren = NameOverride::try_rename("customerId")?;
    assert_eq!(ren, NameOverride::Rename("customerId".to_owned()));
    assert_ne!(ren, NameOverride::Omitted);
    assert!(NameOverride::try_rename("").is_err());

    assert_eq!(trim_trailing_spaces("ABC   "), "ABC");
    assert_eq!(trim_trailing_spaces("A\u{0009}  "), "A\u{0009}");
    assert_eq!(encode_utf16_units("A"), vec![65_u16]);

    let mut small = [b'.'; 3];
    let (cnt1, fit1) = copy_into_cobol_buffer("ABCD", &mut small);
    assert_eq!(
        (cnt1.get(), fit1, small),
        (3, BufferFit::Overflowed, *b"ABC")
    );

    let mut roomy = [b'.'; 5];
    let (cnt2, fit2) = copy_into_cobol_buffer("XY", &mut roomy);
    assert_eq!((cnt2.get(), fit2, roomy), (2, BufferFit::Fits, *b"XY..."));
    Ok(())
}

/// Verifies `SuppressCondition` and `SuppressCategoryFilter` predicates across categories.
#[test]
fn types_suppression_predicates_and_rules() -> Result<()> {
    assert!(SuppressCondition::Always.matches(ValueCategory::Alphanumeric, "anything"));
    assert!(SuppressCondition::WhenZero.matches(ValueCategory::Numeric, "0"));
    assert!(SuppressCondition::WhenZero.matches(ValueCategory::Numeric, "-0.00"));
    assert!(SuppressCondition::WhenZero.matches(ValueCategory::Numeric, "+000"));
    assert!(SuppressCondition::WhenZero.matches(ValueCategory::Numeric, "00-"));
    assert!(!SuppressCondition::WhenZero.matches(ValueCategory::Numeric, "12"));
    assert!(SuppressCondition::WhenZero.matches(ValueCategory::Alphanumeric, "000"));
    assert!(!SuppressCondition::WhenZero.matches(ValueCategory::Alphanumeric, ""));
    assert!(!SuppressCondition::WhenZero.matches(ValueCategory::National, "01"));

    assert!(SuppressCondition::WhenSpace.matches(ValueCategory::Alphanumeric, ""));
    assert!(SuppressCondition::WhenSpace.matches(ValueCategory::Alphanumeric, "   "));
    assert!(!SuppressCondition::WhenSpace.matches(ValueCategory::Alphanumeric, " A "));

    assert!(
        SuppressCondition::WhenLowValue.matches(ValueCategory::Alphanumeric, "\u{0000}\u{0000}")
    );
    assert!(!SuppressCondition::WhenLowValue.matches(ValueCategory::Alphanumeric, ""));
    assert!(!SuppressCondition::WhenLowValue.matches(ValueCategory::Alphanumeric, "\u{0000}A"));

    assert!(
        SuppressCondition::WhenHighValue.matches(ValueCategory::Alphanumeric, "\u{00FF}\u{00FF}")
    );
    assert!(!SuppressCondition::WhenHighValue.matches(ValueCategory::Alphanumeric, ""));
    assert!(!SuppressCondition::WhenHighValue.matches(ValueCategory::Alphanumeric, "\u{00FF}A"));

    assert!(SuppressCondition::WhenQuote.matches(ValueCategory::Alphanumeric, "\u{0022}\u{0022}"));
    assert!(!SuppressCondition::WhenQuote.matches(ValueCategory::Alphanumeric, ""));
    assert!(!SuppressCondition::WhenQuote.matches(ValueCategory::Alphanumeric, "\u{0022}A"));

    let lit = SuppressCondition::WhenLiteral("N/A".to_owned());
    assert!(lit.matches(ValueCategory::Alphanumeric, "N/A"));
    assert!(lit.matches(ValueCategory::Alphanumeric, "N/A   "));
    assert!(!lit.matches(ValueCategory::Alphanumeric, "OTHER"));
    assert_eq!(lit, SuppressCondition::WhenLiteral("N/A".to_owned()));
    assert_ne!(lit, SuppressCondition::Always);

    assert!(SuppressCategoryFilter::Any.accepts(ValueCategory::Numeric));
    assert!(SuppressCategoryFilter::NumericOnly.accepts(ValueCategory::Numeric));
    assert!(!SuppressCategoryFilter::NumericOnly.accepts(ValueCategory::Alphanumeric));
    assert!(SuppressCategoryFilter::NonNumericOnly.accepts(ValueCategory::Alphanumeric));
    assert!(SuppressCategoryFilter::NonNumericOnly.accepts(ValueCategory::National));
    assert!(!SuppressCategoryFilter::NonNumericOnly.accepts(ValueCategory::Numeric));

    let f_a = fname("A")?;
    let f_b = fname("B")?;
    assert_eq!(
        SuppressTarget::Field(f_a.clone()),
        SuppressTarget::Field(f_a.clone())
    );
    assert_ne!(
        SuppressTarget::Field(f_a.clone()),
        SuppressTarget::Every(SuppressCategoryFilter::Any)
    );

    let rules = vec![
        SuppressRule::for_field(f_a, SuppressCondition::WhenSpace),
        SuppressRule::for_field(f_b, SuppressCondition::Always),
        SuppressRule::for_every(
            SuppressCategoryFilter::NumericOnly,
            SuppressCondition::WhenZero,
        ),
    ];
    assert!(should_suppress_item(
        &rules,
        "A",
        Some(ValueCategory::Alphanumeric),
        "   "
    ));
    assert!(!should_suppress_item(
        &rules,
        "A",
        Some(ValueCategory::Alphanumeric),
        "hi"
    ));
    assert!(!should_suppress_item(&rules, "A", None, ""));
    assert!(should_suppress_item(&rules, "B", None, ""));
    assert!(should_suppress_item(
        &rules,
        "OTHER",
        Some(ValueCategory::Numeric),
        "00"
    ));
    assert!(!should_suppress_item(
        &rules,
        "OTHER",
        Some(ValueCategory::Numeric),
        "5"
    ));
    assert!(!should_suppress_item(
        &rules,
        "OTHER",
        Some(ValueCategory::Alphanumeric),
        "00"
    ));
    assert!(!should_suppress_item(&rules, "OTHER", None, ""));
    Ok(())
}

/// Verifies `JsonBooleanMapping`, numeric normalization, and `ParseTargetNode` inspection helpers.
#[test]
fn types_boolean_mappings_numbers_and_target_helpers() -> Result<()> {
    let f_a = fname("A")?;
    let f_b = fname("B")?;
    let bm_yn = JsonBooleanMapping::yes_no(f_a.clone());
    let bm_10 = JsonBooleanMapping::one_zero(f_a.clone());
    let bm_tf = JsonBooleanMapping::true_false_char(f_b.clone());
    let bm_yn_alt_false = JsonBooleanMapping::new(f_a.clone(), "Y", "X");
    assert_eq!(bm_yn, JsonBooleanMapping::yes_no(f_a.clone()));
    assert_ne!(bm_yn, bm_tf);
    assert_ne!(bm_yn, bm_10);
    assert_ne!(bm_yn, bm_yn_alt_false);
    assert!(bm_yn.matches_true("Y"));
    assert!(bm_yn.matches_true("Y   "));
    assert!(!bm_yn.matches_true("N"));
    assert!(bm_10.matches_true("1"));
    assert!(bm_tf.matches_true("T"));

    assert!(!is_zero_numeric_str(""));
    assert!(!is_zero_numeric_str("+"));
    assert!(!is_zero_numeric_str("."));
    assert!(is_zero_numeric_str(".0"));
    assert!(!is_zero_numeric_str("0.0.0"));
    assert!(!is_zero_numeric_str("05"));

    assert_eq!(normalize_cobol_number("XML", " -0042.50 ")?, "-42.50");
    assert_eq!(normalize_cobol_number("XML", "+007")?, "7");
    assert_eq!(normalize_cobol_number("XML", "19-")?, "-19");
    assert_eq!(normalize_cobol_number("XML", "19+")?, "19");
    assert_eq!(normalize_cobol_number("XML", ".25")?, "0.25");
    assert_eq!(normalize_cobol_number("XML", "-00.00")?, "0.00");
    assert_eq!(normalize_cobol_number("XML", "-0")?, "0");
    assert!(normalize_cobol_number("XML", "").is_err());
    assert!(normalize_cobol_number("XML", ".").is_err());
    assert!(normalize_cobol_number("XML", "12.").is_err());
    assert!(normalize_cobol_number("XML", "12.x").is_err());
    assert!(normalize_cobol_number("XML", "x12").is_err());

    let mappings = vec![NameMapping::new(f_a.clone(), NameOverride::Omitted)];
    assert_eq!(
        resolve_name_override(&mappings, "A"),
        Some(&NameOverride::Omitted)
    );
    assert_eq!(resolve_name_override(&mappings, "Z"), None);

    let elem_t = ParseTargetNode::elementary(f_a, ValueCategory::Alphanumeric, "init", None);
    let tbl_t = ParseTargetNode::table(f_b, vec![elem_t.clone()]);
    let grp_t = ParseTargetNode::group(fname("GRP")?, vec![elem_t.clone(), tbl_t.clone()]);
    assert!(grp_t.child("A").is_some());
    assert!(grp_t.child("MISSING").is_none());
    assert!(elem_t.child("A").is_none());
    assert!(tbl_t.child("A").is_none());
    assert_eq!(elem_t.elementary_value(), Some("init"));
    assert_eq!(grp_t.elementary_value(), None);
    assert_eq!(tbl_t.elementary_value(), None);
    assert_eq!(elem_t.assign_state(), Some(FieldAssignState::Unmodified));
    assert_eq!(grp_t.assign_state(), None);
    assert_eq!(tbl_t.assign_state(), None);
    assert!(
        tbl_t
            .table_elements()
            .is_some_and(|(n, s)| n == 0 && s.len() == 1)
    );
    assert!(elem_t.table_elements().is_none());
    assert!(grp_t.table_elements().is_none());
    Ok(())
}

/// Verifies streaming `XML PARSE` across declarations, DOCTYPE, comments, PIs, CDATA, entities, and namespaces.
#[test]
fn xml_parse_well_formed_prolog_and_elements() -> Result<()> {
    let doc = concat!(
        "\u{FEFF}<?xml version=\u{0022}1.0\u{0022} encoding='UTF-8' standalone=\u{0022}yes\u{0022}?>\u{000A}",
        "<!-- prolog comment -->",
        "<!DOCTYPE order [ <!ENTITY dummy 'x'> <!ELEMENT order ANY> \u{0022}<ignored>\u{0022} ]>",
        "<?audit mode=strict?>",
        "<ns:order xmlns=\u{0022}http://default.ns\u{0022} xmlns:ns=\u{0022}http://order.ns\u{0022} ",
        "id=\u{0022}A&amp;B\u{0009}\u{000D}\u{000A}C\u{0022} ns:flag='&lt;&gt;&quot;&apos;' xml:lang=\u{0022}en\u{0022}>",
        "<item>Line1\u{000D}\u{000A}Line2\u{000D}Line3 &#65; &#x42; &#X43;</item>",
        "<empty/>",
        "<![CDATA[<raw>&cdata]]>",
        "<![CDATA[]]>",
        "<!-- inner -->",
        "<?piempty?>",
        "</ns:order>",
        "<!-- epilog --><?post done?>   "
    );

    let mut seen: Vec<(XmlEventKind, String, String, String)> = Vec::new();
    let outcome = xml_parse(doc, &XmlParseOptions::default(), |regs| {
        assert_eq!(regs.xml_ntext, encode_utf16_units(&regs.xml_text));
        seen.push((
            regs.xml_event,
            regs.xml_text.clone(),
            regs.xml_namespace.clone(),
            regs.xml_namespace_prefix.clone(),
        ));
        Ok(())
    })?;

    assert_eq!(outcome.completion, XmlParseCompletion::Completed);
    assert_eq!(outcome.registers.xml_code, XmlCode::ZERO);
    assert_eq!(outcome.registers.xml_event, XmlEventKind::EndOfDocument);
    assert!(outcome.processed_bytes.get() > 0);
    assert!(
        seen.iter()
            .any(|(e, t, _, _)| *e == XmlEventKind::VersionInformation && t == "1.0")
    );
    assert!(
        seen.iter()
            .any(|(e, t, _, _)| *e == XmlEventKind::EncodingDeclaration && t == "UTF-8")
    );
    assert!(
        seen.iter()
            .any(|(e, t, _, _)| *e == XmlEventKind::StandaloneDeclaration && t == "yes")
    );
    assert!(
        seen.iter()
            .any(|(e, _, _, _)| *e == XmlEventKind::DocumentTypeDeclaration)
    );
    assert!(seen.iter().any(|(e, t, ns, p)| {
        *e == XmlEventKind::StartOfElement && t == "order" && ns == "http://order.ns" && p == "ns"
    }));
    assert!(
        seen.iter()
            .any(|(e, t, _, _)| *e == XmlEventKind::AttributeCharacters && t == "A&B  C")
    );
    assert!(seen.iter().any(|(e, t, _, _)| {
        *e == XmlEventKind::AttributeCharacters && t == "<>\u{0022}\u{0027}"
    }));
    assert!(
        seen.iter()
            .any(|(e, t, _, p)| { *e == XmlEventKind::AttributeName && t == "lang" && p == "xml" })
    );
    assert!(seen.iter().any(|(e, t, ns, _)| {
        *e == XmlEventKind::StartOfElement && t == "item" && ns == "http://default.ns"
    }));
    assert!(seen.iter().any(|(e, t, _, _)| {
        *e == XmlEventKind::ContentCharacters && t == "Line1\u{000A}Line2\u{000A}Line3 A B C"
    }));
    Ok(())
}

/// Verifies `XML PARSE` with disabled namespaces, self-closing root, Unicode names, and user termination.
#[test]
fn xml_parse_disabled_namespaces_unicode_and_user_abort() -> Result<()> {
    let disabled = XmlParseOptions::new().with_namespace_mode(XmlNamespaceMode::Disabled);
    let mut raw_events = Vec::new();
    let out = xml_parse(
        "<?xml version='1.1' standalone='no'?><ns:root xmlns:ns='http://x' a='1'/>",
        &disabled,
        |regs| {
            raw_events.push((
                regs.xml_event,
                regs.xml_text.clone(),
                regs.xml_namespace.clone(),
            ));
            Ok(())
        },
    )?;
    assert_eq!(out.completion, XmlParseCompletion::Completed);
    assert!(raw_events.contains(&(
        XmlEventKind::StartOfElement,
        "ns:root".to_owned(),
        String::new()
    )));
    assert!(raw_events.contains(&(
        XmlEventKind::AttributeName,
        "xmlns:ns".to_owned(),
        String::new()
    )));

    let unicode_out = xml_parse(
        "<?xmlStyle data?><\u{00E9}l\u{00B7}\u{0301}1/>",
        &XmlParseOptions::new(),
        |_| Ok(()),
    )?;
    assert_eq!(unicode_out.completion, XmlParseCompletion::Completed);
    assert!(is_valid_xml_char('A'));
    assert!(!is_valid_xml_char('\u{0000}'));
    assert!(is_xml_name_start('_'));
    assert!(is_xml_name_start(':'));
    assert!(!is_xml_name_start('-'));
    assert!(is_xml_name_char('-'));
    assert!(is_xml_name_char('.'));
    assert!(!is_xml_name_char(' '));

    let mut session = XmlParseSession::new("<root><a/><b/></root>", XmlParseOptions::new());
    assert!(session.next_event(XmlCode::ZERO).is_some());
    let mut second =
        session
            .next_event(XmlCode::ZERO)
            .ok_or_else(|| CoblangError::XmlJsonProcess {
                kind: "XML",
                offset: 0,
                message: "expected StartOfElement".to_owned(),
            })?;
    second.request_terminate();
    assert!(session.next_event(second.xml_code).is_none());
    assert!(session.next_event(XmlCode::ZERO).is_none());
    let term_outcome = session.into_outcome();
    assert_eq!(
        term_outcome.completion,
        XmlParseCompletion::TerminatedByUser
    );
    assert_eq!(term_outcome.registers.xml_code, XmlCode::USER_TERMINATE);

    let custom_out = xml_parse("<root/>", &XmlParseOptions::new(), |regs| {
        if regs.xml_event == XmlEventKind::StartOfElement {
            regs.xml_code = XmlCode::new(77);
        }
        Ok(())
    })?;
    assert_eq!(custom_out.completion, XmlParseCompletion::Exception);
    assert_eq!(custom_out.registers.xml_code, XmlCode::new(77));

    let shared_cb = |regs: &mut coblang_xml_json::XmlRegisters| -> Result<()> {
        if regs.xml_text == "boom" {
            return Err(CoblangError::XmlJsonProcess {
                kind: "XML",
                offset: 0,
                message: "callback error".to_owned(),
            });
        }
        Ok(())
    };
    assert!(
        xml_parse("<ok/>", &XmlParseOptions::new(), shared_cb)?
            .registers
            .xml_code
            .is_ok()
    );
    assert!(xml_parse("<boom/>", &XmlParseOptions::new(), shared_cb).is_err());
    Ok(())
}

/// Verifies `XML PARSE` exception detection across malformed prolog, element, attribute, entity, and namespace inputs.
#[test]
fn xml_parse_malformed_exception_matrix() -> Result<()> {
    let bad_inputs = [
        "",
        "   ",
        "<",
        " <?xml version='1.0'?><r/>",
        "<!--c--><?xml version='1.0'?><r/>",
        "<?xml version='1.0'",
        "<?xml?><r/>",
        "<?xml version='2.0'?><r/>",
        "<?xml encoding='UTF-8'?><r/>",
        "<?xml version='1.0' bad='1'?><r/>",
        "<?xml version='1.0' encoding=''?><r/>",
        "<?xml version='1.0' encoding='UTF-8' encoding='UTF-8'?><r/>",
        "<?xml version='1.0' standalone='maybe'?><r/>",
        "<?xml version='1.0' standalone='yes' standalone='no'?><r/>",
        "<?xml version='1.0' standalone='yes' encoding='UTF-8'?><r/>",
        "<?xml version='1.0'a='1'?><r/>",
        "<?xml version='1.0' 123='1'?><r/>",
        "<?xml version='1.0' encoding?><r/>",
        "<?xml version='1.0' encoding=UTF-8?><r/>",
        "<?xml version='1.0' encoding='UTF-8?><r/>",
        "</orphan>",
        "<!</r>",
        "plain-text-before-root",
        "<!-- unclosed",
        "<!-- bad--comment --><r/>",
        "<!-- trailingdash---><r/>",
        "<!-- \u{0000} --><r/>",
        "<!DOCTYPE><r/>",
        "<!DOCTYPE root",
        "<!DOCTYPE root 'unclosed><r/>",
        "<??>",
        "<?XML bad?><r/>",
        "<?pi unclosed",
        "<?pi\u{0000}?><r/>",
        "<?pi \u{0000}?><r/>",
        "<1bad/>",
        "<r",
        "<r a='1'b='2'/>",
        "<r 1='x'/>",
        "<r dup='1' dup='2'/>",
        "<r noeq/>",
        "<r a=unquoted/>",
        "<r a='unclosed>",
        "<r a='<'/>",
        "<r a='&bad;'/>",
        "<r xmlns:='http://x'/>",
        "<r xmlns:a:b='http://x'/>",
        "<r xmlns:a=''/>",
        "<unbound:r/>",
        "<:bad/>",
        "<a: xmlns:a='http://x'/>",
        "<a:b:c xmlns:a='http://x'/>",
        "<r unbound:attr='1'/>",
        "<r>",
        "<r>unclosed-text-eof",
        "<r><!-- unclosed",
        "<r><![CDATA[unclosed",
        "<r><![CDATA[\u{0000}]]></r>",
        "<r><?xml version='1.0'?></r>",
        "<r><!DOCTYPE inner></r>",
        "<r></>",
        "<r></r",
        "<r></wrong>",
        "<r>bad]]>end</r>",
        "<r>&unterminated</r>",
        "<r>&unknown;</r>",
        "<r>&#0;</r>",
        "<r>&#xD800;</r>",
        "<r>&#notnum;</r>",
        "<r>\u{0000}</r>",
        "<r/><extra/>",
        "<r/>trailing-text",
    ];

    for sample in bad_inputs {
        let res = xml_parse(sample, &XmlParseOptions::new(), |_| Ok(()))?;
        assert_eq!(
            res.completion,
            XmlParseCompletion::Exception,
            "expected exception for sample: {sample:?}"
        );
        assert!(!res.registers.xml_code.is_ok());
    }
    Ok(())
}

/// Verifies `XML GENERATE` across `WITH ATTRIBUTES`, `WITH XML-DECLARATION`, `NAMESPACE`, `TYPE OF`, `NAME OF`, and `SUPPRESS`.
#[test]
fn xml_generate_happy_paths_and_buffers() -> Result<()> {
    let tree = DataNode::group(
        fname("INVOICE")?,
        vec![
            DataNode::elementary(fname("INV-ID")?, ValueCategory::Alphanumeric, "INV-01   "),
            DataNode::elementary(fname("AMOUNT")?, ValueCategory::Numeric, "00125.50"),
            DataNode::elementary(fname("DISCOUNT")?, ValueCategory::Numeric, "000.00"),
            DataNode::elementary(
                fname("NOTE")?,
                ValueCategory::Alphanumeric,
                "A & B < C > \u{0022}D\u{0022} \u{0027}E\u{0027}  ",
            )
            .with_xml_placement(XmlNodePlacement::Element),
            DataNode::group(
                fname("TAGGED")?,
                vec![
                    DataNode::elementary(
                        fname("CODE")?,
                        ValueCategory::Alphanumeric,
                        "X&<>\u{0022}\u{0027}",
                    )
                    .with_xml_placement(XmlNodePlacement::Attribute),
                    DataNode::elementary(
                        fname("BODY")?,
                        ValueCategory::Alphanumeric,
                        "inline<txt>",
                    )
                    .with_xml_placement(XmlNodePlacement::Content),
                ],
            ),
            DataNode::group(
                fname("EMPTY-SUB")?,
                vec![DataNode::elementary(
                    fname("BLANK")?,
                    ValueCategory::Alphanumeric,
                    "    ",
                )],
            ),
            DataNode::table(
                fname("LINES")?,
                vec![
                    DataNode::elementary(fname("LINE")?, ValueCategory::Numeric, "10"),
                    DataNode::elementary(fname("LINE")?, ValueCategory::Numeric, "0"),
                ],
            ),
            DataNode::table(
                fname("SUPP-TBL")?,
                vec![DataNode::elementary(
                    fname("SUPP-OCC")?,
                    ValueCategory::Numeric,
                    "0",
                )],
            ),
        ],
    );

    let opts = XmlGenerateOptions::default()
        .with_attributes(XmlAttributeMode::WithAttributes)
        .with_xml_declaration(XmlDeclarationMode::Include)
        .with_namespace("http://example.com/inv  ", Some("inv  ".to_owned()))
        .with_name_mapping(NameMapping::new(
            fname("INV-ID")?,
            NameOverride::try_rename("id")?,
        ))
        .with_suppress_rule(SuppressRule::for_every(
            SuppressCategoryFilter::NumericOnly,
            SuppressCondition::WhenZero,
        ))
        .with_suppress_rule(SuppressRule::for_every(
            SuppressCategoryFilter::NonNumericOnly,
            SuppressCondition::WhenSpace,
        ));

    let out = xml_generate(&tree, &opts)?;
    assert_eq!(out.xml_code, XmlCode::ZERO);
    assert_eq!(out.count.get(), out.xml.len());
    assert!(
        out.xml
            .starts_with("<?xml version=\u{0022}1.0\u{0022} encoding=\u{0022}UTF-8\u{0022}?>")
    );
    assert!(out.xml.contains("<inv:INVOICE xmlns:inv=\u{0022}http://example.com/inv\u{0022} id=\u{0022}INV-01\u{0022} AMOUNT=\u{0022}125.50\u{0022}>"));
    assert!(!out.xml.contains("DISCOUNT"));
    assert!(!out.xml.contains("EMPTY-SUB"));
    assert!(!out.xml.contains("SUPP-TBL"));
    assert!(out.xml.contains(
        "<inv:NOTE>A &amp; B &lt; C &gt; \u{0022}D\u{0022} \u{0027}E\u{0027}</inv:NOTE>"
    ));
    assert!(out.xml.contains(
        "<inv:TAGGED CODE=\u{0022}X&amp;&lt;&gt;&quot;&apos;\u{0022}>inline&lt;txt&gt;</inv:TAGGED>"
    ));
    assert!(out.xml.contains("<inv:LINE>10</inv:LINE>"));
    Ok(())
}

/// Verifies `XML GENERATE` scalar root, default namespace, suppressed root, and fixed-buffer overflow.
#[test]
fn xml_generate_scalar_root_and_buffers() -> Result<()> {
    let scalar_root = DataNode::elementary(fname("MSG")?, ValueCategory::National, "Hello  ");
    let scalar_out = xml_generate(
        &scalar_root,
        &XmlGenerateOptions::new().with_namespace("http://ns.example", None),
    )?;
    assert_eq!(
        scalar_out.xml,
        "<MSG xmlns=\u{0022}http://ns.example\u{0022}>Hello</MSG>"
    );

    let supp_root_out = xml_generate(
        &scalar_root,
        &XmlGenerateOptions::new().with_suppress_rule(SuppressRule::for_field(
            fname("MSG")?,
            SuppressCondition::Always,
        )),
    )?;
    assert_eq!(
        (supp_root_out.xml.as_str(), supp_root_out.count.get()),
        ("", 0)
    );

    let mut ok_buf = [b' '; 64];
    assert_eq!(
        xml_generate_into_buffer(&scalar_root, &XmlGenerateOptions::new(), &mut ok_buf)?.xml_code,
        XmlCode::ZERO
    );
    let mut tiny_buf = [b' '; 4];
    let ovf_res =
        xml_generate_into_buffer(&scalar_root, &XmlGenerateOptions::new(), &mut tiny_buf)?;
    assert_eq!(ovf_res.xml_code, XmlCode::BUFFER_OVERFLOW);
    assert_eq!(&tiny_buf, b"<MSG");
    Ok(())
}

/// Verifies `XML GENERATE` suppression edge cases and invalid configuration/data errors.
#[test]
fn xml_generate_suppression_edges_and_errors() -> Result<()> {
    let grp_misc = DataNode::group(
        fname("ROOT")?,
        vec![
            DataNode::elementary(fname("C")?, ValueCategory::Alphanumeric, "   ")
                .with_xml_placement(XmlNodePlacement::Content),
            DataNode::group(fname("G")?, vec![]).with_xml_placement(XmlNodePlacement::Element),
            DataNode::table(fname("T")?, vec![]),
            DataNode::group(fname("SG")?, vec![]),
            DataNode::table(fname("ST")?, vec![]),
        ],
    );
    let misc_opts = XmlGenerateOptions::new()
        .with_suppress_rule(SuppressRule::for_field(
            fname("C")?,
            SuppressCondition::WhenSpace,
        ))
        .with_suppress_rule(SuppressRule::for_field(
            fname("SG")?,
            SuppressCondition::Always,
        ))
        .with_suppress_rule(SuppressRule::for_field(
            fname("ST")?,
            SuppressCondition::Always,
        ));
    assert_eq!(
        xml_generate(&grp_misc, &misc_opts)?.xml,
        "<ROOT><G></G></ROOT>"
    );

    assert!(!is_valid_xml_ncname(""));
    assert!(!is_valid_xml_ncname(":a"));
    assert!(!is_valid_xml_ncname("a:b"));
    assert!(is_valid_xml_ncname("valid_name-1"));

    let scalar_root = DataNode::elementary(fname("MSG")?, ValueCategory::Alphanumeric, "ok");
    assert!(
        xml_generate(
            &DataNode::table(fname("TBL")?, vec![]),
            &XmlGenerateOptions::new()
        )
        .is_err()
    );
    assert!(
        xml_generate(
            &scalar_root,
            &XmlGenerateOptions::new().with_namespace("http://bad\u{0000}", None)
        )
        .is_err()
    );
    assert!(
        xml_generate(
            &scalar_root,
            &XmlGenerateOptions::new().with_namespace("   ", Some("p".to_owned()))
        )
        .is_err()
    );
    assert!(
        xml_generate(
            &scalar_root,
            &XmlGenerateOptions::new().with_namespace("http://ok", Some("xml".to_owned()))
        )
        .is_err()
    );
    assert!(
        xml_generate(
            &scalar_root,
            &XmlGenerateOptions::new().with_namespace("http://ok", Some("xmlns".to_owned()))
        )
        .is_err()
    );
    assert!(
        xml_generate(
            &scalar_root,
            &XmlGenerateOptions::new().with_namespace("http://ok", Some("1bad".to_owned()))
        )
        .is_err()
    );
    assert!(
        xml_generate(
            &scalar_root,
            &XmlGenerateOptions::new()
                .with_name_mapping(NameMapping::new(fname("MSG")?, NameOverride::Omitted))
        )
        .is_err()
    );
    assert!(
        xml_generate(
            &scalar_root,
            &XmlGenerateOptions::new().with_name_mapping(NameMapping::new(
                fname("MSG")?,
                NameOverride::try_rename("123bad")?
            ))
        )
        .is_err()
    );

    let bad_ctrl = DataNode::elementary(fname("MSG")?, ValueCategory::Alphanumeric, "a\u{0000}b");
    assert!(xml_generate(&bad_ctrl, &XmlGenerateOptions::new()).is_err());
    Ok(())
}

/// Verifies `XML GENERATE` placement, numeric, and buffer error propagation.
#[test]
fn xml_generate_placement_and_numeric_errors() -> Result<()> {
    let misc_opts = XmlGenerateOptions::new().with_suppress_rule(SuppressRule::for_field(
        fname("C")?,
        SuppressCondition::WhenSpace,
    ));
    let root_only_supp_child = DataNode::group(
        fname("ROOT")?,
        vec![DataNode::elementary(
            fname("C")?,
            ValueCategory::Alphanumeric,
            "  ",
        )],
    );
    assert_eq!(
        xml_generate(&root_only_supp_child, &misc_opts)?.xml,
        "<ROOT></ROOT>"
    );

    let bad_num = DataNode::elementary(fname("MSG")?, ValueCategory::Numeric, "NaN");
    let mut dummy_buf = [0_u8; 8];
    assert!(
        xml_generate_into_buffer(&bad_num, &XmlGenerateOptions::new(), &mut dummy_buf).is_err()
    );
    assert!(xml_generate(&bad_num, &XmlGenerateOptions::new()).is_err());

    let bad_grp_attr = DataNode::group(
        fname("R")?,
        vec![
            DataNode::group(fname("SUB")?, vec![]).with_xml_placement(XmlNodePlacement::Attribute),
        ],
    );
    assert!(xml_generate(&bad_grp_attr, &XmlGenerateOptions::new()).is_err());

    let bad_attr_name = DataNode::group(
        fname("R")?,
        vec![
            DataNode::elementary(fname("A")?, ValueCategory::Alphanumeric, "v")
                .with_xml_placement(XmlNodePlacement::Attribute),
        ],
    );
    assert!(
        xml_generate(
            &bad_attr_name,
            &XmlGenerateOptions::new().with_name_mapping(NameMapping::new(
                fname("A")?,
                NameOverride::try_rename("a:b")?
            ))
        )
        .is_err()
    );

    let bad_attr_val = DataNode::group(
        fname("R")?,
        vec![
            DataNode::elementary(fname("A")?, ValueCategory::Numeric, "bad")
                .with_xml_placement(XmlNodePlacement::Attribute),
        ],
    );
    assert!(xml_generate(&bad_attr_val, &XmlGenerateOptions::new()).is_err());

    let bad_content_val = DataNode::group(
        fname("R")?,
        vec![
            DataNode::elementary(fname("A")?, ValueCategory::Numeric, "bad")
                .with_xml_placement(XmlNodePlacement::Content),
        ],
    );
    assert!(xml_generate(&bad_content_val, &XmlGenerateOptions::new()).is_err());
    assert!(
        xml_generate(
            &DataNode::group(fname("R")?, vec![bad_num.clone()]),
            &XmlGenerateOptions::new()
        )
        .is_err()
    );
    assert!(
        xml_generate(
            &DataNode::group(
                fname("R")?,
                vec![DataNode::table(fname("T")?, vec![bad_num])]
            ),
            &XmlGenerateOptions::new()
        )
        .is_err()
    );
    Ok(())
}

/// Verifies RFC 8259 string escaping for control characters, quotes, and backslashes.
#[test]
fn json_escape_control_chars_and_quotes() {
    let mut esc = String::new();
    escape_json_string_into(
        &mut esc,
        "\u{0022}\u{005C}\u{0008}\u{000C}\u{000A}\u{000D}\u{0009}\u{0001}\u{001F}ok",
    );
    assert_eq!(
        esc,
        "\u{0022}\u{005C}\u{0022}\u{005C}\u{005C}\u{005C}b\u{005C}f\u{005C}n\u{005C}r\u{005C}t\u{005C}u0001\u{005C}u001fok\u{0022}"
    );
}

/// Verifies `JSON GENERATE` happy paths, `NAME OF ... OMITTED`, `SUPPRESS`, and `CONVERTING`.
#[test]
fn json_generate_happy_paths() -> Result<()> {
    let record = DataNode::group(
        fname("ACCOUNT")?,
        vec![
            DataNode::elementary(fname("ACCT-ID")?, ValueCategory::Numeric, "0042"),
            DataNode::elementary(fname("HOLDER")?, ValueCategory::Alphanumeric, "Ada   "),
            DataNode::elementary(fname("ACTIVE")?, ValueCategory::Alphanumeric, "Y"),
            DataNode::elementary(fname("CLOSED")?, ValueCategory::Alphanumeric, "N"),
            DataNode::elementary(fname("SKIP-ME")?, ValueCategory::Alphanumeric, "   "),
            DataNode::elementary(fname("REN-SUPP")?, ValueCategory::Alphanumeric, "   "),
            DataNode::group(fname("EMPTY-OBJ")?, vec![]),
            DataNode::table(fname("EMPTY-ARR")?, vec![]),
            DataNode::group(
                fname("FLAT-WRAP")?,
                vec![DataNode::elementary(
                    fname("FLAT-VAL")?,
                    ValueCategory::National,
                    "Zed  ",
                )],
            ),
            DataNode::group(
                fname("SUPP-WRAP")?,
                vec![DataNode::elementary(
                    fname("X")?,
                    ValueCategory::Alphanumeric,
                    "v",
                )],
            ),
            DataNode::group(
                fname("EMPTY-SUB")?,
                vec![DataNode::elementary(
                    fname("BLANK")?,
                    ValueCategory::Alphanumeric,
                    "  ",
                )],
            ),
            DataNode::table(
                fname("TAGS")?,
                vec![
                    DataNode::elementary(fname("TAG")?, ValueCategory::Alphanumeric, "vip "),
                    DataNode::elementary(fname("TAG")?, ValueCategory::Alphanumeric, "   "),
                ],
            ),
            DataNode::table(
                fname("ALL-SUPP-TAGS")?,
                vec![DataNode::elementary(
                    fname("TAG")?,
                    ValueCategory::Alphanumeric,
                    "  ",
                )],
            ),
            DataNode::table(fname("DIRECT-SUPP-TBL")?, vec![]),
            DataNode::group(fname("DIRECT-SUPP-GRP")?, vec![]),
        ],
    );

    let opts = JsonGenerateOptions::default()
        .with_name_mapping(NameMapping::new(
            fname("ACCT-ID")?,
            NameOverride::try_rename("id")?,
        ))
        .with_name_mapping(NameMapping::new(
            fname("REN-SUPP")?,
            NameOverride::try_rename("rs")?,
        ))
        .with_name_mapping(NameMapping::new(fname("FLAT-WRAP")?, NameOverride::Omitted))
        .with_name_mapping(NameMapping::new(fname("SUPP-WRAP")?, NameOverride::Omitted))
        .with_suppress_rule(SuppressRule::for_every(
            SuppressCategoryFilter::NonNumericOnly,
            SuppressCondition::WhenSpace,
        ))
        .with_suppress_rule(SuppressRule::for_field(
            fname("SUPP-WRAP")?,
            SuppressCondition::Always,
        ))
        .with_suppress_rule(SuppressRule::for_field(
            fname("DIRECT-SUPP-TBL")?,
            SuppressCondition::Always,
        ))
        .with_suppress_rule(SuppressRule::for_field(
            fname("DIRECT-SUPP-GRP")?,
            SuppressCondition::Always,
        ))
        .with_boolean_mapping(JsonBooleanMapping::yes_no(fname("ACTIVE")?))
        .with_boolean_mapping(JsonBooleanMapping::yes_no(fname("CLOSED")?));

    let wrapped = json_generate(&record, &opts)?;
    assert_eq!(wrapped.json_code, JsonCode::ZERO);
    assert_eq!(wrapped.json_status, JsonStatus::ZERO);
    assert_eq!(wrapped.count.get(), wrapped.json.len());
    assert_eq!(
        wrapped.json,
        "{\u{0022}ACCOUNT\u{0022}:{\u{0022}id\u{0022}:42,\u{0022}HOLDER\u{0022}:\u{0022}Ada\u{0022},\u{0022}ACTIVE\u{0022}:true,\u{0022}CLOSED\u{0022}:false,\u{0022}EMPTY-OBJ\u{0022}:{},\u{0022}EMPTY-ARR\u{0022}:[],\u{0022}FLAT-VAL\u{0022}:\u{0022}Zed\u{0022},\u{0022}TAGS\u{0022}:[\u{0022}vip\u{0022}]}}"
    );

    let unwrapped = json_generate(
        &record,
        &opts.with_name_mapping(NameMapping::new(fname("ACCOUNT")?, NameOverride::Omitted)),
    )?;
    assert!(unwrapped.json.starts_with("{\u{0022}id\u{0022}:42,"));
    Ok(())
}

/// Verifies `JSON GENERATE` root variants, fixed-buffer overflow, and error propagation.
#[test]
fn json_generate_buffers_and_error_cases() -> Result<()> {
    let msg_node = DataNode::elementary(fname("MSG")?, ValueCategory::Alphanumeric, "hi");
    let renamed = json_generate(
        &msg_node,
        &JsonGenerateOptions::new().with_name_mapping(NameMapping::new(
            fname("MSG")?,
            NameOverride::try_rename("message")?,
        )),
    )?;
    assert_eq!(renamed.json, "{\u{0022}message\u{0022}:\u{0022}hi\u{0022}}");

    let supp_msg = json_generate(
        &msg_node,
        &JsonGenerateOptions::new().with_suppress_rule(SuppressRule::for_field(
            fname("MSG")?,
            SuppressCondition::Always,
        )),
    )?;
    assert_eq!(supp_msg.json, "");

    let all_supp_grp = DataNode::group(
        fname("G")?,
        vec![DataNode::elementary(
            fname("A")?,
            ValueCategory::Alphanumeric,
            "  ",
        )],
    );
    let top_unwrapped_empty = json_generate(
        &all_supp_grp,
        &JsonGenerateOptions::new()
            .with_name_mapping(NameMapping::new(fname("G")?, NameOverride::Omitted))
            .with_suppress_rule(SuppressRule::for_field(
                fname("A")?,
                SuppressCondition::WhenSpace,
            )),
    )?;
    assert_eq!(top_unwrapped_empty.json, "{}");

    let two_items = DataNode::table(
        fname("ARR")?,
        vec![
            DataNode::elementary(fname("E")?, ValueCategory::Numeric, "1"),
            DataNode::elementary(fname("E")?, ValueCategory::Numeric, "2"),
        ],
    );
    let arr_out = json_generate(
        &two_items,
        &JsonGenerateOptions::new()
            .with_name_mapping(NameMapping::new(fname("ARR")?, NameOverride::Omitted)),
    )?;
    assert_eq!(arr_out.json, "[1,2]");

    let mut big_buf = [b' '; 64];
    assert_eq!(
        json_generate_into_buffer(&msg_node, &JsonGenerateOptions::new(), &mut big_buf)?.json_code,
        JsonCode::ZERO
    );
    let mut small_buf = [b' '; 4];
    let ovf_res =
        json_generate_into_buffer(&msg_node, &JsonGenerateOptions::new(), &mut small_buf)?;
    assert_eq!(ovf_res.json_code, JsonCode::BUFFER_OVERFLOW);
    assert_eq!(ovf_res.json_status, JsonStatus::BUFFER_OVERFLOW);

    assert!(
        json_generate(
            &DataNode::group(fname("G")?, vec![msg_node]),
            &JsonGenerateOptions::new()
                .with_name_mapping(NameMapping::new(fname("MSG")?, NameOverride::Omitted))
        )
        .is_err()
    );

    let top_unwrapped_arr = json_generate(
        &DataNode::table(
            fname("ARR")?,
            vec![DataNode::elementary(
                fname("A")?,
                ValueCategory::Alphanumeric,
                "  ",
            )],
        ),
        &JsonGenerateOptions::new()
            .with_name_mapping(NameMapping::new(fname("ARR")?, NameOverride::Omitted))
            .with_suppress_rule(SuppressRule::for_field(
                fname("A")?,
                SuppressCondition::WhenSpace,
            )),
    )?;
    assert_eq!(top_unwrapped_arr.json, "[]");
    Ok(())
}

/// Verifies `JSON GENERATE` invalid numeric and `OMITTED` error propagation.
#[test]
fn json_generate_numeric_error_propagation() -> Result<()> {
    let bad_num_node = DataNode::elementary(fname("N")?, ValueCategory::Numeric, "xyz");
    let mut err_buf = [0_u8; 8];
    assert!(
        json_generate_into_buffer(&bad_num_node, &JsonGenerateOptions::new(), &mut err_buf)
            .is_err()
    );
    assert!(json_generate(&bad_num_node, &JsonGenerateOptions::new()).is_err());
    assert!(
        json_generate(
            &bad_num_node,
            &JsonGenerateOptions::new()
                .with_name_mapping(NameMapping::new(fname("N")?, NameOverride::Omitted))
        )
        .is_err()
    );
    assert!(
        json_generate(
            &bad_num_node,
            &JsonGenerateOptions::new().with_name_mapping(NameMapping::new(
                fname("N")?,
                NameOverride::try_rename("num")?
            ))
        )
        .is_err()
    );
    let bad_grp_num = DataNode::group(fname("G")?, vec![bad_num_node.clone()]);
    assert!(json_generate(&bad_grp_num, &JsonGenerateOptions::new()).is_err());
    assert!(
        json_generate(
            &bad_grp_num,
            &JsonGenerateOptions::new().with_name_mapping(NameMapping::new(
                fname("N")?,
                NameOverride::try_rename("num")?
            ))
        )
        .is_err()
    );
    assert!(
        json_generate(
            &DataNode::group(fname("OUTER")?, vec![bad_grp_num]),
            &JsonGenerateOptions::new()
                .with_name_mapping(NameMapping::new(fname("G")?, NameOverride::Omitted))
        )
        .is_err()
    );
    assert!(
        json_generate(
            &DataNode::table(fname("T")?, vec![bad_num_node]),
            &JsonGenerateOptions::new()
                .with_name_mapping(NameMapping::new(fname("T")?, NameOverride::Omitted))
        )
        .is_err()
    );
    Ok(())
}

/// Verifies strict RFC 8259 `parse_json_document` grammar, escapes, surrogate pairs, and syntax errors.
#[test]
fn rfc8259_document_parser_grammar_and_errors() -> Result<()> {
    let doc = parse_json_document(concat!(
        " { \u{0022}n\u{0022}: null, \u{0022}t\u{0022}: true, \u{0022}f\u{0022}: false, ",
        "\u{0022}nums\u{0022}: [0, -0.5, 42, 1.25e+2, 5E-2], ",
        "\u{0022}empty_arr\u{0022}: [], \u{0022}empty_obj\u{0022}: {}, ",
        "\u{0022}esc\u{0022}: \u{0022}\u{005C}\u{0022}\u{005C}\u{005C}\u{005C}/\u{005C}b\u{005C}f\u{005C}n\u{005C}r\u{005C}t\u{005C}u0041\u{005C}uD83D\u{005C}uDE00\u{0022} } "
    ))?;
    assert!(matches!(doc, JsonValue::Object(_)));

    let invalid_json_samples = [
        "",
        "   ",
        "true false",
        "nul",
        "tru",
        "fals",
        "@",
        "[1 2]",
        "[1,]",
        "{1:2}",
        "{\u{0022}a\u{0022} 1}",
        "{\u{0022}a\u{0022}:1 \u{0022}b\u{0022}:2}",
        "{\u{0022}a\u{0022}:1,}",
        "{\u{0022}unterm_key",
        "{\u{0022}a\u{0022}:@}",
        "\u{0022}unterminated",
        "\u{0022}\u{0001}\u{0022}",
        "\u{0022}\u{005C}",
        "\u{0022}\u{005C}q\u{0022}",
        "\u{0022}\u{005C}u12\u{0022}",
        "\u{0022}\u{005C}uZZZZ\u{0022}",
        "\u{0022}\u{005C}uD83D\u{0022}",
        "\u{0022}\u{005C}uD83D\u{005C}uZZZZ\u{0022}",
        "\u{0022}\u{005C}uD83D\u{005C}u0041\u{0022}",
        "\u{0022}\u{005C}uDC00\u{0022}",
        "-",
        "01",
        "1.",
        "1e",
        "1e+",
    ];
    for bad in invalid_json_samples {
        assert!(
            parse_json_document(bad).is_err(),
            "expected RFC 8259 error for: {bad:?}"
        );
    }
    Ok(())
}

/// Builds the rich `CLIENT` receiving hierarchy for `JSON PARSE` tests.
fn build_client_target() -> Result<ParseTargetNode> {
    Ok(ParseTargetNode::group(
        fname("CLIENT")?,
        vec![
            ParseTargetNode::elementary(
                fname("NAME")?,
                ValueCategory::Alphanumeric,
                "OLD",
                Some(5),
            ),
            ParseTargetNode::elementary(fname("AGE")?, ValueCategory::Numeric, "0", None),
            ParseTargetNode::elementary(fname("RATE")?, ValueCategory::Numeric, "0", None),
            ParseTargetNode::elementary(fname("TINY")?, ValueCategory::Numeric, "0", None),
            ParseTargetNode::elementary(fname("HUGE")?, ValueCategory::Numeric, "0", None),
            ParseTargetNode::elementary(fname("VIP")?, ValueCategory::Alphanumeric, "?", None),
            ParseTargetNode::elementary(fname("FROZEN")?, ValueCategory::Alphanumeric, "?", None),
            ParseTargetNode::elementary(
                fname("UNTOUCHED")?,
                ValueCategory::Alphanumeric,
                "KEEP",
                None,
            ),
            ParseTargetNode::elementary(
                fname("SECRET")?,
                ValueCategory::Alphanumeric,
                "ORIG",
                None,
            ),
            ParseTargetNode::elementary(
                fname("NUM-AS-STR")?,
                ValueCategory::Alphanumeric,
                "",
                None,
            ),
            ParseTargetNode::elementary(fname("BOOL-AS-STR")?, ValueCategory::National, "", None),
            ParseTargetNode::elementary(fname("BOOL-F-STR")?, ValueCategory::National, "", None),
            ParseTargetNode::group(
                fname("FLAT-GRP")?,
                vec![
                    ParseTargetNode::elementary(
                        fname("CITY")?,
                        ValueCategory::Alphanumeric,
                        "",
                        None,
                    ),
                    ParseTargetNode::group(
                        fname("INNER-FLAT")?,
                        vec![ParseTargetNode::elementary(
                            fname("ZIP")?,
                            ValueCategory::Alphanumeric,
                            "",
                            None,
                        )],
                    ),
                ],
            ),
            ParseTargetNode::table(
                fname("SCORES")?,
                vec![
                    ParseTargetNode::elementary(fname("S")?, ValueCategory::Numeric, "0", None),
                    ParseTargetNode::elementary(fname("S")?, ValueCategory::Numeric, "0", None),
                ],
            ),
        ],
    ))
}

/// Verifies `JSON PARSE` mapping, `NAME OF` / `OMITTED`, `SUPPRESS`, `CONVERTING`, `WITH DETAIL`, and truncation.
#[test]
fn json_parse_mapping_and_detail_warnings() -> Result<()> {
    let mut target = build_client_target()?;
    let opts = JsonParseOptions::default()
        .with_detail_mode(JsonDetailMode::WithDetail)
        .with_name_mapping(NameMapping::new(
            fname("NAME")?,
            NameOverride::try_rename("fullName")?,
        ))
        .with_name_mapping(NameMapping::new(fname("FLAT-GRP")?, NameOverride::Omitted))
        .with_name_mapping(NameMapping::new(
            fname("INNER-FLAT")?,
            NameOverride::Omitted,
        ))
        .with_name_mapping(NameMapping::new(
            fname("ZIP")?,
            NameOverride::try_rename("postalCode")?,
        ))
        .with_suppressed_field(fname("SECRET")?)
        .with_boolean_mapping(JsonBooleanMapping::yes_no(fname("VIP")?))
        .with_boolean_mapping(JsonBooleanMapping::yes_no(fname("FROZEN")?));

    let payload = concat!(
        "{\u{0022}CLIENT\u{0022}:{",
        "\u{0022}fullName\u{0022}:\u{0022}Alexander\u{0022},",
        "\u{0022}AGE\u{0022}:\u{0022}-2.5e1\u{0022},",
        "\u{0022}RATE\u{0022}:1.234E1,",
        "\u{0022}TINY\u{0022}:5e-2,",
        "\u{0022}HUGE\u{0022}:3e2,",
        "\u{0022}VIP\u{0022}:true,",
        "\u{0022}FROZEN\u{0022}:false,",
        "\u{0022}UNTOUCHED\u{0022}:null,",
        "\u{0022}SECRET\u{0022}:\u{0022}HACK\u{0022},",
        "\u{0022}NUM-AS-STR\u{0022}:99,",
        "\u{0022}BOOL-AS-STR\u{0022}:true,",
        "\u{0022}BOOL-F-STR\u{0022}:false,",
        "\u{0022}CITY\u{0022}:\u{0022}Paris\u{0022},",
        "\u{0022}postalCode\u{0022}:\u{0022}75001\u{0022},",
        "\u{0022}SCORES\u{0022}:[10,20,30],",
        "\u{0022}EXTRA\u{0022}:1",
        "},\u{0022}TOP-EXTRA\u{0022}:2}"
    );

    let out = json_parse(payload, &mut target, &opts)?;
    assert_eq!(out.completion, JsonParseCompletion::CompletedWithWarnings);
    assert_eq!(out.json_code, JsonCode::ZERO);
    assert_eq!(out.json_status, JsonStatus::TRUNCATED_VALUE);
    verify_populated_client(&target);
    Ok(())
}

/// Asserts expected field values on the populated `CLIENT` target node.
fn verify_populated_client(target: &ParseTargetNode) {
    let val = |k: &str| target.child(k).and_then(ParseTargetNode::elementary_value);
    assert_eq!(val("NAME"), Some("Alexa"));
    assert_eq!(val("AGE"), Some("-25"));
    assert_eq!(val("RATE"), Some("12.34"));
    assert_eq!(val("TINY"), Some("0.05"));
    assert_eq!(val("HUGE"), Some("300"));
    assert_eq!(val("VIP"), Some("Y"));
    assert_eq!(val("FROZEN"), Some("N"));
    assert_eq!(val("UNTOUCHED"), Some("KEEP"));
    assert_eq!(
        target
            .child("UNTOUCHED")
            .and_then(ParseTargetNode::assign_state),
        Some(FieldAssignState::Unmodified)
    );
    assert_eq!(val("SECRET"), Some("ORIG"));
    assert_eq!(val("NUM-AS-STR"), Some("99"));
    assert_eq!(val("BOOL-AS-STR"), Some("true"));
    assert_eq!(val("BOOL-F-STR"), Some("false"));
    assert_eq!(
        target
            .child("FLAT-GRP")
            .and_then(|g| g.child("CITY"))
            .and_then(ParseTargetNode::elementary_value),
        Some("Paris")
    );
    assert_eq!(
        target
            .child("FLAT-GRP")
            .and_then(|g| g.child("INNER-FLAT"))
            .and_then(|g| g.child("ZIP"))
            .and_then(ParseTargetNode::elementary_value),
        Some("75001")
    );
    assert_eq!(
        target
            .child("SCORES")
            .and_then(ParseTargetNode::table_elements)
            .map(|(c, _)| c),
        Some(2)
    );
}

/// Verifies `JSON PARSE` clean completion, syntax errors, conversion exceptions, and invalid option validation.
#[test]
fn json_parse_exceptions_and_validation() -> Result<()> {
    let mut simple = ParseTargetNode::elementary(fname("X")?, ValueCategory::Numeric, "0", None);
    let clean_res = json_parse(
        "{\u{0022}rx\u{0022}:7,\u{0022}ignored\u{0022}:1}",
        &mut simple,
        &JsonParseOptions::new().with_name_mapping(NameMapping::new(
            fname("X")?,
            NameOverride::try_rename("rx")?,
        )),
    )?;
    assert_eq!(clean_res.completion, JsonParseCompletion::Completed);
    assert_eq!(simple.elementary_value(), Some("7"));

    assert_eq!(
        json_parse("null", &mut simple, &JsonParseOptions::new())?.completion,
        JsonParseCompletion::Completed
    );

    let mut omit_t = ParseTargetNode::group(
        fname("ROOT")?,
        vec![
            ParseTargetNode::elementary(fname("K")?, ValueCategory::Alphanumeric, "", Some(5)),
            ParseTargetNode::table(
                fname("ARR")?,
                vec![ParseTargetNode::elementary(
                    fname("E")?,
                    ValueCategory::Numeric,
                    "0",
                    None,
                )],
            ),
            ParseTargetNode::group(
                fname("SUPP-SUB")?,
                vec![ParseTargetNode::elementary(
                    fname("Z")?,
                    ValueCategory::Alphanumeric,
                    "0",
                    None,
                )],
            ),
        ],
    );
    let omit_res = json_parse(
        "{\u{0022}K\u{0022}:\u{0022}ok\u{0022},\u{0022}ARR\u{0022}:[4],\u{0022}Z\u{0022}:\u{0022}skip\u{0022},\u{0022}UNSEEN\u{0022}:1}",
        &mut omit_t,
        &JsonParseOptions::new()
            .with_name_mapping(NameMapping::new(fname("ROOT")?, NameOverride::Omitted))
            .with_name_mapping(NameMapping::new(fname("SUPP-SUB")?, NameOverride::Omitted))
            .with_suppressed_field(fname("SUPP-SUB")?),
    )?;
    assert_eq!(omit_res.completion, JsonParseCompletion::Completed);

    let syn_out = json_parse("{bad", &mut simple, &JsonParseOptions::new())?;
    assert_eq!(syn_out.completion, JsonParseCompletion::Exception);
    assert_eq!(syn_out.json_code, JsonCode::SYNTAX_ERROR);
    assert!(syn_out.error_offset.is_some());

    assert_eq!(
        json_parse("[1,2]", &mut simple, &JsonParseOptions::new())?.completion,
        JsonParseCompletion::Exception
    );
    Ok(())
}

/// Verifies `JSON PARSE` type-conversion errors and invalid `OMITTED` target option rejection.
#[test]
fn json_parse_conversion_errors_and_invalid_options() -> Result<()> {
    let mut simple = ParseTargetNode::elementary(fname("X")?, ValueCategory::Numeric, "0", None);

    let mut err_target = ParseTargetNode::group(
        fname("G")?,
        vec![
            ParseTargetNode::group(fname("SUB")?, vec![]),
            ParseTargetNode::table(fname("TBL")?, vec![]),
            ParseTargetNode::elementary(fname("BM")?, ValueCategory::Alphanumeric, "", None),
            ParseTargetNode::elementary(fname("N1")?, ValueCategory::Numeric, "0", None),
            ParseTargetNode::elementary(fname("N2")?, ValueCategory::Numeric, "0", None),
            ParseTargetNode::elementary(fname("N3")?, ValueCategory::Numeric, "0", None),
            ParseTargetNode::elementary(fname("S1")?, ValueCategory::Alphanumeric, "", None),
        ],
    );
    let err_opts = JsonParseOptions::new()
        .with_name_mapping(NameMapping::new(fname("G")?, NameOverride::Omitted))
        .with_boolean_mapping(JsonBooleanMapping::one_zero(fname("BM")?));
    let conv_out = json_parse(
        concat!(
            "{\u{0022}SUB\u{0022}:123,\u{0022}TBL\u{0022}:456,\u{0022}BM\u{0022}:\u{0022}notbool\u{0022},",
            "\u{0022}N1\u{0022}:\u{0022}abc\u{0022},\u{0022}N2\u{0022}:\u{0022}1e999999999999\u{0022},",
            "\u{0022}N3\u{0022}:true,\u{0022}S1\u{0022}:[1]}"
        ),
        &mut err_target,
        &err_opts,
    )?;
    assert_eq!(conv_out.completion, JsonParseCompletion::Exception);
    assert_eq!(conv_out.json_code, JsonCode::CONVERSION_ERROR);

    let bad_mantissa = json_parse(
        "\u{0022}badE2\u{0022}",
        &mut simple,
        &JsonParseOptions::new()
            .with_name_mapping(NameMapping::new(fname("X")?, NameOverride::Omitted)),
    )?;
    assert_eq!(bad_mantissa.completion, JsonParseCompletion::Exception);

    let mut invalid_target = ParseTargetNode::group(
        fname("G")?,
        vec![ParseTargetNode::elementary(
            fname("SC")?,
            ValueCategory::Alphanumeric,
            "",
            None,
        )],
    );
    let invalid_opts = JsonParseOptions::new()
        .with_name_mapping(NameMapping::new(fname("SC")?, NameOverride::Omitted));
    assert!(json_parse("{}", &mut invalid_target, &invalid_opts).is_err());

    // Also exercise table element validation loop in `validate_target_overrides`.
    let mut tbl_with_invalid = ParseTargetNode::table(fname("T")?, vec![invalid_target]);
    assert!(json_parse("[]", &mut tbl_with_invalid, &invalid_opts).is_err());
    Ok(())
}
