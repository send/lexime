use std::path::Path;
use std::process;

use lex_core::converter::{
    convert, convert_nbest, convert_nbest_with_history, convert_with_history,
};
use lex_core::dict::connection::ConnectionMatrix;
use lex_core::dict::TrieDictionary;
use lex_core::user_history::UserHistory;

macro_rules! die {
    ($result:expr, $($arg:tt)*) => {
        $result.unwrap_or_else(|e| {
            eprintln!($($arg)*, e);
            process::exit(1);
        })
    };
}

pub fn convert_cmd(dict_file: &str, conn_file: &str, kana: &str, n: usize, history: Option<&str>) {
    let dict = die!(
        TrieDictionary::open(Path::new(dict_file)),
        "Error opening dictionary: {}"
    );
    let conn = die!(
        ConnectionMatrix::open(Path::new(conn_file)),
        "Error opening connection matrix: {}"
    );

    let user_history = history.map(|path| {
        die!(
            UserHistory::open(Path::new(path)),
            "Error opening history: {}"
        )
    });

    if n <= 1 {
        let result = if let Some(ref h) = user_history {
            convert_with_history(&dict, Some(&conn), h, kana)
        } else {
            convert(&dict, Some(&conn), kana)
        };
        let segs: Vec<String> = result
            .iter()
            .map(|s| format!("{}({})", s.surface, s.reading))
            .collect();
        println!("{}", segs.join(" | "));
    } else {
        let nbest = if let Some(ref h) = user_history {
            convert_nbest_with_history(&dict, Some(&conn), h, kana, n)
        } else {
            convert_nbest(&dict, Some(&conn), kana, n)
        };
        for (i, path) in nbest.iter().enumerate() {
            let segs: Vec<String> = path
                .iter()
                .map(|s| format!("{}({})", s.surface, s.reading))
                .collect();
            println!("#{:>2}: {}", i + 1, segs.join(" | "));
        }
    }
}

pub fn conn_cost_cmd(conn_file: &str, left: u16, right: u16) {
    let conn = die!(
        ConnectionMatrix::open(Path::new(conn_file)),
        "Error opening connection matrix: {}"
    );
    let cost = conn.cost(left, right);
    let left_role = conn.role(left);
    let right_role = conn.role(right);
    let left_fw = conn.is_function_word(left);
    let right_fw = conn.is_function_word(right);

    let role_name = |r: u8| match r {
        0 => "CW",
        1 => "FW",
        2 => "Suffix",
        3 => "Prefix",
        _ => "?",
    };

    println!(
        "conn({left}, {right}) = {cost}  [{} {}{}→ {} {}{}]",
        left,
        role_name(left_role),
        if left_fw { "(fw)" } else { "" },
        right,
        role_name(right_role),
        if right_fw { "(fw)" } else { "" },
    );
}
