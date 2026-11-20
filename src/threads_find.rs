use std::collections::HashMap;
use std::ops::Range;

use eframe::egui::{self, Color32};
use egui_commonmark::{CommonMarkCache, SearchOptions};
use fancy_regex::{Regex, RegexBuilder};

use crate::threads_db::{Block, Message, Role};

pub const MATCH_BG: Color32 = Color32::from_rgba_premultiplied(70, 57, 6, 70);
pub const ACTIVE_BG: Color32 = Color32::from_rgba_premultiplied(169, 78, 15, 170);

/// The text of a block that a match is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Text,
    Title,
    Input,
    Output,
    /// The diff of the file at this index in the output.
    Diff(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub message: usize,
    pub block: usize,
    pub part: Part,
    /// The match's index among the matches of its part.
    pub ordinal: usize,
}

/// The matches of one part, and which of them is the current one.
pub struct Marks<'a> {
    pub ranges: &'a [Range<usize>],
    pub active: Option<usize>,
}

#[derive(Default)]
pub struct Find {
    pub query: String,
    hits: Vec<Hit>,
    ranges: HashMap<(usize, usize, Part), Vec<Range<usize>>>,
    current: Option<usize>,
    /// Markdown blocks that hold highlights in `CommonMarkCache`, to clear them on the next search.
    marked: Vec<egui::Id>,
    active_markdown: Option<egui::Id>,
}

/// The viewer id of a Markdown block, which also keys its search state in `CommonMarkCache`.
pub fn markdown_id(seq: i64, block: usize) -> egui::Id {
    egui::Id::NULL.with(("markdown", seq, block))
}

pub fn renders_markdown(block: &Block, role: &Role) -> bool {
    match block {
        Block::Text(_) => *role != Role::User,
        Block::Summary(_) => true,
        _ => false,
    }
}

/// Matches each word of the query, ignoring case, like the thread list search.
fn pattern(query: &str) -> Option<String> {
    let words: Vec<_> = query.split_whitespace().map(fancy_regex::escape).collect();
    (!words.is_empty()).then(|| words.join("|"))
}

fn ranges(regex: &Regex, text: &str) -> Vec<Range<usize>> {
    regex
        .find_iter(text)
        .filter_map(Result::ok)
        .map(|found| found.range())
        .filter(|range| !range.is_empty())
        .collect()
}

/// The texts of a block that are drawn as plain text, in display order.
fn plain_parts(block: &Block) -> Vec<(Part, &str)> {
    match block {
        Block::Text(text) | Block::Error(text) | Block::Other(text) => {
            vec![(Part::Text, text.as_str())]
        }
        Block::Tool(call) => {
            let mut parts = vec![
                (Part::Title, call.title.as_str()),
                (Part::Input, call.input.as_str()),
            ];
            if let Some(output) = &call.output {
                parts.push((Part::Output, output.text.as_str()));
                parts.extend(diffs(output));
            }
            parts
        }
        Block::Output(output) => {
            let mut parts = vec![(Part::Output, output.text.as_str())];
            parts.extend(diffs(output));
            parts
        }
        Block::Reasoning(_) | Block::Summary(_) => Vec::new(),
    }
}

fn diffs(output: &crate::threads_db::ToolOutput) -> impl Iterator<Item = (Part, &str)> {
    output
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| (Part::Diff(index), file.diff.as_str()))
}

impl Find {
    /// Finds the query in every block of the transcript, and forgets the current match.
    pub fn search(&mut self, messages: &[Message], markdown: &mut CommonMarkCache) {
        for id in std::mem::take(&mut self.marked) {
            let cache = markdown.search_cache_mut(&id);
            cache.search_query.clear();
            cache.search_ranges.clear();
            cache.active_match = None;
            cache.sync_active_search_range();
        }
        self.active_markdown = None;
        self.hits.clear();
        self.ranges.clear();
        self.current = None;
        let Some(pattern) = pattern(&self.query) else {
            return;
        };
        let Ok(regex) = RegexBuilder::new(&pattern).case_insensitive(true).build() else {
            return;
        };
        for (message_index, message) in messages.iter().enumerate() {
            for (block_index, block) in message.blocks.iter().enumerate() {
                if let (true, Block::Text(text) | Block::Summary(text)) =
                    (renders_markdown(block, &message.role), block)
                {
                    // Parsing every reply on each keystroke is slow, so blocks without a match in the source are skipped.
                    if !regex.is_match(text).unwrap_or(false) {
                        continue;
                    }
                    // egui_commonmark matches the rendered text only, so link targets and syntax never count.
                    let id = markdown_id(message.seq, block_index);
                    let cache = markdown.search_cache_mut(&id);
                    cache.search_query.clone_from(&pattern);
                    cache.search_options = SearchOptions::REGEX;
                    markdown.update_search_matches(&id, text);
                    let cache = markdown.search_cache_mut(&id);
                    cache.active_match = None;
                    cache.sync_active_search_range();
                    let found = cache.search_ranges.clone();
                    self.marked.push(id);
                    self.add(message_index, block_index, Part::Text, found);
                } else {
                    for (part, text) in plain_parts(block) {
                        self.add(message_index, block_index, part, ranges(&regex, text));
                    }
                }
            }
        }
    }

    fn add(&mut self, message: usize, block: usize, part: Part, found: Vec<Range<usize>>) {
        if found.is_empty() {
            return;
        }
        self.hits.extend((0..found.len()).map(|ordinal| Hit {
            message,
            block,
            part,
            ordinal,
        }));
        self.ranges.insert((message, block, part), found);
    }

    pub fn current(&self) -> Option<Hit> {
        self.current.map(|index| self.hits[index])
    }

