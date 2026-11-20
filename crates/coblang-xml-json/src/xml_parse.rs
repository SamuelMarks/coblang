//! Pure-Rust streaming `XML PARSE` engine (`Milestone M20`).
//!
//! Populates COBOL special registers `XML-EVENT`, `XML-CODE`, `XML-TEXT`, `XML-NTEXT`,
//! `XML-NAMESPACE`, `XML-NNAMESPACE`, `XML-NAMESPACE-PREFIX`, and `XML-NNAMESPACE-PREFIX`,
//! supporting both pull-based stepping (`XmlParseSession`) and callback-driven
//! `PROCESSING PROCEDURE` execution (`xml_parse`).

use std::collections::VecDeque;

use coblang_common::Result;

use crate::types::{ByteCount, XmlCode, XmlEventKind, encode_utf16_units};

/// Built-in W3C XML namespace URI bound to the reserved `xml` prefix.
const W3C_XML_NAMESPACE_URI: &str = "http://www.w3.org/XML/1998/namespace";
/// Horizontal tab (`U+0009`).
const CH_TAB: char = '\u{0009}';
/// Line feed (`U+000A`).
const CH_LF: char = '\u{000A}';
/// Carriage return (`U+000D`).
const CH_CR: char = '\u{000D}';
/// Double quote (`U+0022`).
const CH_DQUOTE: char = '\u{0022}';
/// Single quote / apostrophe (`U+0027`).
const CH_SQUOTE: char = '\u{0027}';
/// `CRLF` two-character line ending (`U+000D` followed by `U+000A`).
const CRLF_STR: &str = "\u{000D}\u{000A}";

/// Controls XML Namespaces 1.0 (`xmlns` / `xmlns:prefix`) resolution during `XML PARSE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XmlNamespaceMode {
    /// Resolves `xmlns` declarations, populates `XML-NAMESPACE*` registers, and splits `QName`s.
    Enabled,
    /// Treats `xmlns` attributes as ordinary attributes and keeps unsplit `QName`s in `XML-TEXT`.
    Disabled,
}

/// Configuration options for streaming `XML PARSE`.
#[derive(Debug, Clone)]
pub struct XmlParseOptions {
    /// XML namespace resolution policy.
    pub namespace_mode: XmlNamespaceMode,
}

impl Default for XmlParseOptions {
    /// Returns default `XML PARSE` options with [`XmlNamespaceMode::Enabled`].
    fn default() -> Self {
        Self::new()
    }
}

impl XmlParseOptions {
    /// Constructs default [`XmlParseOptions`] (`XmlNamespaceMode::Enabled`).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            namespace_mode: XmlNamespaceMode::Enabled,
        }
    }

    /// Sets the [`XmlNamespaceMode`] on `self`.
    ///
    /// # Arguments
    /// * `mode` - Desired namespace resolution policy.
    #[must_use]
    pub const fn with_namespace_mode(mut self, mode: XmlNamespaceMode) -> Self {
        self.namespace_mode = mode;
        self
    }
}

/// Snapshot of COBOL XML special registers delivered to the `PROCESSING PROCEDURE` on each event.
#[derive(Debug, Clone)]
pub struct XmlRegisters {
    /// Current `XML-EVENT` register classification.
    pub xml_event: XmlEventKind,
    /// Current `XML-CODE` register (`0` for normal events, `> 0` on `EXCEPTION`, `-1` to abort).
    pub xml_code: XmlCode,
    /// Alphanumeric `XML-TEXT` register payload (`UTF-8`).
    pub xml_text: String,
    /// National `XML-NTEXT` register payload (`UTF-16` code units).
    pub xml_ntext: Vec<u16>,
    /// Alphanumeric `XML-NAMESPACE` URI register (`UTF-8`).
    pub xml_namespace: String,
    /// National `XML-NNAMESPACE` URI register (`UTF-16` code units).
    pub xml_nnamespace: Vec<u16>,
    /// Alphanumeric `XML-NAMESPACE-PREFIX` register (`UTF-8`).
    pub xml_namespace_prefix: String,
    /// National `XML-NNAMESPACE-PREFIX` register (`UTF-16` code units).
    pub xml_nnamespace_prefix: Vec<u16>,
}

impl XmlRegisters {
    /// Constructs a populated [`XmlRegisters`] snapshot, computing all `UTF-16` national mirrors.
    ///
    /// # Arguments
    /// * `xml_event` - Emitted [`XmlEventKind`].
    /// * `xml_code` - Initial [`XmlCode`] for this event.
    /// * `text` - `XML-TEXT` string value.
    /// * `namespace` - `XML-NAMESPACE` URI string.
    /// * `prefix` - `XML-NAMESPACE-PREFIX` string.
    #[must_use]
    pub fn from_parts(
        xml_event: XmlEventKind,
        xml_code: XmlCode,
        text: impl Into<String>,
        namespace: impl Into<String>,
        prefix: impl Into<String>,
    ) -> Self {
        let utf8_text = text.into();
        let utf16_text = encode_utf16_units(&utf8_text);
        let utf8_ns = namespace.into();
        let utf16_ns = encode_utf16_units(&utf8_ns);
        let utf8_pfx = prefix.into();
        let utf16_pfx = encode_utf16_units(&utf8_pfx);
        Self {
            xml_event,
            xml_code,
            xml_text: utf8_text,
            xml_ntext: utf16_text,
            xml_namespace: utf8_ns,
            xml_nnamespace: utf16_ns,
            xml_namespace_prefix: utf8_pfx,
            xml_nnamespace_prefix: utf16_pfx,
        }
    }

