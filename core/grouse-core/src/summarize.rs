//! Model-written one-line labels for a run of tool calls (issue #13).
//!
//! The transcript store derives a *provisional* label from the thinking that
//! preceded a run (see `transcript::summarize`). This module is the rewrite: at
//! the end of a turn the core asks the model — on a hidden session, so the
//! request never appears in any chat — for one short line per run, and the
//! answer replaces the provisional label.
//!
//! Everything here is pure so it can be tested without a server; the plumbing
//! (hidden session, prompt, routing the reply) lives on `Core`/`Conn`.

/// How much of a run's thinking to show the model. Thinking runs to paragraphs;
/// the label is a handful of words, so the opening is enough to place it.
const THOUGHT_CHARS: usize = 400;

/// The instruction sent to the hidden summarizer session: one numbered item per
/// run, oldest first. `runs` is `(thinking, tool titles)`.
pub(crate) fn prompt(runs: &[(String, Vec<String>)]) -> String {
    // The wording is load-bearing: asked loosely, the model echoes the thinking
    // back verbatim, which is no better than the provisional label the store
    // already derived. "Compress, never copy" plus a verb-first rule is what
    // makes it write "Search web for Fairphone charging issues" instead.
    let mut s = String::from(
        "You are labelling steps of your own earlier work for a chat UI, so a long tool \
         run reads as a few short steps instead of a wall of chips.\n\
         Each numbered item is one step: the thinking that preceded it, and the tools it \
         ran.\n\
         Reply with ONE line per step, exactly \"<n>. <label>\", and nothing else.\n\
         A label is at most 8 words, starts with a verb, and names the step's PURPOSE. \
         Compress the thinking — never copy it.\n\n",
    );
    for (i, (thought, tools)) in runs.iter().enumerate() {
        let mut t: String = thought.chars().take(THOUGHT_CHARS).collect();
        if thought.chars().count() > THOUGHT_CHARS {
            t.push('…');
        }
        let t = t.replace('\n', " ");
        let tools = if tools.is_empty() { "(none)".to_string() } else { tools.join(", ") };
        s.push_str(&format!("{}. thinking: {} | tools: {}\n", i + 1, t.trim(), tools));
    }
    s
}

/// Pull `<n>. <label>` lines out of the model's reply, in the order it wrote
/// them. Lenient by design: prose, blank lines and bullets are ignored, and the
/// caller keeps the provisional label wherever the reply does not cover a run.
pub(crate) fn parse_labels(reply: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let rest = line[digits.len()..].trim_start_matches(['.', ')', ':', ' ']).trim();
        let Ok(n) = digits.parse::<usize>() else { continue };
        let label = clean_label(rest);
        if n > 0 && !label.is_empty() {
            out.push((n, label));
        }
    }
    out
}

/// A label as a chip wants it: one line, no quoting, no trailing full stop, capped.
fn clean_label(s: &str) -> String {
    let s = s.split('\n').next().unwrap_or("").trim();
    let s = s.trim_matches(['"', '\'', '`']).trim();
    let s = s.trim_end_matches(['.', ' ', '…']).trim();
    if s.chars().count() > 90 {
        format!("{}…", s.chars().take(89).collect::<String>().trim_end())
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_numbers_every_run_and_carries_its_tools() {
        let runs = vec![
            ("Let me look up the charging issue.".to_string(), vec!["kagi_search".into()]),
            ("Now check the todo.".to_string(), vec!["Read".into(), "Bash".into()]),
        ];
        let p = prompt(&runs);
        assert!(p.contains("1. thinking: Let me look up the charging issue. | tools: kagi_search"));
        assert!(p.contains("2. thinking: Now check the todo. | tools: Read, Bash"));
        assert!(p.contains("exactly \"<n>. <label>\""));
    }

    #[test]
    fn a_run_with_no_tools_says_so() {
        let p = prompt(&[("Hmm.".to_string(), Vec::new())]);
        assert!(p.contains("| tools: (none)"), "{p}");
    }

    #[test]
    fn long_thinking_is_cut_to_the_opening() {
        let p = prompt(&[("x".repeat(THOUGHT_CHARS + 50), vec!["Read".into()])]);
        assert!(p.contains('…'));
        // 400 chars of thinking plus the ellipsis.
        assert!(p.contains(&format!("{}…", "x".repeat(THOUGHT_CHARS))));
    }

    #[test]
    fn labels_parse_numbered_lines_and_ignore_prose() {
        let reply = "Here you go:\n1. Look up the charging issue\n2) Check the todo\n\n\
                     That should do it.";
        assert_eq!(
            parse_labels(reply),
            vec![
                (1, "Look up the charging issue".to_string()),
                (2, "Check the todo".to_string()),
            ]
        );
    }

    #[test]
    fn bullets_and_quotes_and_full_stops_are_stripped() {
        assert_eq!(
            parse_labels("- 3. \"Read the page.\""),
            vec![(3, "Read the page".to_string())]
        );
        assert_eq!(parse_labels("1. Look it up…"), vec![(1, "Look it up".to_string())]);
    }

    #[test]
    fn an_empty_or_unhelpful_reply_yields_nothing() {
        assert!(parse_labels("").is_empty());
        assert!(parse_labels("I cannot help with that.").is_empty());
        assert!(parse_labels("1.\n2. ").is_empty());
    }

    #[test]
    fn a_label_is_capped() {
        let long = format!("1. {}", "y".repeat(200));
        let labels = parse_labels(&long);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].1.chars().count(), 90);
    }
}
