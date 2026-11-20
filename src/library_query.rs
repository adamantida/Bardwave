//! Composite library search: group + tags + free text.
//!
//! [`LibraryQuery`] extends the single-choice [`GroupFilter`] with a set of
//! required tags and a text search, all combined with AND:
//!
//! * `group`: exactly the old [`GroupFilter`] semantics (it is the default
//!   `All`, and `LibraryQuery::from(filter)` wraps an existing filter);
//! * `tags_all`: the sound must carry every listed tag;
//! * `text`: whitespace-separated terms; each term must occur (as a
//!   substring) in the sound's name, its file name or one of its tags.
//!
//! Comparison is case-insensitive for all of Unicode (`to_lowercase`), so
//! "ТАВЕРНА" finds "Таверна" and "ÉCHO" finds "écho". It does not strip
//! accents: "echo" does not find "écho".
//!
//! The module is pure (no I/O). [`Library::search`] and the counting helpers
//! prepare the query once and then scan the sounds, which is fine for the
//! tens of thousands of sounds a library can hold; call them when the query
//! changes, not on every frame, if the library is large.

use std::collections::HashMap;
use std::ops::Range;

use crate::gui::groups::{normalize_tag, GroupFilter, GroupId, Library, LibrarySound};

/// A combined filter over the library. See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryQuery {
    /// Group restriction (`All` = no restriction).
    pub group: GroupFilter,
    /// Tags the sound must all carry (compared after normalization, so
    /// `" Tavern "` means `tavern`). Blank entries are ignored.
    pub tags_all: Vec<String>,
    /// Free text; empty or whitespace-only = no restriction.
    pub text: String,
}

impl From<GroupFilter> for LibraryQuery {
    fn from(group: GroupFilter) -> Self {
        Self {
            group,
            ..Self::default()
        }
    }
}

impl LibraryQuery {
    /// The query that accepts every sound.
    pub fn new() -> Self {
        Self::default()
    }

    /// Restrict to a group filter.
    pub fn with_group(mut self, group: GroupFilter) -> Self {
        self.group = group;
        self
    }

    /// Require one more tag.
    pub fn with_tag(mut self, tag: &str) -> Self {
        self.tags_all.push(tag.to_string());
        self
    }

    /// Set the free text.
    pub fn with_text(mut self, text: &str) -> Self {
        self.text = text.to_string();
        self
    }

    /// True when the query accepts every sound (no group, tags or text).
    pub fn is_unrestricted(&self) -> bool {
        self.group == GroupFilter::All
            && self.tags_all.iter().all(|t| t.trim().is_empty())
            && self.text.trim().is_empty()
    }

    /// The group part as a plain [`GroupFilter`].
    pub fn group_filter(&self) -> GroupFilter {
        self.group
    }

    /// Lower-cased text terms, in the order typed (what the UI should
    /// highlight with [`highlight_ranges`]).
    pub fn text_terms(&self) -> Vec<String> {
        self.text
            .split_whitespace()
            .map(str::to_lowercase)
            .collect()
    }

    /// Does one sound pass the query? Convenience for single checks; searching
    /// many sounds should go through [`Library::search`], which prepares the
    /// query only once.
    pub fn matches(&self, sound: &LibrarySound) -> bool {
        Prepared::new(self).accepts(sound)
    }
}

/// A query with its tags and terms normalized once.
struct Prepared {
    group: GroupFilter,
    tags: Vec<String>,
    terms: Vec<String>,
}