    /// Requests graceful early termination (`MOVE -1 TO XML-CODE`).
    pub const fn request_terminate(&mut self) {
        self.xml_code = XmlCode::USER_TERMINATE;
    }
}

/// Terminal disposition of an `XML PARSE` statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XmlParseCompletion {
    /// Reached `END-OF-DOCUMENT` cleanly with `XML-CODE = 0` (`NOT ON EXCEPTION` path).
    Completed,
    /// Aborted immediately after user procedure set `XML-CODE = -1` (`ON EXCEPTION` path).
    TerminatedByUser,
    /// Halted due to XML well-formedness/namespace `EXCEPTION` or non-zero user `XML-CODE`.
    Exception,
}

/// Final result returned after `XML PARSE` finishes.
#[derive(Debug, Clone)]
pub struct XmlParseOutcome {
    /// Terminal completion state (`Completed`, `TerminatedByUser`, or `Exception`).
    pub completion: XmlParseCompletion,
    /// Final special-register state upon completion.
    pub registers: XmlRegisters,
    /// Byte offset reached in the XML input stream.
    pub processed_bytes: ByteCount,
}

/// Active open XML element record on the parser stack.
#[derive(Debug, Clone)]
struct OpenElement {
    /// Raw `QName` written in the start tag (e.g. `"ns:item"` or `"item"`).
    raw_qname: String,
    /// Emitted local/element name stored in `XML-TEXT`.
    emitted_name: String,
    /// Resolved namespace URI for `END-OF-ELEMENT`.
    namespace_uri: String,
    /// Resolved namespace prefix for `END-OF-ELEMENT`.
    prefix: String,
    /// Snapshot length of the namespace binding stack before this element opened.
    ns_mark: usize,
}

/// Queued parser action allowing multi-part constructs (attributes, empty tags) to stream one by one.
#[derive(Debug, Clone)]
struct QueuedItem {
    /// Registers to expose when popped.
    registers: XmlRegisters,
    /// Optional namespace binding stack truncation depth applied when popped.
    ns_restore: Option<usize>,
}

/// Active document sub-phase while scanning XML tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivePhase {
    /// Prolog before the root element opens.
    Prolog,
    /// Inside one or more open XML elements.
    Body,
    /// After the root element has closed, scanning trailing comments/PIs/whitespace.
    Epilog,
}

/// Internal lifecycle state of [`XmlParseSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPhase {
    /// Initial state before `START-OF-DOCUMENT` is emitted.
    NotStarted,
    /// Actively scanning XML prolog, body, or epilog.
    Active(ActivePhase),
    /// Terminal state after `END-OF-DOCUMENT`, `EXCEPTION`, or user `-1` abort.
    Finished,
}

/// Stateful pull-based streaming XML 1.0 + Namespaces parser for COBOL `XML PARSE`.
#[derive(Debug, Clone)]
pub struct XmlParseSession {
    /// Complete UTF-8 XML input document.
    source: String,
    /// Current byte cursor in `source`.
    pos: usize,
    /// Parser options.
    options: XmlParseOptions,
    /// Current high-level document phase.
    phase: SessionPhase,
    /// Buffered events generated by the latest scanned construct.
    queue: VecDeque<QueuedItem>,
    /// Open element stack for matching `</tag>` validation.
    open_elements: Vec<OpenElement>,
    /// Scoped `(prefix, namespace_uri)` bindings (`""` key denotes default `xmlns="..."`).
    ns_bindings: Vec<(String, String)>,
    /// Last emitted register state.
    last_registers: XmlRegisters,
    /// Terminal disposition once `phase == SessionPhase::Finished`.
    completion: XmlParseCompletion,
}

impl XmlParseSession {
    /// Creates a new streaming [`XmlParseSession`] over `source`.
    ///
    /// # Arguments
    /// * `source` - Input XML document text.
    /// * `options` - `XML PARSE` configuration options.
    #[must_use]
    pub fn new(source: impl Into<String>, options: XmlParseOptions) -> Self {
        let owned = source.into();
        let initial_pos = usize::from(owned.starts_with('\u{FEFF}')) * '\u{FEFF}'.len_utf8();
        Self {
            source: owned,
            pos: initial_pos,
            options,
            phase: SessionPhase::NotStarted,
            queue: VecDeque::new(),
            open_elements: Vec::new(),
            ns_bindings: Vec::new(),
            last_registers: XmlRegisters::from_parts(
                XmlEventKind::StartOfDocument,
                XmlCode::ZERO,
                "",
                "",
                "",
            ),
            completion: XmlParseCompletion::Completed,
        }
    }

    /// Advances the streaming parser by one COBOL `XML-EVENT`, honoring `current_xml_code`
    /// returned from the prior `PROCESSING PROCEDURE` invocation.
    ///
    /// # Arguments
    /// * `current_xml_code` - `XML-CODE` value left by the caller after the previous event
    ///   (ignored prior to `START-OF-DOCUMENT`).
    pub fn next_event(&mut self, current_xml_code: XmlCode) -> Option<XmlRegisters> {
        if self.phase == SessionPhase::NotStarted {
            self.phase = SessionPhase::Active(ActivePhase::Prolog);
            let regs =
                XmlRegisters::from_parts(XmlEventKind::StartOfDocument, XmlCode::ZERO, "", "", "");
            self.last_registers.clone_from(&regs);
            return Some(regs);
        }

        let SessionPhase::Active(active) = self.phase else {
            return None;
        };

        self.last_registers.xml_code = current_xml_code;
        if current_xml_code.is_user_terminate() {
            self.queue.clear();
            self.phase = SessionPhase::Finished;
            self.completion = XmlParseCompletion::TerminatedByUser;
            return None;
        }
        if !current_xml_code.is_ok() {
            self.queue.clear();
            self.phase = SessionPhase::Finished;
            self.completion = XmlParseCompletion::Exception;
            return None;
        }

        let item = if let Some(queued) = self.queue.pop_front() {
            queued
        } else {
            self.fill_next_events(active);
            let fallback = QueuedItem {
                registers: self.last_registers.clone(),
                ns_restore: None,
            };
            self.queue.pop_front().unwrap_or(fallback)
        };
        if let Some(mark) = item.ns_restore {
            self.ns_bindings.truncate(mark);
        }
        if item.registers.xml_event == XmlEventKind::Exception {
            self.queue.clear();
            self.phase = SessionPhase::Finished;
            self.completion = XmlParseCompletion::Exception;
        } else if item.registers.xml_event == XmlEventKind::EndOfDocument {
            self.phase = SessionPhase::Finished;
            self.completion = XmlParseCompletion::Completed;
        }
        self.last_registers.clone_from(&item.registers);
        Some(item.registers)
    }

