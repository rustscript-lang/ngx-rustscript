//! POSIX-like shell word splitting for NGINX configure reports.
//!
//! Handles single quotes, double quotes, and backslash escapes. Rejects NUL,
//! control bytes, dangling backslashes, and unclosed quotes.

pub fn split_shell_words(input: &str) -> Result<Vec<String>, String> {
    if input.as_bytes().contains(&0) {
        return Err("configure arguments contain NUL".to_string());
    }

    let mut words = Vec::new();
    let mut cur = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut saw_token = false;

    while let Some(c) = chars.next() {
        let code = c as u32;
        if code < 0x20 && c != '\t' {
            return Err("configure arguments contain a control character".to_string());
        }

        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                cur.push(c);
            }
            continue;
        }

        if in_double {
            if c == '"' {
                in_double = false;
            } else if c == '\\' {
                match chars.next() {
                    Some(n @ ('"' | '\\' | '$' | '`')) => cur.push(n),
                    Some('\n') => {}
                    Some(n) => {
                        cur.push('\\');
                        cur.push(n);
                    }
                    None => return Err("configure arguments have a dangling backslash".to_string()),
                }
            } else {
                cur.push(c);
            }
            continue;
        }

        match c {
            '\'' => {
                in_single = true;
                saw_token = true;
            }
            '"' => {
                in_double = true;
                saw_token = true;
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                Some(n) => {
                    cur.push(n);
                    saw_token = true;
                }
                None => return Err("configure arguments have a dangling backslash".to_string()),
            },
            c if c.is_whitespace() => {
                if saw_token {
                    words.push(std::mem::take(&mut cur));
                    saw_token = false;
                }
            }
            c => {
                cur.push(c);
                saw_token = true;
            }
        }
    }

    if in_single || in_double {
        return Err("configure arguments have malformed quoting".to_string());
    }
    if saw_token {
        words.push(cur);
    }
    Ok(words)
}
