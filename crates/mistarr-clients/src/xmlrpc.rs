//! Minimal XML-RPC encoder and decoder for rtorrent; see `docs/DOWNLOAD-CLIENTS.md` "rtorrent".

use std::fmt::Write as _;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use quick_xml::escape::{escape, unescape};
use quick_xml::events::Event;
use quick_xml::Reader;

/// Deepest array or struct nesting the decoder accepts.
const MAX_DEPTH: usize = 32;

/// An XML-RPC value.
///
/// ```
/// use mistarr_clients::xmlrpc::Value;
/// let v = Value::Array(vec![Value::from("a"), Value::from(1)]);
/// assert_eq!(v.as_array().map(<[Value]>::len), Some(2));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `<i4>`, `<int>` or `<i8>`.
    Int(i64),
    /// `<boolean>`.
    Bool(bool),
    /// `<string>`, or a value with no type element.
    String(String),
    /// `<double>`.
    Double(f64),
    /// `<base64>`, decoded.
    Base64(Vec<u8>),
    /// `<array>`.
    Array(Vec<Value>),
    /// `<struct>`, members in document order.
    Struct(Vec<(String, Value)>),
}

impl Value {
    /// The value as an integer; booleans read as 0 or 1.
    ///
    /// ```
    /// use mistarr_clients::xmlrpc::Value;
    /// assert_eq!(Value::Bool(true).as_i64(), Some(1));
    /// assert_eq!(Value::from("1").as_i64(), None);
    /// ```
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    /// The value as a string slice.
    ///
    /// ```
    /// assert_eq!(mistarr_clients::xmlrpc::Value::from("x").as_str(), Some("x"));
    /// ```
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The elements of an array.
    ///
    /// ```
    /// assert!(mistarr_clients::xmlrpc::Value::Int(1).as_array().is_none());
    /// ```
    #[must_use]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The first struct member called `name`.
    ///
    /// ```
    /// use mistarr_clients::xmlrpc::Value;
    /// let s = Value::Struct(vec![("a".into(), Value::Int(1))]);
    /// assert_eq!(s.member("a"), Some(&Value::Int(1)));
    /// assert_eq!(s.member("b"), None);
    /// ```
    #[must_use]
    pub fn member(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Struct(members) => members.iter().find(|(n, _)| n == name).map(|(_, v)| v),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<Vec<Value>> for Value {
    fn from(items: Vec<Value>) -> Self {
        Value::Array(items)
    }
}

/// An XML-RPC fault: the server's error code and message.
///
/// ```
/// use mistarr_clients::xmlrpc::Fault;
/// let f = Fault { code: -501, message: "no".into() };
/// assert_eq!(Fault::from_value(&f.to_value()), Some(f));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// `faultCode`.
    pub code: i64,
    /// `faultString`.
    pub message: String,
}

impl Fault {
    /// Reads a `{faultCode, faultString}` struct; `None` for anything else.
    ///
    /// ```
    /// use mistarr_clients::xmlrpc::{Fault, Value};
    /// assert_eq!(Fault::from_value(&Value::Int(0)), None);
    /// ```
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            code: value.member("faultCode")?.as_i64()?,
            message: value.member("faultString")?.as_str()?.to_owned(),
        })
    }

    /// The fault as the struct carried in a `<fault>` element.
    ///
    /// ```
    /// use mistarr_clients::xmlrpc::Fault;
    /// let v = Fault { code: 1, message: "m".into() }.to_value();
    /// assert_eq!(v.member("faultCode").and_then(|c| c.as_i64()), Some(1));
    /// ```
    #[must_use]
    pub fn to_value(&self) -> Value {
        Value::Struct(vec![
            ("faultCode".into(), Value::Int(self.code)),
            ("faultString".into(), Value::String(self.message.clone())),
        ])
    }
}

/// A decoded `<methodResponse>`.
///
/// ```
/// use mistarr_clients::xmlrpc::{MethodResponse, Value};
/// let r = MethodResponse::Success(Value::Int(0));
/// assert!(matches!(r, MethodResponse::Success(_)));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum MethodResponse {
    /// The single return value.
    Success(Value),
    /// The server reported a fault.
    Fault(Fault),
}

/// XML that is not a well-formed XML-RPC document.
///
/// ```
/// let e = mistarr_clients::xmlrpc::decode_response(b"<x/>").unwrap_err();
/// assert!(e.to_string().starts_with("malformed XML-RPC"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed XML-RPC: {0}")]
pub struct DecodeError(String);

