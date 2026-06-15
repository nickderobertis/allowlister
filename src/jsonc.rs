//! Comment-preserving edits to JSONC config text.
//!
//! `install` and `init` update an existing config file in place. Re-serializing
//! the parsed document would discard the file's comments, so edits are spliced
//! into the original text instead: the comment-stripped shadow (same length and
//! offsets as the original — see [`crate::config::strip_jsonc_comments`]) is
//! scanned to locate insertion points, and new content is inserted into the
//! untouched original. Every comment keeps its exact position.
//!
//! Every edit is verified before it is returned: the result must re-parse to
//! exactly the document the equivalent value-level mutation would produce. A
//! mismatch is an error, never a silently corrupted file.

use serde_json::Value;

use crate::config::strip_jsonc_comments;

/// Append `new_rules` to the top-level `rules` array of `text`, preserving all
/// comments and existing formatting. A document without a `rules` key gains
/// one. `text` must parse (after comment stripping) to a JSON object whose
/// `rules`, when present, is an array.
pub(crate) fn append_rules(text: &str, new_rules: &[Value]) -> Result<String, String> {
    if new_rules.is_empty() {
        return Ok(text.to_string());
    }
    let stripped = strip_jsonc_comments(text);
    let top = top_level(&stripped)?;
    let Some(member) = top.members.iter().find(|m| m.key == "rules") else {
        return set_top_level(text, "rules", &Value::Array(new_rules.to_vec()));
    };
    let edited = splice_rules(text, &stripped, member, new_rules)?;

    let mut expected = parse(&stripped)?;
    expected
        .as_object_mut()
        .ok_or("expected a top-level JSON object")?
        .get_mut("rules")
        .and_then(Value::as_array_mut)
        .ok_or("'rules' is not an array")?
        .extend(new_rules.iter().cloned());
    verify(&edited, &expected)?;
    Ok(edited)
}

/// Remove the first top-level rule whose `name` equals `name`, preserving the
/// comments and formatting of every rule that remains. Returns the edited text
/// and whether a rule was removed; a document with no matching rule comes back
/// unchanged with `false`. Like the other edits here, the result is verified to
/// re-parse to exactly the document the value-level removal would produce.
///
/// A comment that sits between the removed rule and its neighbor (a trailing
/// note on the same line, or a standalone block immediately before the next
/// rule) may go with it — comment ownership across a deleted element is
/// inherently ambiguous — but no surviving rule is ever disturbed.
pub(crate) fn remove_rule(text: &str, name: &str) -> Result<(String, bool), String> {
    let stripped = strip_jsonc_comments(text);
    let top = top_level(&stripped)?;
    let Some(member) = top.members.iter().find(|m| m.key == "rules") else {
        return Ok((text.to_string(), false));
    };
    let (elements, close) = array_spans(&stripped, member.value_start)?;

    // Locate the first element whose parsed `name` matches.
    let mut found = None;
    for (idx, &(start, end)) in elements.iter().enumerate() {
        let value: Value = serde_json::from_str(&stripped[start..end])
            .map_err(|err| format!("invalid rule JSON: {err}"))?;
        if value.get("name").and_then(Value::as_str) == Some(name) {
            found = Some(idx);
            break;
        }
    }
    let Some(idx) = found else {
        return Ok((text.to_string(), false));
    };

    let (start, end) = elements[idx];
    let own_line = line_indent(&stripped, start).is_some();
    // Byte ranges to delete. Most cases are a single span; removing a last
    // element that sits on its own line splits into two (its line, and the
    // separating comma alone) so the predecessor's trailing comment survives.
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    if let Some(&(next_start, _)) = elements.get(idx + 1) {
        // Not the last element: drop it and the comma that follows, up to the
        // next element — line-aligned so the next element keeps its indentation.
        let cut_start = line_aligned_start(text, &stripped, start);
        let cut_end = if line_indent(&stripped, next_start).is_some() {
            line_start(text, next_start)
        } else {
            next_start
        };
        cuts.push((cut_start, cut_end));
    } else if idx > 0 {
        // Last element with a predecessor: the separating comma must go, but a
        // trailing comment on the predecessor's line should not. When this
        // element owns its line(s), delete just the comma and this element's
        // lines, leaving the comment between them in place.
        let (_, prev_end) = elements[idx - 1];
        if own_line {
            let comma = comma_after(&stripped, prev_end);
            let cut_end = if line_indent(&stripped, close).is_some() {
                line_start(text, close)
            } else {
                end
            };
            cuts.push((comma, comma + 1));
            cuts.push((line_start(text, start), cut_end));
        } else {
            cuts.push((prev_end, end));
        }
    } else {
        // The only element: drop it, line-aligned when it sits on its own lines.
        let cut_start = line_aligned_start(text, &stripped, start);
        let cut_end = if line_indent(&stripped, close).is_some() {
            line_start(text, close)
        } else {
            end
        };
        cuts.push((cut_start, cut_end));
    }

    let mut edited = text.to_string();
    // Apply the cuts back-to-front so earlier offsets stay valid.
    cuts.sort_by_key(|cut| std::cmp::Reverse(cut.0));
    for (cut_start, cut_end) in cuts {
        edited.replace_range(cut_start..cut_end, "");
    }

    let mut expected = parse(&stripped)?;
    expected
        .as_object_mut()
        .ok_or("expected a top-level JSON object")?
        .get_mut("rules")
        .and_then(Value::as_array_mut)
        .ok_or("'rules' is not an array")?
        .remove(idx);
    verify(&edited, &expected)?;
    Ok((edited, true))
}

