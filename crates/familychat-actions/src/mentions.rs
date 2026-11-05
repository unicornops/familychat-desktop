//! Mention autocomplete (PLAN.md, section 5.8): what the person is typing
//! after an `@`, the room members that match it, and the text once one is
//! picked. Both frontends' composers use these, so they suggest the same
//! people in the same order.

use std::ops::Range;

use familychat_core::{
    fuzzy::fuzzy_match,
    messaging::Mention,
    rooms::{Membership, RoomMember},
};

/// The most people the suggestions list.
pub const MAX_SUGGESTIONS: usize = 6;

/// What is being typed after an `@`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionQuery {
    /// Where the `@` and the query are in the text (byte offsets).
    pub range: Range<usize>,
    /// What follows the `@`.
    pub query: String,
}

/// The mention being typed with the cursor at `cursor` (a byte offset):
/// an `@` at the start of the text or after a space, then up to the
/// cursor no spaces and at most 32 characters.
pub fn mention_query(text: &str, cursor: usize) -> Option<MentionQuery> {
    let before = text.get(..cursor)?;
    let at = before.rfind('@')?;
    let query = &before[at + 1..];
    if query.chars().any(char::is_whitespace) || query.chars().count() > 32 {
        return None;
    }
    let starts_word = before[..at]
        .chars()
        .next_back()
        .is_none_or(|ch| ch.is_whitespace() || matches!(ch, '(' | '"' | '\''));
    starts_word.then(|| MentionQuery {
        range: at..cursor,
        query: query.to_owned(),
    })
}

/// The joined and invited members (not the account itself) that match
/// `query`, best first: by their display name or their user id, with
/// [`fuzzy_match`]. An empty query lists them in the members' order.
pub fn suggestions<'a>(members: &'a [RoomMember], query: &str) -> Vec<&'a RoomMember> {
    let mut scored: Vec<(i32, usize, &RoomMember)> = members
        .iter()
        .enumerate()
        .filter(|(_, member)| {
            !member.is_own && matches!(member.membership, Membership::Joined | Membership::Invited)
        })
        .filter_map(|(index, member)| {
            if query.is_empty() {
                return Some((0, index, member));
            }
            let by_name = member
                .display_name
                .as_deref()
                .and_then(|name| fuzzy_match(query, name))
                .map(|found| found.score);
            let by_id = fuzzy_match(query, member.user_id.localpart()).map(|found| found.score);
            by_name.max(by_id).map(|score| (score, index, member))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, _, member)| member)
        .collect()
}

/// The text after picking `member` for `query`: the `@query` replaced by
/// their name and a space, the cursor after it, and the mention to send.
pub fn complete(text: &str, query: &MentionQuery, member: &RoomMember) -> (String, usize, Mention) {
    let label = member.name();
    let mut out = String::with_capacity(text.len() + label.len() + 1);
    out.push_str(&text[..query.range.start]);
    out.push_str(&label);
    let after = &text[query.range.end..];
    if !after.starts_with(' ') {
        out.push(' ');
    }
    let cursor = query.range.start + label.len() + 1;
    out.push_str(after);
    (
        out,
        cursor,
        Mention {
            user_id: member.user_id.clone(),
            label,
        },
    )
}

#[cfg(test)]
mod tests {
    use familychat_core::{OwnedUserId, rooms::MemberRole};

    use super::*;

    fn member(user: &str, name: Option<&str>, own: bool) -> RoomMember {
        RoomMember {
            user_id: OwnedUserId::try_from(user).expect("an id"),
            display_name: name.map(str::to_owned),
            membership: Membership::Joined,
            role: MemberRole::Member,
            is_own: own,
        }
    }

    fn members() -> Vec<RoomMember> {
        vec![
            member("@ana:smith.test", Some("Ana"), true),
            member("@mum:smith.test", Some("Mum"), false),
            member("@marcus:smith.test", Some("Marcus Smith"), false),
            member("@leo:smith.test", None, false),
        ]
    }

    #[test]
    fn a_query_starts_at_an_at_sign_that_starts_a_word() {
        assert_eq!(
            mention_query("hi @mu", 6),
            Some(MentionQuery {
                range: 3..6,
                query: "mu".to_owned()
            })
        );
        assert_eq!(
            mention_query("@", 1).map(|query| query.query),
            Some(String::new())
        );
        assert_eq!(mention_query("mail me at ana@smith", 20), None);
        assert_eq!(mention_query("hi @mum and", 11), None);
        assert_eq!(mention_query("hi @mum", 2), None);
    }

    #[test]
    fn suggestions_skip_the_account_and_rank_names() {
        let members = members();
        let names = |query: &str| -> Vec<String> {
            suggestions(&members, query)
                .into_iter()
                .map(RoomMember::name)
                .collect()
        };
        assert_eq!(names(""), ["Mum", "Marcus Smith", "@leo:smith.test"]);
        assert_eq!(names("mar"), ["Marcus Smith"]);
        assert_eq!(names("leo"), ["@leo:smith.test"]);
        assert!(names("ana").is_empty());
    }

    #[test]
    fn completing_replaces_the_query_and_records_the_mention() {
        let members = members();
        let query = mention_query("ask @ma about it", 7).expect("a query");
        let (text, cursor, mention) = complete("ask @ma about it", &query, &members[2]);
        assert_eq!(text, "ask Marcus Smith about it");
        assert_eq!(&text[..cursor], "ask Marcus Smith ");
        assert_eq!(mention.label, "Marcus Smith");
        let query = mention_query("@mu", 3).expect("a query");
        let (text, cursor, _) = complete("@mu", &query, &members[1]);
        assert_eq!((text.as_str(), cursor), ("Mum ", 4));
    }
}
