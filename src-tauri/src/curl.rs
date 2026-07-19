//! Curl string → `Request` parser.
//!
//! Not a general shell parser. Handles the shape of curl commands people
//! actually paste (Chrome DevTools "Copy as cURL", terminal history), which
//! means: single/double-quoted args with escapes, backslash line
//! continuations, and the ~15 flags most requests use.
//!
//! Explicitly ignored: `-F/--form` (multipart), `-b/--cookie`, `-u/--user`.
//! Add if the need is real.

use crate::model::*;

// Split a shell-like command line into tokens, respecting single quotes,
// double quotes, and backslash line-continuation. Not a full shell parser
// — handles the shape of curl commands people paste from browser DevTools.
fn tokenize(input: &str) -> Result<Vec<String>, String> {
    let s = input.replace("\\\n", " ").replace("\\\r\n", " ");
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if in_double => {
                if let Some(&nx) = chars.peek() {
                    match nx {
                        '"' | '\\' | '$' | '`' | '\n' => {
                            cur.push(chars.next().unwrap());
                        }
                        _ => cur.push(c),
                    }
                }
            }
            '\\' if !in_single && !in_double => {
                if let Some(nx) = chars.next() {
                    if nx != '\n' {
                        cur.push(nx);
                    }
                }
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if in_single || in_double {
        return Err("unclosed quote".into());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// Parse a curl command string into a `Request`.
///
/// Inference rules:
/// - `-d` present with no `-X` → method = POST
/// - `--json` flag → body kind = json + auto-adds `Content-Type: application/json`
/// - Content-Type header contains `json` → body kind = json
/// - Otherwise body kind = text
pub fn parse_curl(input: &str) -> Result<Request, String> {
    let mut tokens = tokenize(input.trim())?.into_iter();
    let first = tokens.next().ok_or_else(|| "empty".to_string())?;
    if first != "curl" && !first.ends_with("/curl") {
        return Err(format!("expected curl, got `{}`", first));
    }

    let mut method: Option<String> = None;
    let mut url: Option<String> = None;
    let mut headers: Vec<KV> = Vec::new();
    let mut body_data: Option<String> = None;
    let mut body_kind: &str = "text";

    while let Some(tok) = tokens.next() {
        match tok.as_str() {
            "-X" | "--request" => {
                if let Some(m) = tokens.next() {
                    method = Some(m.to_uppercase());
                }
            }
            "-H" | "--header" => {
                if let Some(h) = tokens.next() {
                    if let Some((k, v)) = h.split_once(':') {
                        headers.push(KV {
                            name: k.trim().to_string(),
                            value: v.trim().to_string(),
                            r#type: None,
                            description: None,
                            enabled: Some(true),
                        });
                    }
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii" => {
                if let Some(d) = tokens.next() {
                    body_data = Some(d);
                }
            }
            "--json" => {
                if let Some(d) = tokens.next() {
                    body_data = Some(d);
                    body_kind = "json";
                    if !headers
                        .iter()
                        .any(|h| h.name.eq_ignore_ascii_case("content-type"))
                    {
                        headers.push(KV {
                            name: "Content-Type".into(),
                            value: "application/json".into(),
                            r#type: None,
                            description: None,
                            enabled: Some(true),
                        });
                    }
                }
            }
            "--url" => {
                if let Some(u) = tokens.next() {
                    url = Some(u);
                }
            }
            "-u" | "--user" => {
                let _ = tokens.next();
            }
            "--compressed" | "-i" | "-s" | "-S" | "-L" | "-k" | "--insecure" | "-v"
            | "--verbose" | "-4" | "-6" | "-#" | "--progress-bar" | "-N" | "--no-buffer" | "-g"
            | "--globoff" | "-o" | "-O" => {}
            "-A" | "--user-agent" | "-e" | "--referer" | "-b" | "--cookie"
            | "--connect-timeout" | "--max-time" | "--proxy" | "-x" | "-o " | "-O "
            | "--output" | "--upload-file" | "-T" | "--cacert" | "--cert" | "--key" => {
                let _ = tokens.next();
            }
            arg if arg.starts_with('-') => {
                // unknown flag with value: skip next if present-ish; safest to keep going
            }
            _ => {
                if url.is_none() {
                    url = Some(tok);
                }
            }
        }
    }

    let url = url.ok_or_else(|| "no url in curl".to_string())?;

    // infer body kind from Content-Type if not already json
    if body_kind == "text" {
        for h in &headers {
            if h.name.eq_ignore_ascii_case("content-type") && h.value.contains("json") {
                body_kind = "json";
                break;
            }
        }
    }

    // infer method: -d implies POST unless -X specified
    let method = method.unwrap_or_else(|| {
        if body_data.is_some() {
            "POST".into()
        } else {
            "GET".into()
        }
    });

    let body = if let Some(data) = body_data.clone() {
        Body {
            r#type: Some(body_kind.to_string()),
            json: if body_kind == "json" {
                Some(data.clone())
            } else {
                None
            },
            text: if body_kind == "text" {
                Some(data)
            } else {
                None
            },
            data: None,
        }
    } else {
        Body::default()
    };

    Ok(Request {
        info: Info {
            name: String::new(),
            r#type: "http".into(),
            seq: 1,
        },
        http: HttpSpec {
            method,
            url,
            headers,
            params: Vec::new(),
            body,
            auth: None,
            auth_config: None,
        },
        path: String::new(),
        rel_path: String::new(),
        id: String::new(),
    })
}