/// The start of the deletion: the start of `offset`'s line when the element
/// begins its own line (so its indentation goes too), else `offset` itself.
fn line_aligned_start(text: &str, stripped: &str, offset: usize) -> usize {
    if line_indent(stripped, offset).is_some() {
        line_start(text, offset)
    } else {
        offset
    }
}

/// The offset of the `,` separating an element from the next, found by skipping
/// whitespace (comments are blanked in `stripped`) after the prior value's end.
/// Falls back to `from` if no comma is present (the scanner already validated
/// the array, so this is only reached when a comma exists).
fn comma_after(stripped: &str, from: usize) -> usize {
    let i = skip_ws(stripped.as_bytes(), from);
    if stripped.as_bytes().get(i) == Some(&b',') {
        i
    } else {
        from
    }
}

/// Set top-level `key` to `value`, replacing an existing member in place or
/// appending a new one, preserving all comments and existing formatting. `key`
/// must not require JSON string escaping.
pub(crate) fn set_top_level(text: &str, key: &str, value: &Value) -> Result<String, String> {
    let stripped = strip_jsonc_comments(text);
    let top = top_level(&stripped)?;
    let edited = match top.members.iter().find(|m| m.key == key) {
        Some(member) => {
            let indent = line_indent(&stripped, member.key_start).unwrap_or_default();
            let rendered = indent_block(&pretty(value), &indent, false);
            let mut out = String::with_capacity(text.len() + rendered.len());
            out.push_str(&text[..member.value_start]);
            out.push_str(&rendered);
            out.push_str(&text[member.value_end..]);
            out
        }
        None => {
            let indent = top
                .members
                .first()
                .and_then(|m| line_indent(&stripped, m.key_start))
                .unwrap_or_else(|| "  ".to_string());
            let member = format!(
                "{indent}\"{key}\": {}",
                indent_block(&pretty(value), &indent, false)
            );
            let comma_after = top.members.last().map(|last| last.value_end);
            // When `}` sits alone on its line, insert the member as fresh lines
            // above it; otherwise (e.g. `{}`) break the line open around it.
            if line_indent(&stripped, top.close).is_some() {
                splice(
                    text,
                    comma_after,
                    line_start(text, top.close),
                    &format!("{member}\n"),
                )
            } else {
                splice(text, comma_after, top.close, &format!("\n{member}\n"))
            }
        }
    };

    let mut expected = parse(&stripped)?;
    expected
        .as_object_mut()
        .ok_or("expected a top-level JSON object")?
        .insert(key.to_string(), value.clone());
    verify(&edited, &expected)?;
    Ok(edited)
}

