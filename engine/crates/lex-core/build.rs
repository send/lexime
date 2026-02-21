fn main() {
    // Validate embedded TOML files at compile time.
    validate_toml(
        "src/default_settings.toml",
        include_str!("src/default_settings.toml"),
    );
    validate_toml(
        "src/romaji/default_romaji.toml",
        include_str!("src/romaji/default_romaji.toml"),
    );
}

fn validate_toml(path: &str, content: &str) {
    if content.parse::<toml::Value>().is_err() {
        panic!("{path} contains invalid TOML");
    }
}
