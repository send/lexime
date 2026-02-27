use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum SnippetConfigError {
    #[error("TOML parse error: {0}")]
    Parse(String),
    #[error("undefined variable '{name}' in snippet '{key}'")]
    UndefinedVariable { key: String, name: String },
}

/// Parse a snippets.toml file (flat key = "body" format).
/// Validates that all variable references in bodies refer to known variables.
pub fn parse_snippets_toml(
    toml_str: &str,
    known_variables: &[String],
) -> Result<HashMap<String, String>, SnippetConfigError> {
    let table: HashMap<String, String> =
        toml::from_str(toml_str).map_err(|e| SnippetConfigError::Parse(e.to_string()))?;

    // Validate variable references in all bodies
    for (key, body) in &table {
        for var_name in extract_variable_names(body) {
            if !known_variables.contains(&var_name) {
                return Err(SnippetConfigError::UndefinedVariable {
                    key: key.clone(),
                    name: var_name,
                });
            }
        }
    }

    Ok(table)
}

/// Extract variable names referenced in a template string.
/// Recognizes `$name`, `${name}`, and skips `$$`.
fn extract_variable_names(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '$' {
            continue;
        }

        match chars.peek() {
            Some('$') => {
                chars.next(); // skip escaped $
            }
            Some('{') => {
                chars.next(); // consume '{'
                let mut name = String::new();
                let mut found_closing = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        found_closing = true;
                        break;
                    }
                    name.push(c);
                }
                if found_closing && !name.is_empty() {
                    names.push(name);
                }
            }
            Some(c) if c.is_ascii_alphanumeric() || *c == '_' => {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !name.is_empty() {
                    names.push(name);
                }
            }
            _ => {}
        }
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_snippets() {
        let toml = r#"
gh = "https://github.com/"
email = "user@example.com"
today = "Today is $date"
"#;
        let known = vec![
            "date".to_string(),
            "time".to_string(),
            "datetime".to_string(),
        ];
        let result = parse_snippets_toml(toml, &known).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result["gh"], "https://github.com/");
        assert_eq!(result["today"], "Today is $date");
    }

    #[test]
    fn test_parse_undefined_variable_error() {
        let toml = r#"
greeting = "Hello $nonexistent"
"#;
        let known = vec!["date".to_string()];
        let err = parse_snippets_toml(toml, &known).unwrap_err();
        assert!(matches!(err, SnippetConfigError::UndefinedVariable { .. }));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn test_parse_escaped_dollar_ok() {
        let toml = r#"
price = "$$100"
"#;
        let known: Vec<String> = vec![];
        let result = parse_snippets_toml(toml, &known).unwrap();
        assert_eq!(result["price"], "$$100");
    }

    #[test]
    fn test_parse_invalid_toml() {
        let err = parse_snippets_toml("not valid {{{", &[]).unwrap_err();
        assert!(matches!(err, SnippetConfigError::Parse(_)));
    }

    #[test]
    fn test_extract_variable_names() {
        assert_eq!(extract_variable_names("$foo"), vec!["foo"]);
        assert_eq!(extract_variable_names("${bar}"), vec!["bar"]);
        assert_eq!(extract_variable_names("$a and ${b}"), vec!["a", "b"]);
        assert!(extract_variable_names("$$escaped").is_empty());
        assert!(extract_variable_names("no vars").is_empty());
    }

    #[test]
    fn test_parse_braced_variable() {
        let toml = r#"
greeting = "Today: ${date}"
"#;
        let known = vec!["date".to_string()];
        let result = parse_snippets_toml(toml, &known).unwrap();
        assert_eq!(result["greeting"], "Today: ${date}");
    }
}