    /// Consumes the finished (or aborted) session into its final [`XmlParseOutcome`].
    #[must_use]
    pub fn into_outcome(self) -> XmlParseOutcome {
        XmlParseOutcome {
            completion: self.completion,
            registers: self.last_registers,
            processed_bytes: ByteCount::new(self.pos),
        }
    }

    /// Returns the unparsed suffix of `self.source` starting at `self.pos`.
    fn rest(&self) -> &str {
        self.source.get(self.pos..).unwrap_or("")
    }

    /// Advances `self.pos` by `byte_len` bytes.
    ///
    /// # Arguments
    /// * `byte_len` - Number of UTF-8 bytes to advance.
    fn advance(&mut self, byte_len: usize) {
        self.pos = self.pos.saturating_add(byte_len);
    }

    /// Consumes `prefix` if `self.rest()` begins with it.
    ///
    /// # Arguments
    /// * `prefix` - Literal prefix to match and consume.
    fn eat(&mut self, prefix: &str) -> bool {
        if self.rest().starts_with(prefix) {
            self.advance(prefix.len());
            true
        } else {
            false
        }
    }

    /// Skips ASCII XML whitespace (`' '`, `TAB`, `CR`, `LF`), returning how many bytes moved.
    fn skip_xml_ws(&mut self) -> usize {
        let len = self
            .rest()
            .bytes()
            .take_while(|b| is_xml_ws_byte(*b))
            .count();
        self.advance(len);
        len
    }

    /// Enqueues a single non-restoring event.
    ///
    /// # Arguments
    /// * `event` - [`XmlEventKind`] to enqueue.
    /// * `code` - [`XmlCode`] for the event.
    /// * `text` - `XML-TEXT` payload.
    /// * `ns` - `XML-NAMESPACE` URI.
    /// * `pfx` - `XML-NAMESPACE-PREFIX` string.
    fn push_event(&mut self, event: XmlEventKind, code: XmlCode, text: &str, ns: &str, pfx: &str) {
        self.queue.push_back(QueuedItem {
            registers: XmlRegisters::from_parts(event, code, text, ns, pfx),
            ns_restore: None,
        });
    }

    /// Enqueues an `EXCEPTION` event with `code` and a snippet of offending text.
    ///
    /// # Arguments
    /// * `code` - Positive exception [`XmlCode`].
    /// * `fragment` - Offending input snippet placed in `XML-TEXT`.
    fn push_exception(&mut self, code: XmlCode, fragment: &str) {
        self.push_event(XmlEventKind::Exception, code, fragment, "", "");
    }

    /// Scans the next construct according to `active` and populates `self.queue`.
    ///
    /// # Arguments
    /// * `active` - Current active document phase.
    fn fill_next_events(&mut self, active: ActivePhase) {
        match active {
            ActivePhase::Prolog => self.step_before_root(),
            ActivePhase::Body => self.step_inside_root(),
            ActivePhase::Epilog => self.step_after_root(),
        }
    }

    /// Scans prolog constructs before the root element opens.
    fn step_before_root(&mut self) {
        let at_doc_start = self.pos == 0
            || (self.source.starts_with('\u{FEFF}') && self.pos == '\u{FEFF}'.len_utf8());
        let ws = self.skip_xml_ws();
        let rem = self.rest();
        if rem.is_empty() {
            self.push_exception(XmlCode::UNEXPECTED_EOF, "");
        } else if rem.starts_with("<?xml")
            && rem
                .as_bytes()
                .get(5)
                .is_none_or(|b| is_xml_ws_byte(*b) || *b == b'?')
        {
            if !at_doc_start || ws > 0 {
                self.push_exception(XmlCode::INVALID_XML_DECLARATION, "<?xml");
            } else {
                self.scan_xml_declaration();
            }
        } else if rem.starts_with("<!--") {
            self.scan_comment();
        } else if rem.starts_with("<!DOCTYPE") {
            self.scan_doctype();
        } else if rem.starts_with("<?") {
            self.scan_processing_instruction();
        } else if rem.starts_with("</") {
            self.scan_end_tag();
        } else if rem.starts_with("<!") || !rem.starts_with('<') {
            let frag: String = rem.chars().take(16).collect();
            self.push_exception(XmlCode::MALFORMED_SYNTAX, &frag);
        } else {
            self.scan_start_tag();
        }
    }

