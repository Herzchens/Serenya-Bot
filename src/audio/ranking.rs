use std::time::Duration;

#[derive(Debug, Clone)]
pub struct TrackCandidate {
    pub source: String,
    pub title: String,
    pub artist: String,
    pub duration: Option<Duration>,
    pub popularity: Option<u64>,
    pub is_official: bool,
    pub is_topic_channel: bool,
    pub url: String,
    pub thumbnail: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct VariantRule {
    term: &'static str,
    penalty: f64,
    hard_reject_with_duration: bool,
}

const VARIANTS: &[VariantRule] = &[
    VariantRule {
        term: "remix",
        penalty: 0.30,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "bass boosted",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "bass-boosted",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "slowed",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "sped-up",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "sped up",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "nightcore",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "cover",
        penalty: 0.30,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "edit",
        penalty: 0.25,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "shorts",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "short",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "montage",
        penalty: 0.60,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "gameplay",
        penalty: 0.60,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "highlight",
        penalty: 0.55,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "highlights",
        penalty: 0.55,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "clip",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "clips",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "fragmovie",
        penalty: 0.60,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "teaser",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "preview",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "trailer",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "reaction",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "loop",
        penalty: 0.25,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "1 hour",
        penalty: 0.45,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "extended",
        penalty: 0.25,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "live",
        penalty: 0.25,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "karaoke",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "instrumental",
        penalty: 0.40,
        hard_reject_with_duration: false,
    },
];

fn variant_requested(query_lower: &str, term: &str) -> bool {
    query_lower.contains(term)
        || (term == "bass-boosted" && query_lower.contains("bass boosted"))
        || (term == "bass boosted" && query_lower.contains("bass-boosted"))
        || (term == "sped-up" && query_lower.contains("sped up"))
        || (term == "sped up" && query_lower.contains("sped-up"))
}

fn find_unrequested_variant(
    candidate_title_lower: &str,
    query_lower: &str,
) -> Option<&'static VariantRule> {
    VARIANTS.iter().find(|rule| {
        candidate_title_lower.contains(rule.term) && !variant_requested(query_lower, rule.term)
    })
}

pub fn contains_unrequested_variant(candidate_title: &str, query: &str) -> bool {
    find_unrequested_variant(&candidate_title.to_lowercase(), &query.to_lowercase()).is_some()
}

fn duration_tolerance_seconds(expected: Duration, strict: bool) -> f64 {
    let expected_secs = expected.as_secs_f64();
    if strict {
        (expected_secs * 0.50).max(80.0).min(120.0)
    } else {
        (expected_secs * 0.60).max(90.0).min(150.0)
    }
}