/// Insert top-level `key` as the *first* member of `text`, preserving every
/// comment and the existing formatting. A no-op when `key` is already present:
/// its current value is left exactly where it is, never moved or overwritten.
/// `key` must not require JSON string escaping. Used to stamp a leading
/// `"$schema"` onto a config without disturbing the rules below it.
pub(crate) fn set_top_level_first(text: &str, key: &str, value: &Value) -> Result<String, String> {
    let stripped = strip_jsonc_comments(text);
    let top = top_level(&stripped)?;
    if top.members.iter().any(|m| m.key == key) {
        return Ok(text.to_string());
    }
    let Some(first) = top.members.first() else {
        // No members yet: the first position is the only position, so the
        // appending insert already produces the right result.
        return set_top_level(text, key, value);
    };
    let rendered = pretty(value);
    let edited = match line_indent(&stripped, first.key_start) {
        Some(indent) => {
            // The first member starts its own line: add a fresh member line above
            // it at the same indent, carrying the comma that now precedes it.
            let at = line_start(text, first.key_start);
            let member = format!(
                "{indent}\"{key}\": {},\n",
                indent_block(&rendered, &indent, false)
            );
            let mut out = String::with_capacity(text.len() + member.len());
            out.push_str(&text[..at]);
            out.push_str(&member);
            out.push_str(&text[at..]);
            out
        }
        None => {
            // The first member shares the opening brace's line (e.g. `{ "rules": [] }`):
            // break the object open and place the new member on its own line.
            let at = skip_ws(stripped.as_bytes(), 0) + 1;
            let member = format!("\n  \"{key}\": {},", indent_block(&rendered, "  ", false));
            let mut out = String::with_capacity(text.len() + member.len());
            out.push_str(&text[..at]);
            out.push_str(&member);
            out.push_str(&text[at..]);
            out
        }
    };

    let mut expected = parse(&stripped)?;
    expected
        .as_object_mut()
        .ok_or("expected a top-level JSON object")?
        .insert(key.to_string(), value.clone());
    verify(&edited, &expected)?;
    Ok(edited)
}

/// A top-level object member located in the stripped text. Offsets are equally
/// valid in the original text, because stripping preserves every byte offset.
struct Member {
    key: String,
    /// Offset of the opening quote of the key.
    key_start: usize,
    /// Offset of the first byte of the value.
    value_start: usize,
    /// Offset one past the last byte of the value.
    value_end: usize,
}

struct TopLevel {
    members: Vec<Member>,
    /// Offset of the object's closing `}`.
    close: usize,
}

/// Splice `new_rules` into the array at `member`, returning the edited text.
fn splice_rules(
    text: &str,
    stripped: &str,
    member: &Member,
    new_rules: &[Value],
) -> Result<String, String> {
    let (elements, close) = array_spans(stripped, member.value_start)?;
    let key_indent = line_indent(stripped, member.key_start).unwrap_or_default();
    // One indentation unit, taken from the first element when it starts its own
    // line, else two spaces (matching `serde_json::to_string_pretty`).
    let unit = elements
        .first()
        .and_then(|&(start, _)| line_indent(stripped, start))
        .and_then(|indent| indent.strip_prefix(&key_indent).map(str::to_string))
        .filter(|unit| !unit.is_empty())
        .unwrap_or_else(|| "  ".to_string());
    let element_indent = format!("{key_indent}{unit}");
    let block = new_rules
        .iter()
        .map(|rule| indent_block(&pretty(rule), &element_indent, true))
        .collect::<Vec<_>>()
        .join(",\n");

    // The separating comma goes directly after the last element, so a trailing
    // comment on that line stays attached to its rule.
    let comma_after = elements.last().map(|&(_, end)| end);
    // Same line-splitting logic as member insertion: new elements go on their
    // own lines above a lone `]`, otherwise the line is broken open before it.
    let edited = if line_indent(stripped, close).is_some() {
        splice(
            text,
            comma_after,
            line_start(text, close),
            &format!("{block}\n"),
        )
    } else {
        splice(
            text,
            comma_after,
            close,
            &format!("\n{block}\n{key_indent}"),
        )
    };
    Ok(edited)
}