impl Prepared {
    fn new(query: &LibraryQuery) -> Self {
        let mut tags: Vec<String> = Vec::new();
        for raw in &query.tags_all {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            // A tag too long to be stored can never match; keep it (lower-
            // cased) so it correctly rejects everything.
            let tag = normalize_tag(trimmed).unwrap_or_else(|| trimmed.to_lowercase());
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        Self {
            group: query.group,
            tags,
            terms: query.text_terms(),
        }
    }

    fn accepts(&self, sound: &LibrarySound) -> bool {
        self.accepts_group(sound) && self.accepts_rest(sound)
    }

    fn accepts_group(&self, sound: &LibrarySound) -> bool {
        match self.group {
            GroupFilter::All => true,
            GroupFilter::Ungrouped => sound.group.is_none(),
            GroupFilter::Group(id) => sound.group == Some(id),
        }
    }

    /// Tags and text (everything except the group).
    fn accepts_rest(&self, sound: &LibrarySound) -> bool {
        self.accepts_tags(sound) && self.accepts_text(sound)
    }

    fn accepts_tags(&self, sound: &LibrarySound) -> bool {
        self.tags.iter().all(|tag| sound.tags.contains(tag))
    }

    fn accepts_text(&self, sound: &LibrarySound) -> bool {
        if self.terms.is_empty() {
            return true;
        }
        let name = sound.name.to_lowercase();
        let file = sound
            .path
            .file_name()
            .map(|f| f.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        self.terms.iter().all(|term| {
            name.contains(term.as_str())
                || file.contains(term.as_str())
                || sound.tags.iter().any(|tag| tag.contains(term.as_str()))
        })
    }
}

/// Per-group sound counts under a query (see [`Library::group_counts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupCounts {
    /// Sounds that pass the tags and text, whatever their group.
    pub all: usize,
    /// ... of which belong to no group.
    pub ungrouped: usize,
    /// ... of which belong to each group, in library group order (groups
    /// with no match are present with 0).
    pub groups: Vec<(GroupId, usize)>,
}

impl GroupCounts {
    /// The count the UI shows next to a [`GroupFilter`] entry.
    pub fn for_filter(&self, filter: GroupFilter) -> usize {
        match filter {
            GroupFilter::All => self.all,
            GroupFilter::Ungrouped => self.ungrouped,
            GroupFilter::Group(id) => self
                .groups
                .iter()
                .find(|(group, _)| *group == id)
                .map_or(0, |(_, count)| *count),
        }
    }
}

impl Library {
    /// Sounds that pass `query`, in library order.
    pub fn search(&self, query: &LibraryQuery) -> Vec<&LibrarySound> {
        let prepared = Prepared::new(query);
        self.sounds()
            .iter()
            .filter(|sound| prepared.accepts(sound))
            .collect()
    }

    /// Number of sounds that pass `query`.
    pub fn count_matching(&self, query: &LibraryQuery) -> usize {
        let prepared = Prepared::new(query);
        self.sounds()
            .iter()
            .filter(|sound| prepared.accepts(sound))
            .count()
    }

    /// Does the sound registered at `path` pass `query`? An unregistered path
    /// passes nothing (the same rule as [`Library::matches`]).
    pub fn matches_query(&self, path: &std::path::Path, query: &LibraryQuery) -> bool {
        self.sound(path).is_some_and(|sound| query.matches(sound))
    }

    /// Counts for the group rail: how many sounds each group entry would show
    /// if it were selected, given the tags and text of `query` (its own `group`
    /// is ignored, so every entry gets a number at the same time).
    pub fn group_counts(&self, query: &LibraryQuery) -> GroupCounts {
        let prepared = Prepared::new(query);
        let mut per_group: HashMap<GroupId, usize> = HashMap::new();
        let mut counts = GroupCounts::default();
        for sound in self.sounds() {
            if !prepared.accepts_rest(sound) {
                continue;
            }
            counts.all += 1;
            match sound.group {
                Some(id) => *per_group.entry(id).or_insert(0) += 1,
                None => counts.ungrouped += 1,
            }
        }
        counts.groups = self
            .groups()
            .iter()
            .map(|group| (group.id, per_group.get(&group.id).copied().unwrap_or(0)))
            .collect();
        counts
    }

