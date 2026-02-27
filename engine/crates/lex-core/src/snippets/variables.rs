use std::collections::HashMap;

use serde::Deserialize;
use time::OffsetDateTime;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum SnippetVariable {
    #[serde(rename = "date")]
    Date { format: String },
    #[serde(rename = "static")]
    Static { value: String },
}

pub struct VariableResolver {
    vars: HashMap<String, SnippetVariable>,
}

struct EraEntry {
    name: &'static str,
    start_year: i32,
    start_month: u8,
    start_day: u8,
}

const ERA_TABLE: &[EraEntry] = &[
    EraEntry {
        name: "令和",
        start_year: 2019,
        start_month: 5,
        start_day: 1,
    },
    EraEntry {
        name: "平成",
        start_year: 1989,
        start_month: 1,
        start_day: 8,
    },
    EraEntry {
        name: "昭和",
        start_year: 1926,
        start_month: 12,
        start_day: 25,
    },
    EraEntry {
        name: "大正",
        start_year: 1912,
        start_month: 7,
        start_day: 30,
    },
    EraEntry {
        name: "明治",
        start_year: 1868,
        start_month: 1,
        start_day: 25,
    },
];

fn builtin_defaults() -> HashMap<String, SnippetVariable> {
    let mut m = HashMap::new();
    m.insert(
        "date".to_string(),
        SnippetVariable::Date {
            format: "%Y-%m-%d".to_string(),
        },
    );
    m.insert(
        "time".to_string(),
        SnippetVariable::Date {
            format: "%H:%M".to_string(),
        },
    );
    m.insert(
        "datetime".to_string(),
        SnippetVariable::Date {
            format: "%Y-%m-%d %H:%M".to_string(),
        },
    );
    m.insert(
        "date_jp".to_string(),
        SnippetVariable::Date {
            format: "%Y年%m月%d日".to_string(),
        },
    );
    m.insert(
        "wareki".to_string(),
        SnippetVariable::Date {
            format: "%G%gy年%m月%d日".to_string(),
        },
    );
    m.insert(
        "wareki_ym".to_string(),
        SnippetVariable::Date {
            format: "%G%gy年%m月".to_string(),
        },
    );
    m.insert(
        "wareki_y".to_string(),
        SnippetVariable::Date {
            format: "%G%gy年".to_string(),
        },
    );
    m.insert(
        "year".to_string(),
        SnippetVariable::Date {
            format: "%Y".to_string(),
        },
    );
    m
}

impl VariableResolver {
    pub fn new(user_vars: HashMap<String, SnippetVariable>) -> Self {
        let mut vars = builtin_defaults();
        // User-defined variables override builtins
        vars.extend(user_vars);
        Self { vars }
    }

    pub fn known_names(&self) -> Vec<String> {
        self.vars.keys().cloned().collect()
    }

    pub fn expand(&self, template: &str) -> String {
        let mut result = String::with_capacity(template.len());
        let mut chars = template.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch != '$' {
                result.push(ch);
                continue;
            }

            // Peek next char
            match chars.peek() {
                Some('$') => {
                    // $$ → literal $
                    chars.next();
                    result.push('$');
                }
                Some('{') => {
                    // ${name}
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
                    if found_closing {
                        result.push_str(&self.resolve_var(&name));
                    } else {
                        // Malformed ${... → preserve as literal
                        result.push('$');
                        result.push('{');
                        result.push_str(&name);
                    }
                }
                Some(c) if c.is_ascii_alphanumeric() || *c == '_' => {
                    // $name
                    let mut name = String::new();
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_alphanumeric() || c == '_' {
                            name.push(c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    result.push_str(&self.resolve_var(&name));
                }
                _ => {
                    // Lone $ at end or before non-identifier char
                    result.push('$');
                }
            }
        }

        result
    }

    fn resolve_var(&self, name: &str) -> String {
        match self.vars.get(name) {
            Some(SnippetVariable::Date { format }) => format_date(format),
            Some(SnippetVariable::Static { value }) => value.clone(),
            None => format!("${{{name}}}"),
        }
    }
}

fn format_date(fmt: &str) -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let (era_name, era_year) = current_era(&now);