/// Scan the top-level object of `stripped`, returning its member spans and
/// closing brace.
fn top_level(stripped: &str) -> Result<TopLevel, String> {
    let bytes = stripped.as_bytes();
    let open = skip_ws(bytes, 0);
    if bytes.get(open) != Some(&b'{') {
        return Err("expected a top-level JSON object".to_string());
    }
    let mut members = Vec::new();
    let mut i = open + 1;
    loop {
        i = skip_ws(bytes, i);
        match bytes.get(i) {
            Some(b'}') => return Ok(TopLevel { members, close: i }),
            Some(b'"') => {}
            _ => return Err("malformed object member".to_string()),
        }
        let key_start = i;
        let key_end = string_end(bytes, i)?;
        let key = stripped[key_start + 1..key_end - 1].to_string();
        i = skip_ws(bytes, key_end);
        if bytes.get(i) != Some(&b':') {
            return Err("expected ':' after member key".to_string());
        }
        let value_start = skip_ws(bytes, i + 1);
        let end = value_end(bytes, value_start)?;
        members.push(Member {
            key,
            key_start,
            value_start,
            value_end: end,
        });
        i = skip_ws(bytes, end);
        match bytes.get(i) {
            Some(b',') => i += 1,
            Some(b'}') => return Ok(TopLevel { members, close: i }),
            _ => return Err("expected ',' or '}' after member".to_string()),
        }
    }
}

/// Element spans and the closing `]` of the array starting at `open`.
fn array_spans(stripped: &str, open: usize) -> Result<(Vec<(usize, usize)>, usize), String> {
    let bytes = stripped.as_bytes();
    if bytes.get(open) != Some(&b'[') {
        return Err("'rules' is not an array".to_string());
    }
    let mut elements = Vec::new();
    let mut i = open + 1;
    loop {
        i = skip_ws(bytes, i);
        if bytes.get(i) == Some(&b']') {
            return Ok((elements, i));
        }
        let start = i;
        let end = value_end(bytes, i)?;
        elements.push((start, end));
        i = skip_ws(bytes, end);
        match bytes.get(i) {
            Some(b',') => i += 1,
            Some(b']') => return Ok((elements, i)),
            _ => return Err("expected ',' or ']' after array element".to_string()),
        }
    }
}

