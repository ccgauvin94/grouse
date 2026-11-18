// SPDX-License-Identifier: AGPL-3.0-or-later

//! The dev-session advisor: a second opinion on the turn that just ended.
//!
//! Nothing here calls a model — this module is the PROMPT and the parsing, so the
//! register and the caps can be tested without a server. The plumbing (a hidden
//! session per endpoint, the request, the truncate that keeps it empty) is the same
//! path the tool-run summarizer uses, in `lib.rs` (`spawn_advisor_review`).
//!
//! Why a second turn on a hidden session rather than a delegate or a recipe:
//!
//! * a delegate is a SUB-AGENT (it gets its own tools and a turn budget) and its
//!   traffic lands in the transcript — this is a review, not more work;
//! * a recipe is a server-side object with its own instructions and extensions,
//!   which we would be re-pointing at the user's chat every turn;
//! * a hidden session is `_meta.hidden` (`SessionType::Hidden`, filtered out of
//!   every session list), costs one ordinary model call, and is truncated back to
//!   empty afterwards — so it cannot accumulate a conversation of its own.
//!
//! The review is deliberately told to answer "nothing material" rather than pad
//! itself: a reviewer that always finds three things to say trains the reader to
//! ignore it.

use crate::ToolCall;

/// How much of a call's arguments to show. A `write`'s arguments hold a whole file,
/// and the point is the SHAPE of what changed, not a full re-read.
const ARGS_CHARS: usize = 700;

/// How much of a result to show. An `edit` answers with a diff, which is the most
/// valuable thing in the request — but a test run can answer with thousands of
/// lines, so it is capped rather than trusted.
const OUTPUT_CHARS: usize = 500;

/// How many calls one review covers. A turn can loop for dozens of steps; a review
/// of the first N is worth more than a request the model truncates itself.
const MAX_CALLS: usize = 24;

/// The opening: what this is, and the register it must answer in.
///
/// Load-bearing wording: without "not a summary" the model narrates the changes
/// back; without "nothing material" it invents concerns to seem useful; the
/// "no praise" line exists because "the code is well structured" is the single
/// most common useless line a reviewing model writes.
const INSTRUCTIONS: &str = "\
You are reviewing the work of an agent that just finished a turn in a coding session. \
You are a second opinion, not the author: the agent cannot see your reply, and nobody \
will act on it automatically.

You get the user's request, then the tools the agent ran and what they returned.

Answer as a reviewer of the CHANGES, in at most six short bullets. Say what is WRONG or \
risky, in this order of importance:
- a defect: logic errors, wrong conditions, off-by-one, unhandled failure, a broken edit
- a missed case: input, state or platform the change does not handle, a caller left behind
- a risk: destructive or irreversible action, a secret or a path that should not be there
- unfinished work: a TODO left in place, a piece of the request evidently not done

Rules:
- Quote the file or the command you mean. \"`shell`: `rm -rf build` runs unconditionally\" \
is useful; \"consider error handling\" is not.
- No praise, no summary of what the agent did, no restating the request, no preamble.
- Do not propose a refactor, a rename or a style change unless it is the defect.
- If the work looks correct and complete, reply with exactly: nothing material
";