/// Encodes a `<methodCall>`.
///
/// ```
/// use mistarr_clients::xmlrpc::{encode_call, Value};
/// let xml = encode_call("d.start", &[Value::from("AB")]);
/// assert!(xml.ends_with(b"<value><string>AB</string></value></param></params></methodCall>"));
/// ```
#[must_use]
pub fn encode_call(method: &str, params: &[Value]) -> Vec<u8> {
    let mut out = String::from("<?xml version=\"1.0\"?><methodCall><methodName>");
    out.push_str(&escape(method));
    out.push_str("</methodName><params>");
    for p in params {
        out.push_str("<param>");
        write_value(&mut out, p);
        out.push_str("</param>");
    }
    out.push_str("</params></methodCall>");
    out.into_bytes()
}

/// Encodes a successful `<methodResponse>` carrying `value`.
///
/// ```
/// use mistarr_clients::xmlrpc::{decode_response, encode_response, MethodResponse, Value};
/// let xml = encode_response(&Value::Int(7));
/// assert_eq!(decode_response(&xml)?, MethodResponse::Success(Value::Int(7)));
/// # Ok::<(), mistarr_clients::xmlrpc::DecodeError>(())
/// ```
#[must_use]
pub fn encode_response(value: &Value) -> Vec<u8> {
    let mut out = String::from("<?xml version=\"1.0\"?><methodResponse><params><param>");
    write_value(&mut out, value);
    out.push_str("</param></params></methodResponse>");
    out.into_bytes()
}

/// Encodes a fault `<methodResponse>`.
///
/// ```
/// use mistarr_clients::xmlrpc::{decode_response, encode_fault, Fault, MethodResponse};
/// let f = Fault { code: -1, message: "bad".into() };
/// assert_eq!(decode_response(&encode_fault(&f))?, MethodResponse::Fault(f));
/// # Ok::<(), mistarr_clients::xmlrpc::DecodeError>(())
/// ```
#[must_use]
pub fn encode_fault(fault: &Fault) -> Vec<u8> {
    let mut out = String::from("<?xml version=\"1.0\"?><methodResponse><fault>");
    write_value(&mut out, &fault.to_value());
    out.push_str("</fault></methodResponse>");
    out.into_bytes()
}

/// Decodes a `<methodCall>` into its method name and parameters.
///
/// # Errors
/// [`DecodeError`] when `xml` is not a method call.
///
/// ```
/// use mistarr_clients::xmlrpc::{decode_call, encode_call, Value};
/// let (m, p) = decode_call(&encode_call("d.stop", &[Value::from("AB")]))?;
/// assert_eq!((m.as_str(), p), ("d.stop", vec![Value::from("AB")]));
/// # Ok::<(), mistarr_clients::xmlrpc::DecodeError>(())
/// ```
pub fn decode_call(xml: &[u8]) -> Result<(String, Vec<Value>), DecodeError> {
    let mut p = Parser::new(xml);
    p.expect_open("methodCall")?;
    p.expect_open("methodName")?;
    let method = p.text_until("methodName")?;
    let mut params = Vec::new();
    match p.next_tag()? {
        Tok::Close(n) if n == "methodCall" => return p.finish((method, params)),
        Tok::Open(n) if n == "params" => {}
        other => return Err(unexpected(&other)),
    }
    loop {
        match p.next_tag()? {
            Tok::Open(n) if n == "param" => {
                p.expect_open("value")?;
                params.push(p.value(0)?);
                p.expect_close("param")?;
            }
            Tok::Close(n) if n == "params" => break,
            other => return Err(unexpected(&other)),
        }
    }
    p.expect_close("methodCall")?;
    p.finish((method, params))
}

/// Decodes a `<methodResponse>`: one return value or a fault.
///
/// # Errors
/// [`DecodeError`] when `xml` is not a method response.
///
/// ```
/// use mistarr_clients::xmlrpc::{decode_response, MethodResponse, Value};
/// let xml = b"<methodResponse><params><param><value><i8>5</i8></value></param></params></methodResponse>";
/// assert_eq!(decode_response(xml)?, MethodResponse::Success(Value::Int(5)));
/// # Ok::<(), mistarr_clients::xmlrpc::DecodeError>(())
/// ```
pub fn decode_response(xml: &[u8]) -> Result<MethodResponse, DecodeError> {
    let mut p = Parser::new(xml);
    p.expect_open("methodResponse")?;
    let response = match p.next_tag()? {
        Tok::Open(n) if n == "params" => {
            p.expect_open("param")?;
            p.expect_open("value")?;
            let value = p.value(0)?;
            p.expect_close("param")?;
            p.expect_close("params")?;
            MethodResponse::Success(value)
        }
        Tok::Open(n) if n == "fault" => {
            p.expect_open("value")?;
            let value = p.value(0)?;
            p.expect_close("fault")?;
            let fault = Fault::from_value(&value)
                .ok_or_else(|| DecodeError("fault without faultCode and faultString".into()))?;
            MethodResponse::Fault(fault)
        }
        other => return Err(unexpected(&other)),
    };
    p.expect_close("methodResponse")?;
    p.finish(response)
}