/// End offset (exclusive) of the JSON value starting at `start`.
fn value_end(bytes: &[u8], start: usize) -> Result<usize, String> {
    match bytes.get(start) {
        Some(b'"') => string_end(bytes, start),
        Some(b'{') | Some(b'[') => {
            let mut depth = 0usize;
            let mut i = start;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' => {
                        i = string_end(bytes, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            Err("unterminated object or array".to_string())
        }
        // A number or literal: runs to the next structural byte or whitespace.
        Some(_) => {
            let mut i = start;
            while i < bytes.len()
                && !matches!(bytes[i], b',' | b'}' | b']')
                && !bytes[i].is_ascii_whitespace()
            {
                i += 1;
            }
            Ok(i)
        }
        None => Err("unexpected end of input".to_string()),
    }
}

/// End offset (exclusive) of the string whose opening quote is at `start`.
fn string_end(bytes: &[u8], start: usize) -> Result<usize, String> {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    Err("unterminated string".to_string())
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Start offset of the line containing `offset`.
fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map_or(0, |newline| newline + 1)
}

/// The whitespace prefix of the line containing `offset`, or `None` when
/// non-whitespace precedes `offset` on its line. Measured on the stripped text,
/// so a comment before the token counts as whitespace (it was blanked).
fn line_indent(stripped: &str, offset: usize) -> Option<String> {
    let prefix = &stripped[line_start(stripped, offset)..offset];
    prefix
        .chars()
        .all(|c| c == ' ' || c == '\t')
        .then(|| prefix.to_string())
}

/// Assemble the edited text: a `,` directly after `comma_after` (when set), and
/// `insertion` at `insert_at`. Requires `comma_after <= insert_at`; the two may
/// coincide (last element directly against the closer), in which case the comma
/// still lands first.
fn splice(text: &str, comma_after: Option<usize>, insert_at: usize, insertion: &str) -> String {
    let mut out = String::with_capacity(text.len() + insertion.len() + 1);
    match comma_after {
        Some(end) => {
            out.push_str(&text[..end]);
            out.push(',');
            out.push_str(&text[end..insert_at]);
        }
        None => out.push_str(&text[..insert_at]),
    }
    out.push_str(insertion);
    out.push_str(&text[insert_at..]);
    out
}

/// Pretty-print `value`, shifting every line by `indent` (the first line only
/// when `indent_first`, for content placed at the start of its own line).
fn indent_block(rendered: &str, indent: &str, indent_first: bool) -> String {
    rendered
        .lines()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 && !indent_first {
                line.to_string()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn parse(stripped: &str) -> Result<Value, String> {
    serde_json::from_str(stripped).map_err(|err| format!("invalid JSON: {err}"))
}

/// The edited text must re-parse to exactly the expected document; anything
/// else means the splice went wrong, and the caller must not write it.
fn verify(edited: &str, expected: &Value) -> Result<(), String> {
    let reparsed = parse(&strip_jsonc_comments(edited)).map_err(|err| {
        format!("internal error: comment-preserving edit produced invalid JSON ({err})")
    })?;
    if &reparsed == expected {
        Ok(())
    } else {
        Err("internal error: comment-preserving edit changed the document unexpectedly".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(name: &str) -> Value {
        json!({ "name": name, "match": format!("{name}*"), "action": "allow" })
    }

    #[test]
    fn appends_to_a_commented_config_preserving_every_comment() {
        let src = "{\n  // header note\n  \"rules\": [\n    { \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" } // trailing\n  ] /* after */\n}\n";
        let out = append_rules(src, &[rule("pwd")]).unwrap();
        // Comments survive verbatim, in order.
        assert!(out.contains("// header note"));
        assert!(out.contains("// trailing"));
        assert!(out.contains("/* after */"));
        // The comma lands after the rule, before its trailing comment.
        assert!(out.contains("\"allow\" }, // trailing"));
        // The original prefix (everything up to the last rule) is untouched.
        assert!(out.starts_with("{\n  // header note\n  \"rules\": [\n    { \"name\": \"ls\""));
        let doc: Value = serde_json::from_str(&strip_jsonc_comments(&out)).unwrap();
        assert_eq!(doc["rules"].as_array().unwrap().len(), 2);
        assert_eq!(doc["rules"][1]["name"], "pwd");
    }

    #[test]
    fn appends_to_an_empty_inline_array() {
        let src = "{\n  \"rules\": []\n}\n";
        let out = append_rules(src, &[rule("ls")]).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["rules"][0]["name"], "ls");
    }

    #[test]
    fn appends_to_an_empty_multiline_array() {
        let src = "{\n  \"rules\": [\n  ]\n}\n";
        let out = append_rules(src, &[rule("ls")]).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["rules"][0]["name"], "ls");
    }

    #[test]
    fn appends_to_a_single_line_array() {
        let src = r#"{ "rules": [{ "name": "ls", "match": "ls*", "action": "allow" }] }"#;
        let out = append_rules(src, &[rule("pwd")]).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["rules"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn creates_the_rules_key_when_absent() {
        let src = "{\n  // just a note\n  \"history\": { \"enabled\": true }\n}\n";
        let out = append_rules(src, &[rule("ls")]).unwrap();
        assert!(out.contains("// just a note"));
        let doc: Value = serde_json::from_str(&strip_jsonc_comments(&out)).unwrap();
        assert_eq!(doc["rules"][0]["name"], "ls");
        assert_eq!(doc["history"]["enabled"], true);
    }

    #[test]
    fn appending_nothing_is_identity() {
        let src = "{ \"rules\": [] } // note";
        assert_eq!(append_rules(src, &[]).unwrap(), src);
    }

    #[test]
    fn matches_the_file_indentation_unit() {
        let src = "{\n    \"rules\": [\n            { \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" }\n    ]\n}\n";
        let out = append_rules(src, &[rule("pwd")]).unwrap();
        // First element is indented 12 spaces (4 + unit 8); the new one matches.
        assert!(out.contains("\n            {\n"), "{out}");
        assert!(out.contains("\"name\": \"pwd\""), "{out}");
    }

    #[test]
    fn set_top_level_inserts_after_the_last_member() {
        let src = "{\n  // keep me\n  \"rules\": [] // inline\n}\n";
        let out = set_top_level(src, "history", &json!({ "enabled": true })).unwrap();
        assert!(out.contains("// keep me"));
        assert!(out.contains("// inline"));
        // The comma attaches to the value, before the trailing comment.
        assert!(out.contains("\"rules\": [], // inline"));
        let doc: Value = serde_json::from_str(&strip_jsonc_comments(&out)).unwrap();
        assert_eq!(doc["history"]["enabled"], true);
    }

    #[test]
    fn set_top_level_replaces_an_existing_member_in_place() {
        let src = "{\n  \"history\": { \"enabled\": true }, // toggle\n  \"rules\": []\n}\n";
        let out = set_top_level(src, "history", &json!({ "enabled": false })).unwrap();
        assert!(out.contains("// toggle"));
        let doc: Value = serde_json::from_str(&strip_jsonc_comments(&out)).unwrap();
        assert_eq!(doc["history"]["enabled"], false);
        // `rules` and the member order are untouched.
        assert!(out.ends_with("\"rules\": []\n}\n"));
    }

    #[test]
    fn set_top_level_handles_an_empty_object() {
        let out = set_top_level("{}", "history", &json!({ "enabled": true })).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["history"]["enabled"], true);
    }

    #[test]
    fn comment_markers_inside_strings_do_not_confuse_the_scanner() {
        let src = "{\n  \"rules\": [\n    { \"name\": \"url // not a comment\", \"match\": \"https://x/**\", \"action\": \"allow\" }\n  ]\n}\n";
        let out = append_rules(src, &[rule("ls")]).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["rules"].as_array().unwrap().len(), 2);
        assert_eq!(doc["rules"][0]["name"], "url // not a comment");
    }

    #[test]
    fn non_object_input_is_an_error() {
        assert!(append_rules("[]", &[rule("ls")]).is_err());
        assert!(set_top_level("[]", "history", &json!(true)).is_err());
        assert!(append_rules("", &[rule("ls")]).is_err());
    }

    #[test]
    fn non_array_rules_is_an_error() {
        assert!(append_rules(r#"{ "rules": 5 }"#, &[rule("ls")]).is_err());
    }

    #[test]
    fn nested_rules_keys_are_not_mistaken_for_the_top_level_one() {
        // The first top-level member contains an inner "rules" key; the real
        // rules array comes later and must be the one extended.
        let src = "{\n  \"meta\": { \"rules\": \"not these\" },\n  \"rules\": [\n    { \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" }\n  ]\n}\n";
        let out = append_rules(src, &[rule("pwd")]).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["meta"]["rules"], "not these");
        assert_eq!(doc["rules"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn removes_a_middle_rule_preserving_siblings_and_comments() {
        let src = "{\n  // header\n  \"rules\": [\n    { \"name\": \"a\", \"match\": \"a*\", \"action\": \"allow\" }, // keep a\n    { \"name\": \"b\", \"match\": \"b*\", \"action\": \"allow\" },\n    { \"name\": \"c\", \"match\": \"c*\", \"action\": \"allow\" } // keep c\n  ]\n}\n";
        let (out, removed) = remove_rule(src, "b").unwrap();
        assert!(removed);
        assert!(out.contains("// header"));
        assert!(out.contains("// keep a"));
        assert!(out.contains("// keep c"));
        assert!(!out.contains("\"name\": \"b\""));
        let doc: Value = serde_json::from_str(&strip_jsonc_comments(&out)).unwrap();
        let names: Vec<&str> = doc["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["a", "c"]);
    }

    #[test]
    fn removes_the_first_rule() {
        let src = "{\n  \"rules\": [\n    { \"name\": \"a\", \"match\": \"a*\", \"action\": \"allow\" },\n    { \"name\": \"b\", \"match\": \"b*\", \"action\": \"allow\" }\n  ]\n}\n";
        let (out, removed) = remove_rule(src, "a").unwrap();
        assert!(removed);
        let doc: Value = serde_json::from_str(&strip_jsonc_comments(&out)).unwrap();
        assert_eq!(doc["rules"].as_array().unwrap().len(), 1);
        assert_eq!(doc["rules"][0]["name"], "b");
    }

    #[test]
    fn removes_the_last_rule_keeping_the_predecessors_trailing_comment() {
        let src = "{\n  \"rules\": [\n    { \"name\": \"a\", \"match\": \"a*\", \"action\": \"allow\" }, // note a\n    { \"name\": \"b\", \"match\": \"b*\", \"action\": \"allow\" }\n  ]\n}\n";
        let (out, removed) = remove_rule(src, "b").unwrap();
        assert!(removed);
        assert!(out.contains("// note a"));
        let doc: Value = serde_json::from_str(&strip_jsonc_comments(&out)).unwrap();
        assert_eq!(doc["rules"].as_array().unwrap().len(), 1);
        assert_eq!(doc["rules"][0]["name"], "a");
        // The dangling comma after `a` is gone.
        assert!(!strip_jsonc_comments(&out).contains(",\n"), "{out}");
    }

    #[test]
    fn removes_the_only_rule_leaving_an_empty_array() {
        let src = "{\n  \"rules\": [\n    { \"name\": \"a\", \"match\": \"a*\", \"action\": \"allow\" }\n  ]\n}\n";
        let (out, removed) = remove_rule(src, "a").unwrap();
        assert!(removed);
        let doc: Value = serde_json::from_str(&strip_jsonc_comments(&out)).unwrap();
        assert!(doc["rules"].as_array().unwrap().is_empty());
    }

    #[test]
    fn removes_from_a_single_line_array() {
        let src = r#"{ "rules": [{ "name": "a", "match": "a*", "action": "allow" }, { "name": "b", "match": "b*", "action": "allow" }] }"#;
        let (out, removed) = remove_rule(src, "a").unwrap();
        assert!(removed);
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["rules"].as_array().unwrap().len(), 1);
        assert_eq!(doc["rules"][0]["name"], "b");
    }

    #[test]
    fn removing_an_absent_rule_is_a_noop() {
        let src = "{\n  \"rules\": [\n    { \"name\": \"a\", \"match\": \"a*\", \"action\": \"allow\" }\n  ]\n}\n";
        let (out, removed) = remove_rule(src, "nope").unwrap();
        assert!(!removed);
        assert_eq!(out, src);
    }

    #[test]
    fn removing_when_there_is_no_rules_key_is_a_noop() {
        let src = "{\n  \"history\": { \"enabled\": true }\n}\n";
        let (out, removed) = remove_rule(src, "a").unwrap();
        assert!(!removed);
        assert_eq!(out, src);
    }

    #[test]
    fn block_comments_between_elements_survive() {
        let src = "{\n  \"rules\": [\n    /* first */\n    { \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" }\n    /* last */\n  ]\n}\n";
        let out = append_rules(src, &[rule("pwd")]).unwrap();
        assert!(out.contains("/* first */"));
        assert!(out.contains("/* last */"));
        let doc: Value = serde_json::from_str(&strip_jsonc_comments(&out)).unwrap();
        assert_eq!(doc["rules"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn set_top_level_first_prepends_before_the_existing_members() {
        let src = "{\n  // header\n  \"rules\": [\n    { \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" }\n  ]\n}\n";
        let out = set_top_level_first(src, "$schema", &json!("https://x/s.json")).unwrap();
        // The new member lands first, above the existing one, at its indent.
        assert!(
            out.starts_with("{\n  // header\n  \"$schema\": \"https://x/s.json\",\n  \"rules\": ["),
            "{out}"
        );
        let doc: Value = serde_json::from_str(&strip_jsonc_comments(&out)).unwrap();
        assert_eq!(doc["$schema"], "https://x/s.json");
        assert_eq!(doc["rules"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn set_top_level_first_is_a_noop_when_present() {
        let src = "{\n  \"$schema\": \"https://old/s.json\",\n  \"rules\": []\n}\n";
        let out = set_top_level_first(src, "$schema", &json!("https://new/s.json")).unwrap();
        // Present already: left exactly as-is, value never overwritten.
        assert_eq!(out, src);
    }

    #[test]
    fn set_top_level_first_handles_empty_and_single_line_objects() {
        let empty = set_top_level_first("{}", "$schema", &json!("https://x/s.json")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&empty).unwrap()["$schema"],
            "https://x/s.json"
        );
        let inline =
            set_top_level_first(r#"{ "rules": [] }"#, "$schema", &json!("https://x/s.json"))
                .unwrap();
        let doc: Value = serde_json::from_str(&inline).unwrap();
        assert_eq!(doc["$schema"], "https://x/s.json");
        assert!(doc["rules"].as_array().unwrap().is_empty());
    }
}