    /// Scans constructs inside an open element.
    fn step_inside_root(&mut self) {
        let rem = self.rest();
        if rem.is_empty() {
            self.push_exception(XmlCode::UNEXPECTED_EOF, "");
        } else if rem.starts_with("<!--") {
            self.scan_comment();
        } else if rem.starts_with("<![CDATA[") {
            self.scan_cdata();
        } else if rem.starts_with("<?") {
            self.scan_processing_instruction();
        } else if rem.starts_with("</") {
            self.scan_end_tag();
        } else if rem.starts_with("<!") {
            let frag: String = rem.chars().take(16).collect();
            self.push_exception(XmlCode::MALFORMED_SYNTAX, &frag);
        } else if rem.starts_with('<') {
            self.scan_start_tag();
        } else {
            self.scan_content_text();
        }
    }

    /// Scans trailing miscellaneous constructs after the root element closes.
    fn step_after_root(&mut self) {
        self.skip_xml_ws();
        let rem = self.rest();
        if rem.is_empty() {
            self.push_event(XmlEventKind::EndOfDocument, XmlCode::ZERO, "", "", "");
        } else if rem.starts_with("<!--") {
            self.scan_comment();
        } else if rem.starts_with("<?") {
            self.scan_processing_instruction();
        } else {
            let frag: String = rem.chars().take(16).collect();
            self.push_exception(XmlCode::MALFORMED_SYNTAX, &frag);
        }
    }

    /// Parses `<?xml version="..." [encoding="..."] [standalone="yes|no"]?>`.
    fn scan_xml_declaration(&mut self) {
        self.advance(5);
        let Some(end_rel) = self.rest().find("?>") else {
            self.push_exception(XmlCode::UNEXPECTED_EOF, "<?xml");
            return;
        };
        let body = self.rest().get(..end_rel).unwrap_or("").to_owned();
        self.advance(end_rel + 2);

        let Ok(attrs) = parse_pseudo_attributes(&body) else {
            self.push_exception(XmlCode::INVALID_XML_DECLARATION, &body);
            return;
        };

        let mut iter = attrs.into_iter();
        let Some((first_k, first_v)) = iter.next() else {
            self.push_exception(XmlCode::INVALID_XML_DECLARATION, &body);
            return;
        };
        if first_k != "version" || (first_v != "1.0" && first_v != "1.1") {
            self.push_exception(XmlCode::INVALID_XML_DECLARATION, &first_v);
            return;
        }
        self.push_event(
            XmlEventKind::VersionInformation,
            XmlCode::ZERO,
            &first_v,
            "",
            "",
        );

        let mut saw_encoding = false;
        let mut saw_standalone = false;
        for (key, val) in iter {
            if key == "encoding" && !saw_encoding && !saw_standalone && !val.is_empty() {
                saw_encoding = true;
                self.push_event(
                    XmlEventKind::EncodingDeclaration,
                    XmlCode::ZERO,
                    &val,
                    "",
                    "",
                );
            } else if key == "standalone" && !saw_standalone && (val == "yes" || val == "no") {
                saw_standalone = true;
                self.push_event(
                    XmlEventKind::StandaloneDeclaration,
                    XmlCode::ZERO,
                    &val,
                    "",
                    "",
                );
            } else {
                self.queue.clear();
                self.push_exception(XmlCode::INVALID_XML_DECLARATION, &key);
                return;
            }
        }
    }

    /// Parses `<!-- ... -->` and enqueues [`XmlEventKind::Comment`].
    fn scan_comment(&mut self) {
        self.advance(4);
        let Some(end_rel) = self.rest().find("-->") else {
            self.push_exception(XmlCode::UNEXPECTED_EOF, "<!--");
            return;
        };
        let body = self.rest().get(..end_rel).unwrap_or("").to_owned();
        if body.contains("--") || body.ends_with('-') || !body.chars().all(is_valid_xml_char) {
            self.push_exception(XmlCode::MALFORMED_SYNTAX, &body);
            return;
        }
        self.advance(end_rel + 3);
        self.push_event(XmlEventKind::Comment, XmlCode::ZERO, &body, "", "");
    }

    /// Parses `<![CDATA[ ... ]]>` and enqueues CDATA start, content, and end events.
    fn scan_cdata(&mut self) {
        self.advance(9);
        let Some(end_rel) = self.rest().find("]]>") else {
            self.push_exception(XmlCode::UNEXPECTED_EOF, "<![CDATA[");
            return;
        };
        let payload = self.rest().get(..end_rel).unwrap_or("").to_owned();
        if !payload.chars().all(is_valid_xml_char) {
            self.push_exception(XmlCode::INVALID_CHARACTER, &payload);
            return;
        }
        self.advance(end_rel + 3);
        self.push_event(XmlEventKind::StartOfCdataSection, XmlCode::ZERO, "", "", "");
        if !payload.is_empty() {
            self.push_event(
                XmlEventKind::ContentCharacters,
                XmlCode::ZERO,
                &payload,
                "",
                "",
            );
        }
        self.push_event(XmlEventKind::EndOfCdataSection, XmlCode::ZERO, "", "", "");
    }