fn write_value(out: &mut String, value: &Value) {
    out.push_str("<value>");
    match value {
        Value::Int(i) => {
            let tag = if i32::try_from(*i).is_ok() {
                "i4"
            } else {
                "i8"
            };
            // Writing to a String cannot fail.
            let _ = write!(out, "<{tag}>{i}</{tag}>");
        }
        Value::Bool(b) => {
            out.push_str(if *b {
                "<boolean>1</boolean>"
            } else {
                "<boolean>0</boolean>"
            });
        }
        Value::String(s) => {
            out.push_str("<string>");
            out.push_str(&escape(s.as_str()));
            out.push_str("</string>");
        }
        Value::Double(d) => {
            let _ = write!(out, "<double>{d}</double>");
        }
        Value::Base64(bytes) => {
            out.push_str("<base64>");
            BASE64.encode_string(bytes, out);
            out.push_str("</base64>");
        }
        Value::Array(items) => {
            out.push_str("<array><data>");
            for item in items {
                write_value(out, item);
            }
            out.push_str("</data></array>");
        }
        Value::Struct(members) => {
            out.push_str("<struct>");
            for (name, v) in members {
                out.push_str("<member><name>");
                out.push_str(&escape(name.as_str()));
                out.push_str("</name>");
                write_value(out, v);
                out.push_str("</member>");
            }
            out.push_str("</struct>");
        }
    }
    out.push_str("</value>");
}

/// A simplified XML token; adjacent text, CDATA and references are merged.
#[derive(Debug)]
enum Tok {
    Open(String),
    Close(String),
    Text(String),
    Eof,
}

fn unexpected(tok: &Tok) -> DecodeError {
    DecodeError(format!("unexpected {tok:?}"))
}

struct Parser<'a> {
    reader: Reader<&'a [u8]>,
    pending: Option<Tok>,
}

impl<'a> Parser<'a> {
    fn new(xml: &'a [u8]) -> Self {
        let mut reader = Reader::from_reader(xml);
        let config = reader.config_mut();
        config.expand_empty_elements = true;
        config.check_end_names = true;
        Self {
            reader,
            pending: None,
        }
    }

    fn next(&mut self) -> Result<Tok, DecodeError> {
        if let Some(tok) = self.pending.take() {
            return Ok(tok);
        }
        let bad = |e: &dyn std::fmt::Display| DecodeError(e.to_string());
        let mut text: Option<String> = None;
        loop {
            let event = self.reader.read_event().map_err(|e| bad(&e))?;
            let tok = match event {
                Event::Start(e) => {
                    Tok::Open(String::from_utf8_lossy(e.local_name().as_ref()).into_owned())
                }
                Event::End(e) => {
                    Tok::Close(String::from_utf8_lossy(e.local_name().as_ref()).into_owned())
                }
                Event::Text(t) => {
                    let s = t.xml10_content().map_err(|e| bad(&e))?;
                    text.get_or_insert_with(String::new).push_str(&s);
                    continue;
                }
                Event::CData(c) => {
                    let s = c.decode().map_err(|e| bad(&e))?;
                    text.get_or_insert_with(String::new).push_str(&s);
                    continue;
                }
                Event::GeneralRef(r) => {
                    let name = r.decode().map_err(|e| bad(&e))?;
                    let s = unescape(&format!("&{name};"))
                        .map_err(|e| bad(&e))?
                        .into_owned();
                    text.get_or_insert_with(String::new).push_str(&s);
                    continue;
                }
                Event::Eof => Tok::Eof,
                Event::Empty(_)
                | Event::Comment(_)
                | Event::Decl(_)
                | Event::PI(_)
                | Event::DocType(_) => continue,
            };
            return Ok(match text {
                Some(t) => {
                    self.pending = Some(tok);
                    Tok::Text(t)
                }
                None => tok,
            });
        }
    }

    /// The next open or close tag, skipping whitespace between elements.
    fn next_tag(&mut self) -> Result<Tok, DecodeError> {
        loop {
            match self.next()? {
                Tok::Text(t) if t.trim().is_empty() => {}
                tok => return Ok(tok),
            }
        }
    }