/// The prompt for one turn's review.
///
/// `ask` is what the user asked for (it is what "unfinished work" is measured
/// against), and `calls` is the turn's tool calls in order.
pub(crate) fn prompt(ask: &str, calls: &[ToolCall]) -> String {
    let mut s = String::from(INSTRUCTIONS);
    s.push_str("\n## The request\n");
    if ask.trim().is_empty() {
        // A turn with no visible ask (a queued prompt drained after another turn,
        // a resume) still has tools to review; saying so is better than an empty
        // heading the model has to interpret.
        s.push_str("(not available for this turn)\n");
    } else {
        s.push_str(&clip(ask.trim(), 1200));
        s.push('\n');
    }
    s.push_str("\n## What the agent ran\n");
    if calls.is_empty() {
        // Nothing to review is a legitimate outcome, and saying it plainly stops
        // the model inventing a change set from the request alone.
        s.push_str("(no tools ran this turn)\n");
        return s;
    }
    for (i, call) in calls.iter().take(MAX_CALLS).enumerate() {
        s.push_str(&format!(
            "\n{}. {} [{}]\n",
            i + 1,
            clip(&call.title, 120),
            if call.status.is_empty() { "unknown" } else { &call.status }
        ));
        if !call.detail.trim().is_empty() {
            s.push_str(&format!("   args: {}\n", clip(&call.detail, ARGS_CHARS)));
        }
        if !call.output.trim().is_empty() {
            s.push_str(&format!("   result: {}\n", clip(&call.output, OUTPUT_CHARS)));
        }
    }
    if calls.len() > MAX_CALLS {
        s.push_str(&format!(
            "\n({} later calls of this turn are not shown)\n",
            calls.len() - MAX_CALLS
        ));
    }
    s
}

/// Whether the model found nothing worth saying. Matched instead of shown, so the
/// UI can stay silent rather than print a card that says "nothing".
pub(crate) fn nothing_material(reply: &str) -> bool {
    let t = reply.trim().trim_end_matches('.').trim();
    t.eq_ignore_ascii_case("nothing material")
}

/// Collapse whitespace and cut to `max` chars, marking the cut. Newlines inside an
/// argument or a result are noise once it is quoted in a prompt: they double the
/// request without adding meaning.
fn clip(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(title: &str, detail: &str, output: &str) -> ToolCall {
        ToolCall {
            id: "t".into(),
            title: title.into(),
            detail: detail.into(),
            output: output.into(),
            status: "completed".into(),
            thought: String::new(),
            seq: 0,
            steps: Vec::new(),
        }
    }

    #[test]
    fn the_prompt_carries_the_ask_and_the_calls_in_order() {
        let p = prompt(
            "add a retry to the fetch",
            &[
                call("edit", r#"{"path":"/w/a.rs","old":"x","new":"y"}"#, "done"),
                call("shell", r#"{"command":"cargo test"}"#, "3 passed"),
            ],
        );
        assert!(p.contains("add a retry to the fetch"));
        let edit = p.find("1. edit").expect("first call");
        let shell = p.find("2. shell").expect("second call");
        assert!(edit < shell, "calls keep the turn's order");
        assert!(p.contains("cargo test"));
        assert!(p.contains("[completed]"));
    }

    #[test]
    fn a_turn_with_no_tools_says_so_rather_than_leaving_it_blank() {
        let p = prompt("what time is it", &[]);
        assert!(p.contains("(no tools ran this turn)"));
    }

    #[test]
    fn a_missing_ask_is_stated_not_invented() {
        let p = prompt("   ", &[call("shell", "ls", "")]);
        assert!(p.contains("(not available for this turn)"));
    }

    #[test]
    fn long_arguments_and_results_are_clipped() {
        let long = "x".repeat(5_000);
        let p = prompt("do it", &[call("write", &long, &long)]);
        assert!(p.len() < 4_000, "a big write must not paste a whole file: {}", p.len());
        assert!(p.contains('…'));
    }

    #[test]
    fn later_calls_are_counted_not_dropped_silently() {
        let calls: Vec<ToolCall> = (0..MAX_CALLS + 3)
            .map(|i| call(&format!("tool{i}"), "{}", ""))
            .collect();
        let p = prompt("go", &calls);
        assert!(p.contains("3 later calls of this turn are not shown"));
    }

    #[test]
    fn the_nothing_material_answer_is_recognised_with_punctuation_and_case() {
        assert!(nothing_material("nothing material"));
        assert!(nothing_material("  Nothing Material.  "));
        assert!(!nothing_material("Nothing material, except the unwrap on line 4."));
        assert!(!nothing_material(""));
    }
}