    /// Parses `<!DOCTYPE ...>` (supporting internal `[...]` subset and quoted literals).
    fn scan_doctype(&mut self) {
        let start_pos = self.pos;
        self.advance(9);
        if self.skip_xml_ws() == 0 {
            self.push_exception(XmlCode::MALFORMED_SYNTAX, "<!DOCTYPE");
            return;
        }
        let mut depth = 1_usize;
        let mut quote: Option<char> = None;
        let mut consumed = 0_usize;
        for ch in self.rest().chars() {
            consumed += ch.len_utf8();
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
            } else if ch == CH_DQUOTE || ch == CH_SQUOTE {
                quote = Some(ch);
            } else if ch == '<' {
                depth += 1;
            } else if ch == '>' {
                depth -= 1;
                if depth == 0 {
                    self.advance(consumed);
                    let full = self
                        .source
                        .get(start_pos..self.pos)
                        .unwrap_or("")
                        .to_owned();
                    self.push_event(
                        XmlEventKind::DocumentTypeDeclaration,
                        XmlCode::ZERO,
                        &full,
                        "",
                        "",
                    );
                    return;
                }
            }
        }
        self.advance(consumed);
        self.push_exception(XmlCode::UNEXPECTED_EOF, "<!DOCTYPE");
    }

    /// Parses `<?target [data]?>` and enqueues target + data PI events.
    fn scan_processing_instruction(&mut self) {
        self.advance(2);
        let target = self.take_xml_name();
        if target.is_empty() {
            self.push_exception(XmlCode::MALFORMED_SYNTAX, "<?");
            return;
        }
        if target.eq_ignore_ascii_case("xml") {
            self.push_exception(XmlCode::INVALID_XML_DECLARATION, &target);
            return;
        }
        let ws = self.skip_xml_ws();
        let Some(end_rel) = self.rest().find("?>") else {
            self.push_exception(XmlCode::UNEXPECTED_EOF, &target);
            return;
        };
        if ws == 0 && end_rel > 0 {
            self.push_exception(XmlCode::MALFORMED_SYNTAX, &target);
            return;
        }
        let data = self.rest().get(..end_rel).unwrap_or("").to_owned();
        if !data.chars().all(is_valid_xml_char) {
            self.push_exception(XmlCode::INVALID_CHARACTER, &data);
            return;
        }
        self.advance(end_rel + 2);
        self.push_event(
            XmlEventKind::ProcessingInstructionTarget,
            XmlCode::ZERO,
            &target,
            "",
            "",
        );
        self.push_event(
            XmlEventKind::ProcessingInstructionData,
            XmlCode::ZERO,
            &data,
            "",
            "",
        );
    }

    /// Parses an opening or self-closing element tag (`<name attr="val"...>` or `<name.../>`).
    fn scan_start_tag(&mut self) {
        self.advance(1);
        let qname = self.take_xml_name();
        if qname.is_empty() {
            self.push_exception(XmlCode::MALFORMED_SYNTAX, "<");
            return;
        }

        let ns_mark = self.ns_bindings.len();
        let Some((raw_attrs, self_closing)) = self.scan_raw_attributes(&qname) else {
            return;
        };

        let Some(user_attrs) = self.apply_attributes_and_namespaces(raw_attrs, ns_mark) else {
            return;
        };

        let Ok((elem_name, elem_ns, elem_pfx)) = self.resolve_qname(&qname, ElementOrAttr::Element)
        else {
            self.ns_bindings.truncate(ns_mark);
            self.push_exception(XmlCode::UNBOUND_NAMESPACE_PREFIX, &qname);
            return;
        };

        let mut resolved_attrs = Vec::with_capacity(user_attrs.len());
        for (attr_qname, attr_val) in user_attrs {
            let Ok((a_name, a_ns, a_pfx)) = self.resolve_qname(&attr_qname, ElementOrAttr::Attr)
            else {
                self.ns_bindings.truncate(ns_mark);
                self.push_exception(XmlCode::UNBOUND_NAMESPACE_PREFIX, &attr_qname);
                return;
            };
            resolved_attrs.push((a_name, a_ns, a_pfx, attr_val));
        }

        self.push_event(
            XmlEventKind::StartOfElement,
            XmlCode::ZERO,
            &elem_name,
            &elem_ns,
            &elem_pfx,
        );
        for (a_name, a_ns, a_pfx, a_val) in resolved_attrs {
            self.push_event(
                XmlEventKind::AttributeName,
                XmlCode::ZERO,
                &a_name,
                &a_ns,
                &a_pfx,
            );
            self.push_event(
                XmlEventKind::AttributeCharacters,
                XmlCode::ZERO,
                &a_val,
                "",
                "",
            );
        }

        if self_closing {
            self.queue.push_back(QueuedItem {
                registers: XmlRegisters::from_parts(
                    XmlEventKind::EndOfElement,
                    XmlCode::ZERO,
                    &elem_name,
                    &elem_ns,
                    &elem_pfx,
                ),
                ns_restore: Some(ns_mark),
            });
            if self.open_elements.is_empty() {
                self.phase = SessionPhase::Active(ActivePhase::Epilog);
            }
        } else {
            self.open_elements.push(OpenElement {
                raw_qname: qname,
                emitted_name: elem_name,
                namespace_uri: elem_ns,
                prefix: elem_pfx,
                ns_mark,
            });
            self.phase = SessionPhase::Active(ActivePhase::Body);
        }
    }

    /// Scans raw `(attr_name, decoded_value)` pairs up to `>` or `/>`.
    ///
    /// # Arguments
    /// * `elem_qname` - Enclosing element name used for error snippets.
    fn scan_raw_attributes(&mut self, elem_qname: &str) -> Option<(Vec<(String, String)>, bool)> {
        let mut attrs: Vec<(String, String)> = Vec::new();
        loop {
            let ws = self.skip_xml_ws();
            if self.eat("/>") {
                return Some((attrs, true));
            }
            if self.eat(">") {
                return Some((attrs, false));
            }
            if self.rest().is_empty() {
                self.push_exception(XmlCode::UNEXPECTED_EOF, elem_qname);
                return None;
            }
            if ws == 0 {
                self.push_exception(XmlCode::MALFORMED_SYNTAX, elem_qname);
                return None;
            }
            let attr_name = self.take_xml_name();
            if attr_name.is_empty() {
                self.push_exception(XmlCode::MALFORMED_SYNTAX, elem_qname);
                return None;
            }
            if attrs.iter().any(|(existing, _)| existing == &attr_name) {
                self.push_exception(XmlCode::DUPLICATE_ATTRIBUTE, &attr_name);
                return None;
            }
            self.skip_xml_ws();
            if !self.eat("=") {
                self.push_exception(XmlCode::MALFORMED_SYNTAX, &attr_name);
                return None;
            }
            self.skip_xml_ws();
            let decoded_val = self.scan_quoted_attr_value(&attr_name)?;
            attrs.push((attr_name, decoded_val));
        }
    }

    /// Scans a `"`- or `'`-delimited XML attribute value and decodes entity/character references.
    ///
    /// # Arguments
    /// * `attr_name` - Attribute identifier used in exception fragments.
    fn scan_quoted_attr_value(&mut self, attr_name: &str) -> Option<String> {
        let first_ch = self.rest().chars().next();
        let quote = match first_ch {
            Some(CH_DQUOTE) => CH_DQUOTE,
            Some(CH_SQUOTE) => CH_SQUOTE,
            _ => {
                self.push_exception(XmlCode::MALFORMED_SYNTAX, attr_name);
                return None;
            }
        };
        self.advance(quote.len_utf8());
        let Some(end_rel) = self.rest().find(quote) else {
            self.push_exception(XmlCode::UNEXPECTED_EOF, attr_name);
            return None;
        };
        let raw_val = self.rest().get(..end_rel).unwrap_or("").to_owned();
        if raw_val.contains('<') {
            self.push_exception(XmlCode::MALFORMED_SYNTAX, &raw_val);
            return None;
        }
        self.advance(end_rel + quote.len_utf8());
        match decode_xml_references(&raw_val, AttrWhitespace::Normalize) {
            Ok(decoded) => Some(decoded),
            Err(code) => {
                self.push_exception(code, &raw_val);
                None
            }
        }
    }

    /// Extracts `xmlns` declarations when enabled and returns remaining normal attributes.
    ///
    /// # Arguments
    /// * `raw_attrs` - Raw attribute `(name, decoded_value)` list.
    /// * `ns_mark` - Namespace binding stack checkpoint to roll back on failure.
    fn apply_attributes_and_namespaces(
        &mut self,
        raw_attrs: Vec<(String, String)>,
        ns_mark: usize,
    ) -> Option<Vec<(String, String)>> {
        if self.options.namespace_mode == XmlNamespaceMode::Disabled {
            return Some(raw_attrs);
        }
        let mut user_attrs = Vec::with_capacity(raw_attrs.len());
        for (name, val) in raw_attrs {
            if name == "xmlns" {
                self.ns_bindings.push((String::new(), val));
            } else if let Some(pfx) = name.strip_prefix("xmlns:") {
                if pfx.is_empty() || pfx.contains(':') || val.is_empty() {
                    self.ns_bindings.truncate(ns_mark);
                    self.push_exception(XmlCode::MALFORMED_SYNTAX, &name);
                    return None;
                }
                self.ns_bindings.push((pfx.to_owned(), val));
            } else {
                user_attrs.push((name, val));
            }
        }
        Some(user_attrs)
    }

    /// Resolves an element or attribute `QName` against active namespace bindings.
    ///
    /// # Arguments
    /// * `qname` - Raw XML `QName` (`"prefix:local"` or `"local"`).
    /// * `kind` - Whether resolving an element (uses default `xmlns`) or attribute (does not).
    fn resolve_qname(
        &self,
        qname: &str,
        kind: ElementOrAttr,
    ) -> core::result::Result<(String, String, String), ()> {
        if self.options.namespace_mode == XmlNamespaceMode::Disabled {
            return Ok((qname.to_owned(), String::new(), String::new()));
        }
        if let Some((pfx, local)) = qname.split_once(':') {
            if pfx.is_empty() || local.is_empty() || local.contains(':') {
                return Err(());
            }
            if pfx == "xml" {
                return Ok((
                    local.to_owned(),
                    W3C_XML_NAMESPACE_URI.to_owned(),
                    "xml".to_owned(),
                ));
            }
            let uri = self
                .lookup_namespace(pfx)
                .filter(|u| !u.is_empty())
                .ok_or(())?;
            Ok((local.to_owned(), uri.to_owned(), pfx.to_owned()))
        } else {
            let default_uri = match kind {
                ElementOrAttr::Element => self.lookup_namespace("").unwrap_or(""),
                ElementOrAttr::Attr => "",
            };
            Ok((qname.to_owned(), default_uri.to_owned(), String::new()))
        }
    }

    /// Looks up the innermost namespace URI bound to `prefix` (`""` for default namespace).
    ///
    /// # Arguments
    /// * `prefix` - Namespace prefix to search from innermost to outermost scope.
    fn lookup_namespace(&self, prefix: &str) -> Option<&str> {
        self.ns_bindings
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, uri)| uri.as_str())
    }

    /// Parses a closing `</name>` tag and validates it against the open element stack.
    fn scan_end_tag(&mut self) {
        self.advance(2);
        let qname = self.take_xml_name();
        self.skip_xml_ws();
        if qname.is_empty() || !self.eat(">") {
            let err_code = if self.rest().is_empty() {
                XmlCode::UNEXPECTED_EOF
            } else {
                XmlCode::MALFORMED_SYNTAX
            };
            self.push_exception(err_code, &qname);
            return;
        }
        let Some(open) = self.open_elements.pop() else {
            self.push_exception(XmlCode::MISMATCHED_END_TAG, &qname);
            return;
        };
        if open.raw_qname != qname {
            self.push_exception(XmlCode::MISMATCHED_END_TAG, &qname);
            return;
        }
        self.queue.push_back(QueuedItem {
            registers: XmlRegisters::from_parts(
                XmlEventKind::EndOfElement,
                XmlCode::ZERO,
                &open.emitted_name,
                &open.namespace_uri,
                &open.prefix,
            ),
            ns_restore: Some(open.ns_mark),
        });
        if self.open_elements.is_empty() {
            self.phase = SessionPhase::Active(ActivePhase::Epilog);
        }
    }

    /// Scans element character content up to the next `<` and decodes entity/character references.
    fn scan_content_text(&mut self) {
        let end_rel = self.rest().find('<').unwrap_or_else(|| self.rest().len());
        let raw_chunk = self.rest().get(..end_rel).unwrap_or("").to_owned();
        if raw_chunk.contains("]]>") {
            self.push_exception(XmlCode::MALFORMED_SYNTAX, "]]>");
            return;
        }
        self.advance(end_rel);
        match decode_xml_references(&raw_chunk, AttrWhitespace::Preserve) {
            Ok(decoded) => {
                self.push_event(
                    XmlEventKind::ContentCharacters,
                    XmlCode::ZERO,
                    &decoded,
                    "",
                    "",
                );
            }
            Err(code) => {
                self.push_exception(code, &raw_chunk);
            }
        }
    }

    /// Consumes a valid XML `Name` token from `self.rest()`, returning `""` if none starts here.
    fn take_xml_name(&mut self) -> String {
        let mut chars = self.rest().chars();
        let Some(first) = chars.next() else {
            return String::new();
        };
        if !is_xml_name_start(first) {
            return String::new();
        }
        let mut byte_len = first.len_utf8();
        for ch in chars {
            if is_xml_name_char(ch) {
                byte_len += ch.len_utf8();
            } else {
                break;
            }
        }
        let slice = self.rest().get(..byte_len).unwrap_or("").to_owned();
        self.advance(byte_len);
        slice
    }
}