    fn expect_open(&mut self, name: &str) -> Result<(), DecodeError> {
        match self.next_tag()? {
            Tok::Open(n) if n == name => Ok(()),
            other => Err(DecodeError(format!("expected <{name}>, got {other:?}"))),
        }
    }

    fn expect_close(&mut self, name: &str) -> Result<(), DecodeError> {
        match self.next_tag()? {
            Tok::Close(n) if n == name => Ok(()),
            other => Err(DecodeError(format!("expected </{name}>, got {other:?}"))),
        }
    }

    /// Text content up to the close tag `name`, which is consumed.
    fn text_until(&mut self, name: &str) -> Result<String, DecodeError> {
        match self.next()? {
            Tok::Close(n) if n == name => Ok(String::new()),
            Tok::Text(t) => {
                self.expect_close(name)?;
                Ok(t)
            }
            other => Err(unexpected(&other)),
        }
    }

    /// Parses the content of a `<value>` whose open tag was consumed, and its close tag.
    fn value(&mut self, depth: usize) -> Result<Value, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError("nesting too deep".into()));
        }
        let tag = match self.next()? {
            Tok::Close(n) if n == "value" => return Ok(Value::String(String::new())),
            Tok::Text(t) => match self.next()? {
                Tok::Close(n) if n == "value" => return Ok(Value::String(t)),
                Tok::Open(n) if t.trim().is_empty() => n,
                other => return Err(unexpected(&other)),
            },
            Tok::Open(n) => n,
            other => return Err(unexpected(&other)),
        };
        let value = match tag.as_str() {
            "i4" | "int" | "i8" => {
                let t = self.text_until(&tag)?;
                Value::Int(t.trim().parse().map_err(|_| bad_scalar(&tag, &t))?)
            }
            "boolean" => {
                let t = self.text_until(&tag)?;
                match t.trim() {
                    "1" => Value::Bool(true),
                    "0" => Value::Bool(false),
                    _ => return Err(bad_scalar(&tag, &t)),
                }
            }
            "string" => Value::String(self.text_until(&tag)?),
            "double" => {
                let t = self.text_until(&tag)?;
                Value::Double(t.trim().parse().map_err(|_| bad_scalar(&tag, &t))?)
            }
            "base64" => {
                let t = self.text_until(&tag)?;
                let compact: String = t.split_ascii_whitespace().collect();
                Value::Base64(BASE64.decode(compact).map_err(|_| bad_scalar(&tag, &t))?)
            }
            "array" => self.array(depth)?,
            "struct" => self.structure(depth)?,
            other => return Err(DecodeError(format!("unsupported type <{other}>"))),
        };
        self.expect_close("value")?;
        Ok(value)
    }

    fn array(&mut self, depth: usize) -> Result<Value, DecodeError> {
        self.expect_open("data")?;
        let mut items = Vec::new();
        loop {
            match self.next_tag()? {
                Tok::Open(n) if n == "value" => items.push(self.value(depth + 1)?),
                Tok::Close(n) if n == "data" => break,
                other => return Err(unexpected(&other)),
            }
        }
        self.expect_close("array")?;
        Ok(Value::Array(items))
    }

    fn structure(&mut self, depth: usize) -> Result<Value, DecodeError> {
        let mut members = Vec::new();
        loop {
            match self.next_tag()? {
                Tok::Open(n) if n == "member" => {
                    self.expect_open("name")?;
                    let name = self.text_until("name")?;
                    self.expect_open("value")?;
                    let value = self.value(depth + 1)?;
                    self.expect_close("member")?;
                    members.push((name, value));
                }
                Tok::Close(n) if n == "struct" => break,
                other => return Err(unexpected(&other)),
            }
        }
        Ok(Value::Struct(members))
    }

    fn finish<T>(&mut self, result: T) -> Result<T, DecodeError> {
        match self.next_tag()? {
            Tok::Eof => Ok(result),
            other => Err(DecodeError(format!("trailing {other:?}"))),
        }
    }
}

