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

    println!("cargo:rerun-if-changed=src/default_settings.toml");
    println!("cargo:rerun-if-changed=src/romaji/default_romaji.toml");
}

fn validate_toml(path: &str, content: &str) {
    content
        .parse::<toml::Value>()
        .unwrap_or_else(|e| panic!("{path} contains invalid TOML: {e}"));
}