    /// Keeps the current match after a new search when it still exists, as when the transcript grew.
    pub fn restore(&mut self, index: Option<usize>) {
        self.current = index.filter(|&index| index < self.hits.len());
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    /// Moves to the next or previous match, wrapping around. Returns whether there is one.
    pub fn step(&mut self, forward: bool) -> bool {
        let len = self.hits.len();
        if len == 0 {
            return false;
        }
        self.current = Some(match (self.current, forward) {
            (None, true) => 0,
            (None, false) => len - 1,
            (Some(index), true) => (index + 1) % len,
            (Some(index), false) => (index + len - 1) % len,
        });
        true
    }

    /// Marks the current match in its Markdown block, which `egui_commonmark` highlights and scrolls to.
    pub fn activate(&mut self, messages: &[Message], markdown: &mut CommonMarkCache, scroll: bool) {
        if let Some(id) = self.active_markdown.take() {
            let cache = markdown.search_cache_mut(&id);
            cache.active_match = None;
            cache.sync_active_search_range();
        }
        let Some(hit) = self.current() else {
            return;
        };
        let message = &messages[hit.message];
        if !renders_markdown(&message.blocks[hit.block], &message.role) {
            return;
        }
        let id = markdown_id(message.seq, hit.block);
        let cache = markdown.search_cache_mut(&id);
        cache.active_match = Some(hit.ordinal);
        cache.sync_active_search_range();
        if scroll {
            cache.scroll_to_active_search_match();
        }
        self.active_markdown = Some(id);
    }

    pub fn marks(&self, message: usize, block: usize, part: Part) -> Option<Marks<'_>> {
        let ranges = self.ranges.get(&(message, block, part))?;
        let active = self
            .current()
            .filter(|hit| (hit.message, hit.block, hit.part) == (message, block, part))
            .map(|hit| hit.ordinal);
        Some(Marks { ranges, active })
    }

    pub fn counter(&self) -> String {
        match (self.query.trim().is_empty(), self.current, self.hits.len()) {
            (true, _, _) => String::new(),
            (false, _, 0) => "No matches".to_owned(),
            (false, Some(index), len) => format!("{} of {len}", index + 1),
            (false, None, len) => format!("{len} matches"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threads_db::{FileChange, ToolCall, ToolOutput};

    fn message(seq: i64, role: Role, blocks: Vec<Block>) -> Message {
        Message {
            seq,
            timestamp: 0,
            role,
            blocks,
        }
    }

    fn transcript() -> Vec<Message> {
        let call = ToolCall {
            id: "t1".into(),
            title: "Bash  cargo build".into(),
            input: "command: cargo build".into(),
            output: Some(ToolOutput {
                text: "Compiling\nerror: build failed".into(),
                files: vec![FileChange {
                    path: "a.rs".into(),
                    change: "Update".into(),
                    diff: "-old\n+build()\n".into(),
                }],
                ..ToolOutput::default()
            }),
        };
        vec![
            message(
                1,
                Role::User,
                vec![Block::Text("Please fix the BUILD".into())],
            ),
            message(
                2,
                Role::Assistant,
                vec![
                    Block::Reasoning(Some(10)),
                    Block::Text(
                        "Run `cargo build`, then build, see [docs](https://build.example).".into(),
                    ),
                    Block::Tool(call),
                ],
            ),
        ]
    }

    #[test]
    fn quotes_words_into_an_alternation() {
        assert_eq!(pattern("a.b  c").as_deref(), Some(r"a\.b|c"));
        assert_eq!(pattern("   "), None);
    }

    #[test]
    fn finds_every_word_in_display_order() {
        let messages = transcript();
        let mut markdown = CommonMarkCache::default();
        let mut find = Find {
            query: "build".into(),
            ..Find::default()
        };
        find.search(&messages, &mut markdown);
        let parts: Vec<_> = find
            .hits
            .iter()
            .map(|hit| (hit.message, hit.block, hit.part))
            .collect();
        assert_eq!(
            parts,
            [
                (0, 0, Part::Text),
                (1, 1, Part::Text),
                (1, 1, Part::Text),
                (1, 2, Part::Title),
                (1, 2, Part::Input),
                (1, 2, Part::Output),
                (1, 2, Part::Diff(0)),
            ],
            "the link target in the Markdown reply does not count"
        );
        assert_eq!(find.counter(), "7 matches");
        // Outside code spans, egui_commonmark's ranges are source byte ranges.
        let source = "Run `cargo build`, then build, see [docs](https://build.example).";
        let marks = find.marks(1, 1, Part::Text).unwrap();
        assert_eq!(&source[marks.ranges[1].clone()], "build");
    }

    #[test]
    fn steps_wrap_and_mark_the_current_match() {
        let messages = transcript();
        let mut markdown = CommonMarkCache::default();
        let mut find = Find {
            query: "cargo FAILED".into(),
            ..Find::default()
        };
        find.search(&messages, &mut markdown);
        assert_eq!(find.hits.len(), 4);
        assert!(find.step(false));
        assert_eq!(find.counter(), "4 of 4");
        assert_eq!(find.marks(1, 2, Part::Output).unwrap().active, Some(0));
        assert!(find.step(true));
        assert_eq!(find.current().unwrap().part, Part::Text);
        find.activate(&messages, &mut markdown, true);
        let cache = markdown.search_cache_mut(&markdown_id(2, 1));
        assert_eq!(cache.active_match, Some(0));

        find.query = "nothing here".into();
        find.search(&messages, &mut markdown);
        assert_eq!(find.counter(), "No matches");
        assert!(!find.step(true));
        let cache = markdown.search_cache_mut(&markdown_id(2, 1));
        assert!(cache.search_ranges.is_empty() && cache.active_match.is_none());
    }
}
