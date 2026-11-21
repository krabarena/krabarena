/// Test that all canonical about content is consistent and non-empty.
/// Both CLI and web UI consume these constants, so if they pass this
/// test, both surfaces are guaranteed to show the same content.
#[cfg(test)]
#[allow(clippy::const_is_empty)]
mod tests {
    use super::super::about;

    #[test]
    fn tagline_is_not_empty() {
        assert!(!about::TAGLINE.is_empty());
    }

    #[test]
    fn vocab_entries_are_present() {
        let words: Vec<&str> = about::VOCAB.iter().map(|v| v.word).collect();
        assert_eq!(
            words,
            vec![
                "Battle",
                "Claim",
                "Verify / Refute",
                "Comment",
                "Upvote / Downvote",
            ]
        );
    }

    #[test]
    fn vocab_entries_have_definitions_and_examples() {
        for v in about::VOCAB {
            assert!(!v.definition.is_empty(), "{} missing definition", v.word);
            assert!(!v.example.is_empty(), "{} missing example", v.word);
        }
    }

    #[test]
    fn motto_references_core_concepts() {
        let motto = about::MOTTO.to_lowercase();
        assert!(motto.contains("battle"), "Motto missing Battle");
        assert!(motto.contains("claim"), "Motto missing Claim");
        assert!(motto.contains("verified"), "Motto missing Verified");
        assert!(motto.contains("refuted"), "Motto missing Refuted");
    }

    #[test]
    fn mission_is_not_empty() {
        assert!(!about::MISSION.is_empty());
        for line in about::MISSION {
            assert!(!line.is_empty());
        }
    }

    #[test]
    fn faq_entries_have_questions_and_answers() {
        assert!(!about::FAQ.is_empty());
        for (q, a) in about::FAQ {
            assert!(!q.is_empty(), "FAQ has empty question");
            assert!(!a.is_empty(), "FAQ has empty answer");
            assert!(q.ends_with('?'), "FAQ question should end with ?: {}", q);
        }
    }

    #[test]
    fn sign_off_is_not_empty() {
        assert!(!about::SIGN_OFF.is_empty());
    }
}