/// Distinguishes element vs attribute namespace resolution rules.
#[derive(Debug, Clone, Copy)]
enum ElementOrAttr {
    /// Element `QName` (unprefixed elements inherit active default `xmlns`).
    Element,
    /// Attribute `QName` (unprefixed attributes are in no namespace per XML Namespaces 1.0).
    Attr,
}

/// Controls XML 1.0 §3.3.3 whitespace normalization for attribute values vs element text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrWhitespace {
    /// Replaces literal `TAB`, `CR`, `LF` with `' '` while preserving numeric character refs.
    Normalize,
    /// Preserves whitespace while normalizing `CRLF` / `CR` line endings to `LF`.
    Preserve,
}

/// Executes streaming `XML PARSE` with a COBOL `PROCESSING PROCEDURE` callback.
///
/// Invokes `procedure` once for each [`XmlRegisters`] event until `END-OF-DOCUMENT`,
/// `EXCEPTION`, or user-requested termination (`MOVE -1 TO XML-CODE`).
///
/// # Arguments
/// * `source` - Input XML document text.
/// * `options` - `XML PARSE` configuration options.
/// * `procedure` - Callback modeling `PROCESSING PROCEDURE IS p1 [THRU p2]`.
///
/// # Errors
/// Propagates any [`coblang_common::CoblangError`] returned by `procedure`.
pub fn xml_parse<F>(
    source: &str,
    options: &XmlParseOptions,
    mut procedure: F,
) -> Result<XmlParseOutcome>
where
    F: FnMut(&mut XmlRegisters) -> Result<()>,
{
    let mut session = XmlParseSession::new(source, options.clone());
    let mut code = XmlCode::ZERO;
    while let Some(mut regs) = session.next_event(code) {
        procedure(&mut regs)?;
        code = regs.xml_code;
    }
    Ok(session.into_outcome())
}

