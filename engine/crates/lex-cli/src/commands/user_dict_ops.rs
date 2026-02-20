use std::path::Path;
use std::process;

use lex_core::user_dict::UserDictionary;

macro_rules! die {
    ($result:expr, $($arg:tt)*) => {
        $result.unwrap_or_else(|e| {
            eprintln!($($arg)*, e);
            process::exit(1);
        })
    };
}

pub fn default_user_dict_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    format!("{home}/Library/Application Support/Lexime/user_dict.lxuw")
}

pub fn user_dict_add(path: &Path, reading: &str, surface: &str) {
    let dict = die!(
        UserDictionary::open(path),
        "Error opening user dictionary: {}"
    );
    if dict.register(reading, surface) {
        die!(dict.save(path), "Error saving user dictionary: {}");
        println!("Added: {reading} → {surface}");
    } else {
        println!("Already exists: {reading} → {surface}");
    }
}

pub fn user_dict_remove(path: &Path, reading: &str, surface: &str) {
    let dict = die!(
        UserDictionary::open(path),
        "Error opening user dictionary: {}"
    );
    if dict.unregister(reading, surface) {
        die!(dict.save(path), "Error saving user dictionary: {}");
        println!("Removed: {reading} → {surface}");
    } else {
        println!("Not found: {reading} → {surface}");
    }
}

pub fn user_dict_list(path: &Path) {
    let dict = die!(
        UserDictionary::open(path),
        "Error opening user dictionary: {}"
    );
    let entries = dict.list();
    if entries.is_empty() {
        println!("(empty)");
    } else {
        for (reading, surface) in &entries {
            println!("{reading}\t{surface}");
        }
        println!("---");
        println!("{} entries", entries.len());
    }
}
