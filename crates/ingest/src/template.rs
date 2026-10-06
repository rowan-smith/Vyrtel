//! Minimal message-template rendering (`"User {UserId} logged in"`).
//!
//! Used when a producer sends only a template plus properties (Serilog
//! CLEF `@mt`). Supports `{Name}`, `{@Name}`, `{$Name}`, `{Name:format}`
//! and `{Name,alignment}` (format/alignment ignored) and `{{`/`}}` escapes.
//! Unknown names are left as-is.

use telemetry::{Fields, Value};

pub fn render_template(template: &str, props: &Fields) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut chars = template.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '{' if matches!(chars.peek(), Some((_, '{'))) => {
                chars.next();
                out.push('{');
            }
            '}' if matches!(chars.peek(), Some((_, '}'))) => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let rest = &template[i + 1..];
                let Some(end) = rest.find('}') else {
                    out.push_str(&template[i..]);
                    return out;
                };
                let token = &rest[..end];
                let name = token.trim_start_matches(['@', '$']).split([':', ',']).next().unwrap_or("");
                match props.lookup_path(name) {
                    Some(Value::String(s)) => out.push_str(s),
                    Some(v) => out.push_str(&v.to_display_string()),
                    None => {
                        out.push('{');
                        out.push_str(token);
                        out.push('}');
                    }
                }
                // Skip the token characters.
                for _ in 0..token.chars().count() + 1 {
                    chars.next();
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props() -> Fields {
        let v: Value =
            serde_json::from_str(r#"{"UserId": 42, "Name": "ana", "Amount": 1.5, "Obj": {"a": 1}}"#).unwrap();
        v.as_object().unwrap().clone()
    }

    #[test]
    fn renders_properties() {
        assert_eq!(render_template("User {UserId} ({Name}) paid {Amount:0.00}", &props()), "User 42 (ana) paid 1.5");
        assert_eq!(render_template("{@Obj} {$Name}", &props()), r#"{"a":1} ana"#);
    }

    #[test]
    fn escapes_and_unknowns() {
        assert_eq!(render_template("{{literal}} {Missing}", &props()), "{literal} {Missing}");
        assert_eq!(render_template("unterminated {Name", &props()), "unterminated {Name");
        assert_eq!(render_template("ünïcode {Name} ✓", &props()), "ünïcode ana ✓");
    }
}