    let mut result = String::with_capacity(fmt.len());
    let mut chars = fmt.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '%' {
            match chars.peek() {
                Some('Y') => {
                    chars.next();
                    result.push_str(&format!("{:04}", now.year()));
                }
                Some('m') => {
                    chars.next();
                    result.push_str(&format!("{:02}", now.month() as u8));
                }
                Some('d') => {
                    chars.next();
                    result.push_str(&format!("{:02}", now.day()));
                }
                Some('H') => {
                    chars.next();
                    result.push_str(&format!("{:02}", now.hour()));
                }
                Some('M') => {
                    chars.next();
                    result.push_str(&format!("{:02}", now.minute()));
                }
                Some('S') => {
                    chars.next();
                    result.push_str(&format!("{:02}", now.second()));
                }
                Some('G') => {
                    chars.next();
                    result.push_str(era_name);
                }
                Some('g') => {
                    chars.next();
                    // %gy = era year
                    if chars.peek() == Some(&'y') {
                        chars.next();
                        result.push_str(&era_year.to_string());
                    } else {
                        result.push('%');
                        result.push('g');
                    }
                }
                Some('%') => {
                    chars.next();
                    result.push('%');
                }
                Some(_) => {
                    // Unknown specifier — preserve as-is
                    result.push('%');
                    result.push(chars.next().unwrap());
                }
                None => {
                    result.push('%');
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

fn current_era(now: &OffsetDateTime) -> (&'static str, i32) {
    let y = now.year();
    let m = now.month() as u8;
    let d = now.day();

    for era in ERA_TABLE {
        if y > era.start_year
            || (y == era.start_year && m > era.start_month)
            || (y == era.start_year && m == era.start_month && d >= era.start_day)
        {
            return (era.name, y - era.start_year + 1);
        }
    }
    // Before Meiji — just return year as-is
    ("", y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_static_variable() {
        let mut user = HashMap::new();
        user.insert(
            "email".to_string(),
            SnippetVariable::Static {
                value: "test@example.com".to_string(),
            },
        );
        let resolver = VariableResolver::new(user);
        assert_eq!(resolver.expand("$email"), "test@example.com");
        assert_eq!(resolver.expand("${email}"), "test@example.com");
        assert_eq!(resolver.expand("hi $email!"), "hi test@example.com!");
    }

    #[test]
    fn test_expand_dollar_escape() {
        let resolver = VariableResolver::new(HashMap::new());
        assert_eq!(resolver.expand("$$100"), "$100");
        assert_eq!(resolver.expand("$$"), "$");
    }

    #[test]
    fn test_expand_unknown_variable() {
        let resolver = VariableResolver::new(HashMap::new());
        assert_eq!(resolver.expand("$unknown"), "${unknown}");
        assert_eq!(resolver.expand("${unknown}"), "${unknown}");
    }

    #[test]
    fn test_expand_builtin_date() {
        let resolver = VariableResolver::new(HashMap::new());
        let expanded = resolver.expand("$date");
        // Should be YYYY-MM-DD format
        assert_eq!(expanded.len(), 10);
        assert_eq!(&expanded[4..5], "-");
        assert_eq!(&expanded[7..8], "-");
    }

    #[test]
    fn test_expand_builtin_year() {
        let resolver = VariableResolver::new(HashMap::new());
        let expanded = resolver.expand("$year");
        assert_eq!(expanded.len(), 4);
        let _: i32 = expanded.parse().expect("year should be numeric");
    }

    #[test]
    fn test_era_lookup_reiwa() {
        let now = time::OffsetDateTime::now_utc();
        if now.year() >= 2019 {
            let (era, _year) = current_era(&now);
            assert_eq!(era, "令和");
        }
    }

    #[test]
    fn test_user_overrides_builtin() {
        let mut user = HashMap::new();
        user.insert(
            "date".to_string(),
            SnippetVariable::Date {
                format: "%Y/%m/%d".to_string(),
            },
        );
        let resolver = VariableResolver::new(user);
        let expanded = resolver.expand("$date");
        // Should be YYYY/MM/DD format
        assert_eq!(&expanded[4..5], "/");
    }

    #[test]
    fn test_known_names_includes_builtins() {
        let resolver = VariableResolver::new(HashMap::new());
        let names = resolver.known_names();
        assert!(names.contains(&"date".to_string()));
        assert!(names.contains(&"time".to_string()));
        assert!(names.contains(&"datetime".to_string()));
        assert!(names.contains(&"wareki".to_string()));
        assert!(names.contains(&"wareki_ym".to_string()));
        assert!(names.contains(&"wareki_y".to_string()));
        assert!(names.contains(&"year".to_string()));
        assert!(names.contains(&"date_jp".to_string()));
    }

    #[test]
    fn test_era_placeholder_order() {
        // %gy must be replaced before %G to avoid partial match
        let now = time::OffsetDateTime::now_utc();
        if now.year() >= 2019 {
            let result = format_date("%G%gy年");
            assert!(result.starts_with("令和"));
            // Should NOT contain "%G" or "令和令和"
            assert!(!result.contains("令和令和"));
            // Era year should follow era name
            let after_era = &result["令和".len()..];
            let year_part: String = after_era
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            assert!(!year_part.is_empty());
        }
    }

    #[test]
    fn test_unclosed_brace_preserved_as_literal() {
        let resolver = VariableResolver::new(HashMap::new());
        assert_eq!(resolver.expand("${unclosed"), "${unclosed");
        assert_eq!(resolver.expand("before ${unclosed"), "before ${unclosed");
    }

    #[test]
    fn test_lone_dollar_preserved() {
        let resolver = VariableResolver::new(HashMap::new());
        assert_eq!(resolver.expand("$ "), "$ ");
        assert_eq!(resolver.expand("$"), "$");
    }
}