    /// Tags found on the sounds that pass `query`, with how many of those
    /// sounds carry each: the tags that can still narrow the result. Most used
    /// first, ties alphabetical. Tags already required by `query` are included
    /// (their count equals the result size).
    pub fn tag_counts(&self, query: &LibraryQuery) -> Vec<(String, usize)> {
        let prepared = Prepared::new(query);
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for sound in self.sounds() {
            if prepared.accepts(sound) {
                for tag in &sound.tags {
                    *counts.entry(tag.as_str()).or_insert(0) += 1;
                }
            }
        }
        let mut out: Vec<(String, usize)> = counts
            .into_iter()
            .map(|(tag, count)| (tag.to_string(), count))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// Byte ranges of `text` to highlight for the given lower-cased search terms
/// (as returned by [`LibraryQuery::text_terms`]).
///
/// The match is case-insensitive like the search itself. Because lower-casing
/// can change the length of a character ("İ" becomes two characters), a
/// match that covers part of such a character highlights the whole original
/// character. The ranges are sorted, non-overlapping, merged when they touch,
/// and always fall on `char` boundaries of `text`, so they can be used to
/// slice it.
pub fn highlight_ranges(text: &str, terms: &[String]) -> Vec<Range<usize>> {
    let terms: Vec<&str> = terms
        .iter()
        .map(String::as_str)
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() || text.is_empty() {
        return Vec::new();
    }

    // Lower-case char by char, remembering for every byte of the lowered text
    // which original character produced it.
    let mut lowered = String::with_capacity(text.len());
    let mut origin: Vec<(usize, usize)> = Vec::with_capacity(text.len());
    for (start, ch) in text.char_indices() {
        let end = start + ch.len_utf8();
        let before = lowered.len();
        lowered.extend(ch.to_lowercase());
        for _ in before..lowered.len() {
            origin.push((start, end));
        }
    }

    let mut ranges: Vec<Range<usize>> = Vec::new();
    for term in terms {
        for (at, matched) in lowered.match_indices(term) {
            let first = origin[at].0;
            let last = origin[at + matched.len() - 1].1;
            ranges.push(first..last);
        }
    }
    ranges.sort_by_key(|r| (r.start, r.end));

    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn library() -> (Library, GroupId) {
        let mut lib = Library::new();
        let tavern = lib.add_group("Таверна").unwrap();
        lib.insert_sound(
            Path::new("/s/Crowd Noise.ogg"),
            "Гул толпы",
            1.0,
            Some(tavern),
        );
        lib.insert_sound(Path::new("/s/fire.wav"), "Fireplace", 1.0, Some(tavern));
        lib.insert_sound(Path::new("/s/sword.flac"), "", 1.0, None);
        lib.set_tags(Path::new("/s/Crowd Noise.ogg"), &["Loud", "Indoor"]);
        lib.set_tags(Path::new("/s/fire.wav"), &["indoor", "warm"]);
        lib.set_tags(Path::new("/s/sword.flac"), &["loud", "combat"]);
        (lib, tavern)
    }

    #[test]
    fn default_query_accepts_everything() {
        let (lib, _) = library();
        let q = LibraryQuery::new();
        assert!(q.is_unrestricted());
        assert_eq!(lib.search(&q).len(), 3);
        assert_eq!(lib.count_matching(&q), 3);
    }

    #[test]
    fn group_filter_semantics_are_unchanged() {
        let (lib, tavern) = library();
        for filter in [
            GroupFilter::All,
            GroupFilter::Ungrouped,
            GroupFilter::Group(tavern),
        ] {
            let q = LibraryQuery::from(filter);
            assert_eq!(lib.count_matching(&q), lib.count_for(filter));
            assert_eq!(lib.search(&q).len(), lib.sounds_for(filter).len());
        }
    }

    #[test]
    fn tags_all_requires_every_tag() {
        let (lib, _) = library();
        let q = LibraryQuery::new().with_tag("LOUD").with_tag(" indoor ");
        let found = lib.search(&q);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Гул толпы");
        assert_eq!(lib.count_matching(&LibraryQuery::new().with_tag("loud")), 2);
        assert_eq!(
            lib.count_matching(&LibraryQuery::new().with_tag("missing")),
            0
        );
        // Blank tags are ignored.
        assert_eq!(lib.count_matching(&LibraryQuery::new().with_tag("  ")), 3);
    }

    #[test]
    fn text_is_unicode_case_insensitive_over_name_file_and_tags() {
        let (lib, _) = library();
        assert_eq!(lib.count_matching(&LibraryQuery::new().with_text("ГУЛ")), 1);
        assert_eq!(
            lib.count_matching(&LibraryQuery::new().with_text("fireP")),
            1
        );
        // File name, because the sword has no display name.
        assert_eq!(
            lib.count_matching(&LibraryQuery::new().with_text("SWORD")),
            1
        );
        // Tag text.
        assert_eq!(
            lib.count_matching(&LibraryQuery::new().with_text("warm")),
            1
        );
        // All terms must hit (possibly in different fields).
        assert_eq!(
            lib.count_matching(&LibraryQuery::new().with_text("толпы indoor")),
            1
        );
        assert_eq!(
            lib.count_matching(&LibraryQuery::new().with_text("толпы warm")),
            0
        );
    }

    #[test]
    fn everything_combines_with_and() {
        let (lib, tavern) = library();
        let q = LibraryQuery::new()
            .with_group(GroupFilter::Group(tavern))
            .with_tag("indoor")
            .with_text("fire");
        let found = lib.search(&q);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Fireplace");
        let q = q.with_group(GroupFilter::Ungrouped);
        assert!(lib.search(&q).is_empty());
    }

    #[test]
    fn matches_query_is_false_for_unknown_paths() {
        let (lib, _) = library();
        assert!(lib.matches_query(Path::new("/s/fire.wav"), &LibraryQuery::new()));
        assert!(!lib.matches_query(Path::new("/s/none.wav"), &LibraryQuery::new()));
    }

    #[test]
    fn group_counts_ignore_the_query_group() {
        let (lib, tavern) = library();
        let q = LibraryQuery::new()
            .with_group(GroupFilter::Ungrouped)
            .with_tag("loud");
        let counts = lib.group_counts(&q);
        assert_eq!(counts.all, 2);
        assert_eq!(counts.ungrouped, 1);
        assert_eq!(counts.for_filter(GroupFilter::Group(tavern)), 1);
        assert_eq!(counts.for_filter(GroupFilter::All), 2);
    }

    #[test]
    fn tag_counts_follow_the_query() {
        let (lib, _) = library();
        let all = lib.tag_counts(&LibraryQuery::new());
        assert_eq!(all[0], ("indoor".to_string(), 2));
        assert_eq!(all[1], ("loud".to_string(), 2));
        let narrowed = lib.tag_counts(&LibraryQuery::new().with_tag("combat"));
        assert_eq!(
            narrowed,
            vec![("combat".to_string(), 1), ("loud".to_string(), 1)]
        );
    }

    #[test]
    fn highlight_is_case_insensitive_and_merges() {
        let terms = vec!["гул".to_string(), "тол".to_string()];
        let text = "Гул толпы";
        let ranges = highlight_ranges(text, &terms);
        let parts: Vec<&str> = ranges.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(parts, vec!["Гул", "тол"]);

        let touching = highlight_ranges("abcd", &["ab".to_string(), "bc".to_string()]);
        assert_eq!(touching, vec![0..3]);
    }

    #[test]
    fn highlight_survives_length_changing_lowercase() {
        // 'İ' lower-cases to "i" + a combining dot (3 bytes in 2 chars).
        let text = "İstanbul";
        let ranges = highlight_ranges(text, &["stan".to_string()]);
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].clone()], "stan");
        let ranges = highlight_ranges(text, &["i".to_string()]);
        assert_eq!(&text[ranges[0].clone()], "İ");
    }

    #[test]
    fn highlight_without_terms_is_empty() {
        assert!(highlight_ranges("abc", &[]).is_empty());
        assert!(highlight_ranges("abc", &[String::new()]).is_empty());
        assert!(highlight_ranges("", &["a".to_string()]).is_empty());
    }
}
