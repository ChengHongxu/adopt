//! `adopt(host): +ripgrep(pixi) +htop(apt) -tmux(apt) ~zshrc`

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Add {
        name: String,
        tag: String,
    },
    Remove {
        name: String,
        tag: String,
    },
    /// A package entry edited in place (name mapping, pixi env body).
    Update {
        name: String,
        tag: String,
    },
    /// A dotfile, shown by basename without the leading dot.
    Config {
        name: String,
    },
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Change::Add { name, tag } => write!(f, "+{name}({tag})"),
            Change::Remove { name, tag } => write!(f, "-{name}({tag})"),
            Change::Update { name, tag } => write!(f, "~{name}({tag})"),
            Change::Config { name } => write!(f, "~{name}"),
        }
    }
}

const SUBJECT_MAX: usize = 72;

/// Subject line with every change if it fits, otherwise a count with the list in the body.
pub fn message(host: &str, changes: &[Change]) -> String {
    let items: Vec<String> = changes.iter().map(ToString::to_string).collect();
    let full = format!("adopt({host}): {}", items.join(" "));
    if full.chars().count() <= SUBJECT_MAX {
        return full;
    }
    let mut subject = format!("adopt({host}):");
    let mut shown = 0;
    for item in &items {
        let rest = items.len() - shown - 1;
        let suffix = if rest > 0 {
            format!(" 等 {} 项", items.len())
        } else {
            String::new()
        };
        if subject.chars().count() + 1 + item.chars().count() + suffix.chars().count() > SUBJECT_MAX
        {
            break;
        }
        subject.push(' ');
        subject.push_str(item);
        shown += 1;
    }
    if shown < items.len() {
        subject.push_str(&format!(" 等 {} 项", items.len()));
    }
    format!("{subject}\n\n{}\n", items.join("\n"))
}

/// The part after `adopt(host): ` for the "已提交并推送" line.
pub fn summary(changes: &[Change]) -> String {
    changes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(n: &str, t: &str) -> Change {
        Change::Add {
            name: n.into(),
            tag: t.into(),
        }
    }

    #[test]
    fn short_message_is_one_line() {
        let cs = [
            add("ripgrep", "pixi"),
            add("htop", "apt"),
            Change::Config {
                name: "zshrc".into(),
            },
        ];
        assert_eq!(
            message("dev", &cs),
            "adopt(dev): +ripgrep(pixi) +htop(apt) ~zshrc"
        );
    }

    #[test]
    fn long_message_moves_list_to_body() {
        let cs: Vec<Change> = (0..20)
            .map(|i| add(&format!("package-{i}"), "apt"))
            .collect();
        let msg = message("dev", &cs);
        let (subject, body) = msg.split_once("\n\n").unwrap();
        assert!(subject.chars().count() <= SUBJECT_MAX, "{subject}");
        assert!(subject.ends_with("等 20 项"), "{subject}");
        assert_eq!(body.lines().count(), 20);
    }
}