fn bad_scalar(tag: &str, text: &str) -> DecodeError {
    DecodeError(format!("invalid <{tag}> {text:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_type() -> Value {
        Value::Struct(vec![
            ("int".into(), Value::Int(-7)),
            ("big".into(), Value::Int(1 << 40)),
            ("yes".into(), Value::Bool(true)),
            ("no".into(), Value::Bool(false)),
            ("text".into(), Value::from("a < b & \"c\" > 'd'")),
            ("empty".into(), Value::from("")),
            ("double".into(), Value::Double(1.5)),
            ("bytes".into(), Value::Base64(vec![0, 1, 2, 255])),
            (
                "list".into(),
                Value::Array(vec![Value::Array(vec![]), Value::Struct(vec![])]),
            ),
        ])
    }

    #[test]
    fn encodes_a_call_exactly() {
        let xml = encode_call(
            "load.raw",
            &[
                Value::from(""),
                Value::Base64(b"de".to_vec()),
                Value::Int(1),
            ],
        );
        assert_eq!(
            String::from_utf8(xml).expect("utf8"),
            "<?xml version=\"1.0\"?><methodCall><methodName>load.raw</methodName><params>\
             <param><value><string></string></value></param>\
             <param><value><base64>ZGU=</base64></value></param>\
             <param><value><i4>1</i4></value></param></params></methodCall>"
        );
    }

    #[test]
    fn call_round_trips_every_type() {
        let params = vec![every_type(), Value::Int(i64::MIN), Value::from("x")];
        let (method, back) = decode_call(&encode_call("m.x", &params)).expect("decode");
        assert_eq!(method, "m.x");
        assert_eq!(back, params);
        let (_, none) = decode_call(&encode_call("m", &[])).expect("decode");
        assert!(none.is_empty());
    }

    #[test]
    fn response_round_trips_every_type() {
        let v = every_type();
        assert_eq!(
            decode_response(&encode_response(&v)).expect("decode"),
            MethodResponse::Success(v)
        );
    }

    #[test]
    fn parses_a_fault() {
        let xml = br"<?xml version='1.0'?>
<methodResponse>
  <fault>
    <value><struct>
      <member><name>faultCode</name><value><i4>-501</i4></value></member>
      <member><name>faultString</name><value><string>Could not find info-hash.</string></value></member>
    </struct></value>
  </fault>
</methodResponse>
";
        assert_eq!(
            decode_response(xml).expect("decode"),
            MethodResponse::Fault(Fault {
                code: -501,
                message: "Could not find info-hash.".into()
            })
        );
    }

    #[test]
    fn fault_needs_code_and_string() {
        let xml =
            b"<methodResponse><fault><value><struct></struct></value></fault></methodResponse>";
        assert!(decode_response(xml).is_err());
    }

    #[test]
    fn reads_untyped_values_entities_and_whitespace() {
        let xml = b"<methodResponse><params><param><value><array><data>
            <value>plain &amp; &#x41;</value>
            <value></value>
            <value> <int> 3 </int> </value>
            <value><string/></value>
            <value><string><![CDATA[<raw>]]></string></value>
            <value><base64>AAEC
            /w==</base64></value>
            </data></array></value></param></params></methodResponse>";
        let MethodResponse::Success(Value::Array(items)) = decode_response(xml).expect("decode")
        else {
            panic!("not an array");
        };
        assert_eq!(
            items,
            vec![
                Value::from("plain & A"),
                Value::from(""),
                Value::Int(3),
                Value::from(""),
                Value::from("<raw>"),
                Value::Base64(vec![0, 1, 2, 255]),
            ]
        );
    }

    #[test]
    fn rejects_malformed_documents() {
        let cases: [&[u8]; 8] = [
            b"",
            b"<methodResponse></methodResponse>",
            b"<methodResponse><params><param><value><i4>x</i4></value></param></params></methodResponse>",
            b"<methodResponse><params><param><value><boolean>2</boolean></value></param></params></methodResponse>",
            b"<methodResponse><params><param><value><nil/></value></param></params></methodResponse>",
            b"<methodResponse><params><param><value><i4>1</i4></value></param></params></methodResponse><x/>",
            b"<methodResponse><params><param><value><i4>1</i4></value></param></params>",
            b"<methodResponse><params><param><value><i4>1</i8></value></param></params></methodResponse>",
        ];
        for xml in cases {
            assert!(
                decode_response(xml).is_err(),
                "{}",
                String::from_utf8_lossy(xml)
            );
        }
        assert!(decode_call(b"<methodResponse/>").is_err());
    }

    #[test]
    fn deep_nesting_is_rejected() {
        let mut v = Value::Int(0);
        for _ in 0..=MAX_DEPTH + 1 {
            v = Value::Array(vec![v]);
        }
        assert!(decode_response(&encode_response(&v)).is_err());
    }

    #[test]
    fn accessors() {
        assert_eq!(Value::from(String::from("s")).as_str(), Some("s"));
        assert_eq!(Value::from(4_i64).as_i64(), Some(4));
        assert_eq!(Value::from(false).as_i64(), Some(0));
        assert_eq!(Value::from(vec![]).as_array(), Some(&[][..]));
        assert_eq!(Value::Int(1).member("a"), None);
    }
}