/// Returns `true` if `byte` is one of the four XML 1.0 whitespace bytes (`0x20`, `0x09`, `0x0D`, `0x0A`).
///
/// # Arguments
/// * `byte` - Single UTF-8 byte to inspect.
const fn is_xml_ws_byte(byte: u8) -> bool {
    matches!(byte, b' ' | 0x09 | 0x0D | 0x0A)
}

/// Parses pseudo-attributes (`version="..." encoding="..." standalone="..."`) inside `<?xml ...?>`.
///
/// # Arguments
/// * `body` - Raw interior text between `<?xml` and `?>`.
fn parse_pseudo_attributes(body: &str) -> core::result::Result<Vec<(String, String)>, ()> {
    let mut rest = body;
    let mut out = Vec::new();
    loop {
        let ws = rest.bytes().take_while(|b| is_xml_ws_byte(*b)).count();
        rest = rest.get(ws..).unwrap_or("");
        if rest.is_empty() {
            return Ok(out);
        }
        if ws == 0 {
            return Err(());
        }
        let name_len = rest.bytes().take_while(u8::is_ascii_lowercase).count();
        if name_len == 0 {
            return Err(());
        }
        let key = rest.get(..name_len).unwrap_or("").to_owned();
        rest = rest
            .get(name_len..)
            .unwrap_or("")
            .trim_start_matches([' ', CH_TAB, CH_CR, CH_LF]);
        let Some(after_eq) = rest.strip_prefix('=') else {
            return Err(());
        };
        rest = after_eq.trim_start_matches([' ', CH_TAB, CH_CR, CH_LF]);
        let (quote, quoted_tail) = if let Some(s) = rest.strip_prefix(CH_DQUOTE) {
            (CH_DQUOTE, s)
        } else if let Some(s) = rest.strip_prefix(CH_SQUOTE) {
            (CH_SQUOTE, s)
        } else {
            return Err(());
        };
        let end_idx = quoted_tail.find(quote).ok_or(())?;
        let val = quoted_tail.get(..end_idx).unwrap_or("").to_owned();
        rest = quoted_tail.get(end_idx + 1..).unwrap_or("");
        out.push((key, val));
    }
}