/// Helper to clean titles from common annotations like [Official Video], (Lyrics), etc.
pub fn clean_title(title: &str) -> String {
    let mut cleaned = String::new();
    let mut in_bracket: u32 = 0;
    let mut in_paren: u32 = 0;

    for c in title.chars() {
        match c {
            '[' => in_bracket += 1,
            ']' => in_bracket = in_bracket.saturating_sub(1),
            '(' => in_paren += 1,
            ')' => in_paren = in_paren.saturating_sub(1),
            _ => {
                if in_bracket == 0 && in_paren == 0 {
                    cleaned.push(c);
                }
            }
        }
    }

    // Normalize whitespace
    cleaned.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// Calculate Jaro-Winkler similarity between two strings (0.0 to 1.0)
pub fn jaro_winkler_similarity(s1: &str, s2: &str) -> f64 {
    let s1_chars: Vec<char> = s1
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    let s2_chars: Vec<char> = s2
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();

    let len1 = s1_chars.len();
    let len2 = s2_chars.len();

    if len1 == 0 && len2 == 0 {
        return 1.0;
    }
    if len1 == 0 || len2 == 0 {
        return 0.0;
    }

    let match_distance = (len1.max(len2) / 2).saturating_sub(1);

    let mut s1_matches = vec![false; len1];
    let mut s2_matches = vec![false; len2];

    let mut matches = 0.0;
    let mut transpositions = 0.0;

    for i in 0..len1 {
        let start = i.saturating_sub(match_distance);
        let end = (i + match_distance + 1).min(len2);

        for j in start..end {
            if !s2_matches[j] && s1_chars[i] == s2_chars[j] {
                s1_matches[i] = true;
                s2_matches[j] = true;
                matches += 1.0;
                break;
            }
        }
    }

    if matches == 0.0 {
        return 0.0;
    }

    let mut k = 0;
    for i in 0..len1 {
        if s1_matches[i] {
            while k < len2 && !s2_matches[k] {
                k += 1;
            }
            if k < len2 && s1_chars[i] != s2_chars[k] {
                transpositions += 1.0;
            }
            k += 1;
        }
    }

    let jaro = (matches / len1 as f64
        + matches / len2 as f64
        + (matches - transpositions / 2.0) / matches)
        / 3.0;

    // Winkler bonus for prefix matching
    let mut prefix_len = 0;
    for i in 0..4.min(len1).min(len2) {
        if s1_chars[i] == s2_chars[i] {
            prefix_len += 1;
        } else {
            break;
        }
    }

    let p = 0.1; // scaling factor
    jaro + prefix_len as f64 * p * (1.0 - jaro)
}

/// Score and rank candidates based on metadata relevance and quality signals.
/// Returns candidates sorted in descending order of score.
pub fn score_candidates(
    candidates: Vec<TrackCandidate>,
    query: &str,
    expected_title: &str,
    expected_artist: Option<&str>,
    expected_duration: Option<Duration>,
) -> Vec<(TrackCandidate, f64)> {
    let query_lower = query.to_lowercase();
    let clean_expected_title = clean_title(expected_title);
    let mut scored = Vec::new();

    for (rank_idx, candidate) in candidates.into_iter().enumerate() {
        let candidate_title_lower = candidate.title.to_lowercase();

        // 1. Duration Tolerance Guard (Hard Reject)
        if let Some(expected) = expected_duration {
            if let Some(candidate_dur) = candidate.duration {
                let diff = (expected.as_secs_f64() - candidate_dur.as_secs_f64()).abs();
                let tolerance = duration_tolerance_seconds(expected, expected_artist.is_some());
                if diff > tolerance {
                    tracing::info!(
                        "candidate_rejected reason=duration_mismatch expected={}s actual={}s",
                        expected.as_secs(),
                        candidate_dur.as_secs()
                    );
                    tracing::debug!(
                        candidate = %candidate.title,
                        candidate_duration = candidate_dur.as_secs(),
                        expected_duration = expected.as_secs(),
                        diff_s = %diff,
                        tolerance_s = %tolerance,
                        reject_reason = "duration_mismatch",
                        "rejecting search candidate"
                    );
                    continue; // Exclude candidate
                }
            }
        }

        if expected_duration.is_some() {
            if let Some(rule) = find_unrequested_variant(&candidate_title_lower, &query_lower) {
                if rule.hard_reject_with_duration {
                    tracing::info!(
                        "candidate_rejected reason=variant_conflict variant={}",
                        rule.term
                    );
                    tracing::debug!(
                        candidate = %candidate.title,
                        variant = rule.term,
                        reject_reason = "variant_conflict",
                        "rejecting search candidate"
                    );
                    continue;
                }
            }
        }

        // 2. Title Match Score
        let clean_cand_title = clean_title(&candidate.title);
        let title_similarity = jaro_winkler_similarity(&clean_expected_title, &clean_cand_title)
            .max(jaro_winkler_similarity(expected_title, &candidate.title));

        // 3. Artist Match Score (if expected artist exists)
        let artist_similarity = if let Some(expected_art) = expected_artist {
            let art_lower = expected_art.to_lowercase();
            let channel_lower = candidate.artist.to_lowercase();

            if channel_lower.contains(&art_lower) || art_lower.contains(&channel_lower) {
                1.0
            } else {
                jaro_winkler_similarity(expected_art, &candidate.artist)
            }
        } else {
            1.0
        };

        // 4. Duration Similarity Score (soft penalty near the tolerance boundary)
        let duration_similarity = if let Some(expected) = expected_duration {
            if let Some(candidate_dur) = candidate.duration {
                let diff = (expected.as_secs_f64() - candidate_dur.as_secs_f64()).abs();
                let diff_ratio = diff / expected.as_secs_f64();
                (1.0 - diff_ratio).max(0.0)
            } else {
                0.5 // Unknown candidate duration gets a medium score
            }
        } else {
            1.0
        };

        // Assemble base score
        let mut score = if expected_artist.is_some() {
            // Weight components: 50% title, 30% artist, 20% duration
            title_similarity * 0.5 + artist_similarity * 0.3 + duration_similarity * 0.2
        } else {
            // Weight components: 80% title, 20% duration
            title_similarity * 0.8 + duration_similarity * 0.2
        };

        // 5. Official / Topic channel boosts
        let is_vevo = candidate.artist.to_lowercase().contains("vevo");
        if candidate.is_official || candidate.is_topic_channel || is_vevo {
            score += 0.15;
        }

        // 5b. Lyric video boost (often contains original high-quality audio)
        let is_lyric = candidate_title_lower.contains("lyric") || candidate_title_lower.contains("lyrics");
        if is_lyric {
            score += 0.20;
        }

        // 6. Popularity boost (view count logarithmic scale)
        if let Some(views) = candidate.popularity {
            let view_log = (views as f64).ln().max(0.0);
            let view_score = (view_log / 18.0).min(1.0); // ln(65M) ≈ 18
            score += view_score * 0.05;
        } else {
            score += 0.02; // neutral boost if views not available
        }

        // 7. Search rank boost (earlier candidates are prioritized slightly)
        let rank_boost = match rank_idx {
            0 => 0.05,
            1 => 0.03,
            2 => 0.01,
            _ => 0.0,
        };
        score += rank_boost;

        // Clamp base score to 1.0 before applying penalties
        let mut final_score = score.min(1.0);

        // 8. Variant Penalties
        for rule in VARIANTS {
            if candidate_title_lower.contains(rule.term)
                && !variant_requested(&query_lower, rule.term)
            {
                final_score = (final_score - rule.penalty).max(0.0);
                tracing::debug!(
                    candidate = %candidate.title,
                    variant = %rule.term,
                    penalty = %rule.penalty,
                    reject_reason = "variant_penalty",
                    "penalized search candidate"
                );
            }
        }

        final_score = adjust_single_word_score_with_expected(
            &candidate.title,
            query,
            expected_title,
            final_score,
        );
        scored.push((candidate, final_score));
    }

    // Sort by score in descending order
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

pub fn adjust_single_word_score(candidate_title: &str, query: &str, base_score: f64) -> f64 {
    let query_clean = query.to_lowercase().trim().to_owned();
    if query_clean.contains(' ') || query_clean.is_empty() {
        return base_score;
    }

    let cand_title_clean = clean_title(candidate_title).to_lowercase();
    let cand_title_raw = candidate_title.to_lowercase();

    let is_exact = cand_title_clean == query_clean || cand_title_raw.trim() == query_clean;

    if is_exact {
        base_score.max(0.95)
    } else {
        let is_expanded = cand_title_clean.contains(' ') || cand_title_raw.contains(' ');
        let cap = if is_expanded { 0.70 } else { 0.80 };
        base_score.min(cap)
    }
}

pub fn adjust_single_word_score_with_expected(
    candidate_title: &str,
    query: &str,
    expected_title: &str,
    base_score: f64,
) -> f64 {
    let query_clean = query.to_lowercase().trim().to_owned();
    if query_clean.contains(' ') || query_clean.is_empty() {
        return base_score;
    }

    let cand_title_clean = clean_title(candidate_title).to_lowercase();
    let cand_title_raw = candidate_title.to_lowercase();
    let expected_clean = clean_title(expected_title).to_lowercase();

    let matches_expected =
        cand_title_clean == expected_clean || cand_title_raw.trim() == expected_clean;
    let matches_query = cand_title_clean == query_clean || cand_title_raw.trim() == query_clean;

    if matches_expected || matches_query {
        base_score.max(0.95)
    } else {
        let is_expanded = cand_title_clean.contains(' ') || cand_title_raw.contains(' ');
        let cap = if is_expanded { 0.70 } else { 0.80 };
        base_score.min(cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_title() {
        assert_eq!(clean_title("Idol (Official Video)"), "Idol");
        assert_eq!(clean_title("Idol [Lyrics]"), "Idol");
        assert_eq!(
            clean_title("Yoasobi - Idol (Lyric Video) [1080p]"),
            "Yoasobi - Idol"
        );
    }

    #[test]
    fn test_jaro_winkler() {
        let sim1 = jaro_winkler_similarity("Yoasobi", "yoasobi");
        assert!(sim1 > 0.99);

        let sim2 = jaro_winkler_similarity("Dynamite", "Dynamite - Remix");
        let sim3 = jaro_winkler_similarity("Dynamite", "Fire");
        assert!(sim2 > sim3);
    }

    #[test]
    fn test_duration_guard() {
        let candidate = TrackCandidate {
            source: "YouTube".to_owned(),
            title: "Yoasobi - Idol".to_owned(),
            artist: "Yoasobi".to_owned(),
            url: "https://youtube.com/watch?v=1".to_owned(),
            duration: Some(Duration::from_secs(204)), // 3:24
            popularity: Some(1000000),
            is_official: true,
            is_topic_channel: true,
            thumbnail: None,
        };

        // Within tolerance (10% of 200s is 20s, max(15, 20) = 20s. candidate is 204s, diff is 4s <= 20s)
        let scored_in = score_candidates(
            vec![candidate.clone()],
            "Yoasobi Idol",
            "Idol",
            Some("Yoasobi"),
            Some(Duration::from_secs(200)),
        );
        assert_eq!(scored_in.len(), 1);

        // Outside tolerance (10% of 100s is 10s, max(15, 10) = 15s. candidate is 204s, diff is 104s > 15s)
        let scored_out = score_candidates(
            vec![candidate],
            "Yoasobi Idol",
            "Idol",
            Some("Yoasobi"),
            Some(Duration::from_secs(100)),
        );
        assert_eq!(scored_out.len(), 0);
    }

    #[test]
    fn test_metadata_duration_guard_is_strict() {
        let candidate = TrackCandidate {
            source: "YouTube".to_owned(),
            title: "Zedd, VALORANT, Foxes, BUNT. - Clarity".to_owned(),
            artist: "Zedd".to_owned(),
            url: "https://youtube.com/watch?v=strict".to_owned(),
            duration: Some(Duration::from_secs(212)),
            popularity: Some(1000000),
            is_official: true,
            is_topic_channel: true,
            thumbnail: None,
        };

        let scored = score_candidates(
            vec![candidate],
            "Clarity Valorant",
            "Clarity",
            Some("Zedd, VALORANT, Foxes, BUNT."),
            Some(Duration::from_secs(204)),
        );
        assert_eq!(scored.len(), 1);
    }

    #[test]
    fn test_variant_penalties() {
        let candidate = TrackCandidate {
            source: "YouTube".to_owned(),
            title: "Yoasobi - Idol (Remix)".to_owned(),
            artist: "Yoasobi".to_owned(),
            url: "https://youtube.com/watch?v=1".to_owned(),
            duration: Some(Duration::from_secs(200)),
            popularity: Some(1000000),
            is_official: true,
            is_topic_channel: true,
            thumbnail: None,
        };

        // Query does not contain "remix", penalty should be applied
        let scored_penalized = score_candidates(
            vec![candidate.clone()],
            "Yoasobi Idol",
            "Idol",
            Some("Yoasobi"),
            Some(Duration::from_secs(200)),
        );
        let score_with_penalty = scored_penalized[0].1;

        // Query contains "remix", no penalty should be applied
        let scored_unpenalized = score_candidates(
            vec![candidate],
            "Yoasobi Idol Remix",
            "Idol",
            Some("Yoasobi"),
            Some(Duration::from_secs(200)),
        );
        let score_no_penalty = scored_unpenalized[0].1;

        assert!(score_with_penalty < score_no_penalty);
    }

    #[test]
    fn test_variant_conflict_detection() {
        assert!(contains_unrequested_variant(
            "Clarity (BUNT. Remix & Bass Boosted)",
            "Clarity Valorant"
        ));
        assert!(!contains_unrequested_variant(
            "Clarity (BUNT. Remix)",
            "Clarity Valorant remix"
        ));
    }

    #[test]
    fn test_clarity_valorant_prefers_catalog_duration_candidate() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Clarity Valorant Teaser".to_owned(),
                artist: "VALORANT".to_owned(),
                url: "https://youtube.com/watch?v=teaser".to_owned(),
                duration: Some(Duration::from_secs(29)),
                popularity: Some(4_000_000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Clarity Valorant Edit".to_owned(),
                artist: "Fan Upload".to_owned(),
                url: "https://youtube.com/watch?v=edit".to_owned(),
                duration: Some(Duration::from_secs(98)),
                popularity: Some(8_000_000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Clarity (BUNT. Remix & Bass Boosted)".to_owned(),
                artist: "Bass Channel".to_owned(),
                url: "https://youtube.com/watch?v=remix".to_owned(),
                duration: Some(Duration::from_secs(204)),
                popularity: Some(12_000_000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube Music".to_owned(),
                title: "Clarity".to_owned(),
                artist: "Zedd, VALORANT, Foxes, BUNT. - Topic".to_owned(),
                url: "https://youtube.com/watch?v=official".to_owned(),
                duration: Some(Duration::from_secs(204)),
                popularity: Some(5_000_000),
                is_official: true,
                is_topic_channel: true,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Clarity Valorant",
            "Clarity",
            Some("Zedd, VALORANT, Foxes, BUNT."),
            Some(Duration::from_secs(204)),
        );

        assert_eq!(scored[0].0.url, "https://youtube.com/watch?v=official");
        assert!(scored.iter().all(|(candidate, _)| {
            candidate.url != "https://youtube.com/watch?v=teaser"
                && candidate.url != "https://youtube.com/watch?v=edit"
        }));

        let remix_score = scored
            .iter()
            .find(|(candidate, _)| candidate.url == "https://youtube.com/watch?v=remix")
            .map(|(_, score)| *score)
            .unwrap_or(0.0);
        assert!(remix_score < scored[0].1);
    }

    #[test]
    fn test_rejects_youtube_montage_for_catalog_music() {
        let scored = score_candidates(
            vec![TrackCandidate {
                source: "YouTube Music".to_owned(),
                title: "Clarity Valorant montage short".to_owned(),
                artist: "Valorant Clips".to_owned(),
                url: "https://youtube.com/watch?v=montage".to_owned(),
                duration: Some(Duration::from_secs(204)),
                popularity: Some(20_000_000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            }],
            "Zedd, VALORANT, BUNT., Foxes - Clarity (BUNT. Remix)",
            "Clarity (BUNT. Remix)",
            Some("Zedd, VALORANT, BUNT., Foxes"),
            Some(Duration::from_secs(204)),
        );

        assert!(scored.is_empty());
    }

    #[test]
    fn test_single_word_query_ranking() {
        let candidates = vec![
            TrackCandidate {
                source: "Spotify".to_owned(),
                title: "Destory".to_owned(),
                artist: "Artist A".to_owned(),
                url: "https://spotify/destory".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(80),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Destroy".to_owned(),
                artist: "Artist B".to_owned(),
                url: "https://youtube/destroy".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(10000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Destroy Love OST".to_owned(),
                artist: "Artist C".to_owned(),
                url: "https://youtube/destroy_love".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(5000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Seek & Destroy".to_owned(),
                artist: "Metallica".to_owned(),
                url: "https://youtube/seek_and_destroy".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(999999),
                is_official: true,
                is_topic_channel: true,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Destory",
            "Destory",
            None,
            Some(Duration::from_secs(200)),
        );

        // Exact Destory wins
        assert_eq!(scored[0].0.title, "Destory");
        assert!(scored[0].1 >= 0.90);

        // No Destroy... gets auto-pick confidence
        for (cand, score) in scored.iter().skip(1) {
            assert!(
                *score < 0.90,
                "Candidate {} has confidence {} >= 0.90",
                cand.title,
                score
            );
        }
    }
}
