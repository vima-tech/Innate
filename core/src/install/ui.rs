use super::*;

pub(super) fn tty() -> bool {
    #[cfg(unix)]
    unsafe {
        use std::os::unix::io::AsRawFd;
        libc::isatty(io::stdout().as_raw_fd()) == 1 && std::env::var("NO_COLOR").is_err()
    }
    #[cfg(not(unix))]
    false
}

pub(super) fn c(s: &str, code: u8) -> String {
    if tty() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub(super) fn green(s: &str) -> String {
    c(s, 32)
}
pub(super) fn gray(s: &str) -> String {
    c(s, 90)
}
pub(super) fn bold(s: &str) -> String {
    c(s, 1)
}
pub(super) fn cyan(s: &str) -> String {
    c(s, 36)
}
pub(super) fn yellow(s: &str) -> String {
    c(s, 33)
}
pub(super) fn dim(s: &str) -> String {
    c(s, 2)
}

/// `┌  title`
pub(super) fn box_open(title: &str) {
    println!("{}", green(&format!("┌  {}", bold(title))));
    println!("{}", gray("│"));
}

/// `└  message`
pub(super) fn box_close(msg: &str) {
    println!("{}", gray("│"));
    println!("{}", green(&format!("└  {msg}")));
}

/// Gray vertical bar separator
pub(super) fn sep() {
    println!("{}", gray("│"));
}

/// `◇  question`
pub(super) fn question(q: &str) {
    println!("{}", cyan(&format!("◇  {q}")));
}

/// `│  text`
pub(super) fn info(text: &str) {
    println!("{}  {text}", gray("│"));
}

/// `◆  text`  (result / completed step)
pub(super) fn result_line(text: &str) {
    println!("{}  {text}", green("◆"));
}

/// `◆  text` in yellow (warning / unchanged)
pub(super) fn warn_line(text: &str) {
    println!("{}  {}", yellow("◆"), text);
}

// ── Interactive prompts ───────────────────────────────────────────────────────

/// Multi-select prompt. Returns a `Vec<bool>` parallel to `options`.
/// `selected[i] = true` means the option was confirmed.
///
/// Display:
/// ```text
/// ◇  prompt
/// │  [1] ✓ option A
/// │  [2] ✓ option B
/// │  [3] ✗ option C
/// │
/// │  ENTER to confirm, type a number to toggle:
/// │  >
/// ```
pub(super) fn prompt_multi_select(prompt: &str, options: &[(&str, bool)]) -> Vec<bool> {
    question(prompt);
    let mut selected: Vec<bool> = options.iter().map(|(_, s)| *s).collect();

    loop {
        for (i, (name, _)) in options.iter().enumerate() {
            let mark = if selected[i] {
                green("✓")
            } else {
                gray("✗")
            };
            info(&format!("[{}] {mark} {name}", i + 1));
        }
        sep();
        info("ENTER to confirm, or type a number to toggle:");
        print!("{}  {} ", gray("│"), dim("▶"));
        io::stdout().flush().ok();

        let line = read_line().trim().to_string();
        if line.is_empty() {
            break;
        }

        if let Ok(n) = line.parse::<usize>() {
            if n >= 1 && n <= options.len() {
                selected[n - 1] = !selected[n - 1];
                // Redraw — clear the lines we just printed (options + sep + info + prompt)
                let lines_back = options.len() + 3;
                if tty() {
                    print!("\x1b[{}A\x1b[J", lines_back);
                    io::stdout().flush().ok();
                }
                continue;
            }
        }
        break;
    }

    // Print the confirmed answer line.
    let chosen: Vec<&str> = options
        .iter()
        .zip(selected.iter())
        .filter(|(_, &s)| s)
        .map(|((name, _), _)| *name)
        .collect();
    let answer = if chosen.is_empty() {
        "none".to_string()
    } else {
        chosen.join(", ")
    };
    info(&green(&answer));
    sep();

    selected
}

/// Yes/No confirm. Returns true if the user says yes (default can be true or false).
pub(super) fn prompt_confirm(prompt: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "Y/n" } else { "y/N" };
    question(&format!("{prompt} ({hint})"));
    print!("{}  {} ", gray("│"), dim("▶"));
    io::stdout().flush().ok();

    let line = read_line().trim().to_lowercase();
    let result = if line.is_empty() {
        default_yes
    } else {
        line.starts_with('y')
    };
    info(&green(if result { "Yes" } else { "No" }));
    sep();
    result
}

/// Single-select from a list of options. Returns the chosen index.
pub(super) fn prompt_select(prompt: &str, options: &[&str]) -> usize {
    question(prompt);
    for (i, opt) in options.iter().enumerate() {
        info(&format!("[{}] {opt}", i + 1));
    }
    sep();
    print!("{}  {} ", gray("│"), dim("▶"));
    io::stdout().flush().ok();

    let line = read_line().trim().to_string();
    let idx = line
        .parse::<usize>()
        .unwrap_or(1)
        .saturating_sub(1)
        .min(options.len() - 1);
    info(&green(options[idx]));
    sep();
    idx
}

pub(super) fn read_line() -> String {
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).ok();
    // In pipe/non-TTY mode stdin has no echo, so we need an explicit newline
    // to move past the ▶ prompt before printing the answer.
    #[cfg(unix)]
    if unsafe { libc::isatty(0) } == 0 {
        println!();
    }
    line
}

/// Free-text input prompt. Returns the user's input, or `default` if empty.
pub(super) fn prompt_text(prompt: &str, default: &str, hint: &str) -> String {
    if hint.is_empty() {
        question(&format!(
            "{prompt} {}",
            dim(&format!("(default: {default})"))
        ));
    } else {
        question(&format!("{prompt} {}", dim(hint)));
    }
    print!("{}  {} ", gray("│"), dim("▶"));
    io::stdout().flush().ok();
    let line = read_line().trim().to_string();
    let result = if line.is_empty() {
        default.to_string()
    } else {
        line
    };
    info(&green(if result.is_empty() {
        "skipped"
    } else {
        &result
    }));
    sep();
    result
}

/// Masked API key input — same as prompt_text but prints "••••••••" in the result line.
pub(super) fn prompt_secret(prompt: &str, hint: &str) -> String {
    question(&format!("{prompt} {}", dim(hint)));
    print!("{}  {} ", gray("│"), dim("▶"));
    io::stdout().flush().ok();
    let line = read_line().trim().to_string();
    let display = if line.is_empty() {
        "skipped"
    } else {
        "••••••••"
    };
    info(&green(display));
    sep();
    line
}