/// Decodes XML predefined entities (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`) and numeric
/// character references (`&#...;` / `&#x...;`), validating XML 1.0 legal characters.
///
/// # Arguments
/// * `raw` - Raw text slice from content or attribute literal.
/// * `ws_mode` - Attribute whitespace normalization vs element text preservation.
fn decode_xml_references(
    raw: &str,
    ws_mode: AttrWhitespace,
) -> core::result::Result<String, XmlCode> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(ch) = rest.chars().next() {
        if ch == '&' {
            let after_amp = rest.get(1..).unwrap_or("");
            let semi = after_amp
                .find(';')
                .ok_or(XmlCode::INVALID_ENTITY_OR_CHAR_REF)?;
            let ref_body = after_amp.get(..semi).unwrap_or("");
            let decoded_ch = resolve_entity_or_char_ref(ref_body)?;
            out.push(decoded_ch);
            rest = after_amp.get(semi + 1..).unwrap_or("");
        } else if let Some(after_crlf) = rest.strip_prefix(CRLF_STR) {
            out.push(if ws_mode == AttrWhitespace::Normalize {
                ' '
            } else {
                CH_LF
            });
            rest = after_crlf;
        } else {
            if !is_valid_xml_char(ch) {
                return Err(XmlCode::INVALID_CHARACTER);
            }
            if ws_mode == AttrWhitespace::Normalize && matches!(ch, CH_TAB | CH_CR | CH_LF) {
                out.push(' ');
            } else if ch == CH_CR {
                out.push(CH_LF);
            } else {
                out.push(ch);
            }
            rest = rest.get(ch.len_utf8()..).unwrap_or("");
        }
    }
    Ok(out)
}

/// Resolves an entity name or numeric character reference body (without `&` and `;`).
///
/// # Arguments
/// * `ref_body` - Reference token between `&` and `;` (e.g. `"lt"`, `"#65"`, `"#x1F600"`).
fn resolve_entity_or_char_ref(ref_body: &str) -> core::result::Result<char, XmlCode> {
    let resolved = match ref_body {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some(CH_DQUOTE),
        "apos" => Some(CH_SQUOTE),
        _ => {
            if let Some(hex_str) = ref_body
                .strip_prefix("#x")
                .or_else(|| ref_body.strip_prefix("#X"))
            {
                u32::from_str_radix(hex_str, 16)
                    .ok()
                    .and_then(char::from_u32)
            } else if let Some(dec_str) = ref_body.strip_prefix('#') {
                dec_str.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                None
            }
        }
    };
    let ch = resolved.ok_or(XmlCode::INVALID_ENTITY_OR_CHAR_REF)?;
    if !is_valid_xml_char(ch) {
        return Err(XmlCode::INVALID_ENTITY_OR_CHAR_REF);
    }
    Ok(ch)
}

/// Returns `true` if `ch` is a legal XML 1.0 `Char` (`#x9 | #xA | #xD | [#x20-#xD7FF] | ...`).
///
/// # Arguments
/// * `ch` - Unicode scalar value to validate.
#[must_use]
pub const fn is_valid_xml_char(ch: char) -> bool {
    matches!(
        ch,
        CH_TAB | CH_LF | CH_CR | '\u{0020}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}'
    )
}

/// Inclusive Unicode codepoint intervals for non-ASCII XML 1.0 `NameStartChar`.
const XML_NAME_START_NON_ASCII: &[(u32, u32)] = &[
    (0x00C0, 0x00D6),
    (0x00D8, 0x00F6),
    (0x00F8, 0x02FF),
    (0x0370, 0x037D),
    (0x037F, 0x1FFF),
    (0x200C, 0x200D),
    (0x2070, 0x218F),
    (0x2C00, 0x2FEF),
    (0x3001, 0xD7FF),
    (0xF900, 0xFDCF),
    (0xFDF0, 0xFFFD),
    (0x10000, 0xEFFFF),
];

/// Inclusive Unicode codepoint intervals for extra non-ASCII XML 1.0 `NameChar` continuations.
const XML_NAME_EXTRA_NON_ASCII: &[(u32, u32)] =
    &[(0x00B7, 0x00B7), (0x0300, 0x036F), (0x203F, 0x2040)];

/// Returns `true` if `ch` is a valid XML 1.0 `NameStartChar`.
///
/// # Arguments
/// * `ch` - Candidate initial character of an XML `Name`.
#[must_use]
pub fn is_xml_name_start(ch: char) -> bool {
    let cp = u32::from(ch);
    ch.is_ascii_alphabetic()
        || ch == ':'
        || ch == '_'
        || XML_NAME_START_NON_ASCII
            .iter()
            .any(|&(lo, hi)| cp >= lo && cp <= hi)
}

/// Returns `true` if `ch` is a valid XML 1.0 `NameChar`.
///
/// # Arguments
/// * `ch` - Candidate continuation character of an XML `Name`.
#[must_use]
pub fn is_xml_name_char(ch: char) -> bool {
    let cp = u32::from(ch);
    is_xml_name_start(ch)
        || ch.is_ascii_digit()
        || ch == '-'
        || ch == '.'
        || XML_NAME_EXTRA_NON_ASCII
            .iter()
            .any(|&(lo, hi)| cp >= lo && cp <= hi)
}
