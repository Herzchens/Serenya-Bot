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
    pub thumbnail: Option<std::sync::Arc<str>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MetadataConfidence {
    Trusted,     // Spotify, Deezer, Apple Music catalog
    SemiTrusted, // Database playlists, exact matches
    Untrusted,   // General search
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    OfficialAudio,
    TopicAudio,
    Audio,
    LyricVideo,
    CleanVersion,
    Visualizer,
    MusicVideo,
    LivePerformance,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RequestedVariantType {
    Remix,
    Cover,
    Live,
    Acoustic,
    Instrumental,
    Karaoke,
    SlowedReverb,
    SpedUp,
    Nightcore,
    CleanCensored,
    BassBoosted,
    Extended,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct ParsedQueryContext {
    pub core_title: String,
    pub artist_tokens: Vec<String>,
    pub remixer: Option<String>,
    pub requested_variant: Option<RequestedVariantType>,
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
        penalty: 0.20,
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
        penalty: 0.20,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "edit",
        penalty: 0.30,
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
        term: "behind the scenes",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "behind the scene",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "behind scene",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "making of",
        penalty: 0.50,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "making video",
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
        penalty: 0.90,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "clean",
        penalty: 0.80,
        hard_reject_with_duration: true,
    },
    VariantRule {
        term: "instrumental",
        penalty: 0.25,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "video",
        penalty: 0.15,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "visualizer",
        penalty: 0.12,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "mv",
        penalty: 0.15,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "vietsub",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "việt sub",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "lời việt",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "thuyết minh",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "phụ đề",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "video reaction",
        penalty: 0.45,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "official video",
        penalty: 0.25,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "official mv",
        penalty: 0.25,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "official music video",
        penalty: 0.25,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "reverb",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "lofi",
        penalty: 0.30,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "piano",
        penalty: 0.25,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "acapella",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
    VariantRule {
        term: "speed up",
        penalty: 0.35,
        hard_reject_with_duration: false,
    },
];

pub fn count_word(text: &str, word: &str) -> usize {
    let text_lower = normalize_string(text);
    let word_lower = normalize_string(word);

    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = text_lower[start..].find(&word_lower) {
        let actual_pos = start + pos;

        let char_before = if actual_pos > 0 {
            text_lower[..actual_pos].chars().next_back()
        } else {
            None
        };

        let char_after = text_lower[actual_pos + word_lower.len()..].chars().next();

        let is_boundary_before = match char_before {
            Some(c) => !c.is_alphanumeric(),
            None => true,
        };

        let is_boundary_after = match char_after {
            Some(c) => !c.is_alphanumeric(),
            None => true,
        };

        if is_boundary_before && is_boundary_after {
            count += 1;
        }
        start = actual_pos + word_lower.len();
    }
    count
}

pub fn contains_word(text: &str, word: &str) -> bool {
    count_word(text, word) > 0
}

fn strip_artist(text: &str, artist: Option<&str>) -> String {
    let mut result = text.to_lowercase();
    if let Some(a) = artist {
        let a_lower = a.to_lowercase();
        if !a_lower.is_empty() && result.contains(&a_lower) {
            result = result.replacen(&a_lower, " ", 1);
        }
    }
    result
}

fn variant_requested(query_lower: &str, expected_artist_lower: Option<&str>, term: &str) -> bool {
    let stripped = strip_artist(query_lower, expected_artist_lower);
    contains_word(&stripped, term)
        || (term == "bass-boosted" && contains_word(&stripped, "bass boosted"))
        || (term == "bass boosted" && contains_word(&stripped, "bass-boosted"))
        || (term == "sped-up" && contains_word(&stripped, "sped up"))
        || (term == "sped up" && contains_word(&stripped, "sped-up"))
}

fn find_unrequested_variant(
    candidate_title_lower: &str,
    candidate_artist_lower: &str,
    query_lower: &str,
    expected_title_lower: &str,
    expected_artist_lower: Option<&str>,
) -> Option<&'static VariantRule> {
    let mut cand_stripped = strip_artist(candidate_title_lower, Some(candidate_artist_lower));
    if let Some(expected_art) = expected_artist_lower {
        cand_stripped = strip_artist(&cand_stripped, Some(expected_art));
    }
    VARIANTS.iter().find(|rule| {
        let has_variant = contains_word(&cand_stripped, rule.term);
        let req_query = variant_requested(query_lower, expected_artist_lower, rule.term);
        let req_title = variant_requested(expected_title_lower, expected_artist_lower, rule.term);
        has_variant && !req_query && !req_title
    })
}

pub fn contains_unrequested_variant(candidate_title: &str, query: &str) -> bool {
    find_unrequested_variant(
        &candidate_title.to_lowercase(),
        "",
        &query.to_lowercase(),
        "",
        None,
    )
    .is_some()
}

pub fn normalize_string(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut normalized = String::with_capacity(lower.len());
    for c in lower.chars() {
        let folded = match c {
            'á' | 'à' | 'ả' | 'ã' | 'ạ' | 'ă' | 'ắ' | 'ằ' | 'ẳ' | 'ẵ' | 'ặ' | 'â' | 'ấ' | 'ầ'
            | 'ẩ' | 'ẫ' | 'ậ' => 'a',
            'é' | 'è' | 'ẻ' | 'ẽ' | 'ẹ' | 'ê' | 'ế' | 'ề' | 'ể' | 'ễ' | 'ệ' => {
                'e'
            }
            'í' | 'ì' | 'ỉ' | 'ĩ' | 'ị' => 'i',
            'ó' | 'ò' | 'ỏ' | 'õ' | 'ọ' | 'ô' | 'ố' | 'ồ' | 'ổ' | 'ỗ' | 'ộ' | 'ơ' | 'ớ' | 'ờ'
            | 'ở' | 'ỡ' | 'ợ' => 'o',
            'ú' | 'ù' | 'ủ' | 'ũ' | 'ụ' | 'ư' | 'ứ' | 'ừ' | 'ử' | 'ữ' | 'ự' => {
                'u'
            }
            'ý' | 'ỳ' | 'ỷ' | 'ỹ' | 'ỵ' => 'y',
            'đ' => 'd',
            _ => c,
        };
        // Normalize CJK full-width characters to ASCII equivalents
        let u = folded as u32;
        let final_char = if (0xFF01..=0xFF5E).contains(&u) {
            char::from_u32(u - 0xFEE0).unwrap_or(folded)
        } else if u == 0x3000 {
            ' '
        } else {
            folded
        };
        normalized.push(final_char);
    }
    normalized
}

pub fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32,
            0x4E00..=0x9FFF |
            0x3400..=0x4DBF |
            0x3040..=0x309F |
            0x30A0..=0x30FF |
            0xAC00..=0xD7AF |
            0xF900..=0xFAFF |
            0xFF00..=0xFFEF
        )
    })
}

pub fn extract_core_and_tags(title: &str) -> (String, Vec<String>) {
    let mut core_title = String::new();
    let mut tags = Vec::new();
    let mut current_tag = String::new();
    let mut depth: u32 = 0;
    for c in title.chars() {
        if c == '(' || c == '[' {
            depth += 1;
            if depth == 1 && !current_tag.is_empty() {
                let tag = current_tag.trim().to_lowercase();
                if !tag.is_empty() {
                    tags.push(tag);
                }
                current_tag.clear();
            }
        } else if c == ')' || c == ']' {
            depth = depth.saturating_sub(1);
            if depth == 0 && !current_tag.is_empty() {
                let tag = current_tag.trim().to_lowercase();
                if !tag.is_empty() {
                    tags.push(tag);
                }
                current_tag.clear();
            }
        } else {
            if depth > 0 {
                current_tag.push(c);
            } else {
                core_title.push(c);
            }
        }
    }
    if !current_tag.is_empty() {
        let tag = current_tag.trim().to_lowercase();
        if !tag.is_empty() {
            tags.push(tag);
        }
    }
    let core_title = core_title.split_whitespace().collect::<Vec<_>>().join(" ");
    (core_title, tags)
}

pub fn clean_title(title: &str) -> String {
    extract_core_and_tags(title).0
}

fn get_tokens(s: &str) -> Vec<String> {
    normalize_string(s)
        .split(|c: char| !c.is_alphanumeric())
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

pub fn jaro_winkler_similarity(s1: &str, s2: &str) -> f64 {
    if s1.is_empty() && s2.is_empty() {
        return 1.0;
    }
    if s1.is_empty() || s2.is_empty() {
        return 0.0;
    }
    strsim::jaro_winkler(&normalize_string(s1), &normalize_string(s2))
}

pub fn extract_remixer(
    text: &str,
    expected_title: &str,
    expected_artist: Option<&str>,
) -> Option<String> {
    let text_lower = text.to_lowercase();
    let artist_lower = expected_artist.map(|a| a.to_lowercase());
    let exp_title_lower = expected_title.to_lowercase();

    // Pattern 1: search inside brackets/tags
    let (_, tags) = extract_core_and_tags(text);
    for tag in tags {
        if tag.contains("remix") {
            let mut remixer = tag.replace("remix", "");
            if let Some(ref art) = artist_lower {
                remixer = remixer.replace(art, "");
            }
            remixer = remixer.replace(&exp_title_lower, "");

            let cleaned = remixer.trim().to_owned();
            if !cleaned.is_empty() && cleaned.split_whitespace().count() <= 4 {
                return Some(cleaned);
            }
        }
    }

    // Pattern 2: inline, e.g. "Song Da Tweekaz Remix"
    if contains_word(&text_lower, "remix") {
        let mut inline_part = text_lower;
        // Strip expected title
        if !exp_title_lower.is_empty() && inline_part.contains(&exp_title_lower) {
            inline_part = inline_part.replace(&exp_title_lower, "");
        }
        // Strip expected artist
        if let Some(ref art) = artist_lower
            && !art.is_empty()
            && inline_part.contains(art)
        {
            inline_part = inline_part.replace(art, "");
        }
        inline_part = inline_part.replace("remix", "");

        // Clean symbols
        let cleaned_inline: String = inline_part
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c.is_whitespace() {
                    c
                } else {
                    ' '
                }
            })
            .collect();

        let remixer = cleaned_inline.trim().to_owned();
        if !remixer.is_empty() && remixer.split_whitespace().count() <= 4 {
            return Some(remixer);
        }
    }
    None
}

pub fn parse_query_context(
    query: &str,
    expected_title: &str,
    expected_artist: Option<&str>,
) -> ParsedQueryContext {
    let query_lower = query.to_lowercase();
    let expected_title_lower = expected_title.to_lowercase();
    let artist_lower = expected_artist.map(|a| a.to_lowercase());

    // 1. extract remixer
    let remixer = extract_remixer(query, expected_title, expected_artist)
        .or_else(|| extract_remixer(expected_title, expected_title, expected_artist));

    // 2. requested_variant
    let mut requested_variant = None;

    // Check in query or expected_title (stripped of artist name to prevent "Clean Bandit" issues)
    let check_text = format!("{} {}", query_lower, expected_title_lower);
    let check_text = if let Some(ref art) = artist_lower {
        if !art.is_empty() && check_text.contains(art) {
            check_text.replace(art, "")
        } else {
            check_text
        }
    } else {
        check_text
    };

    if contains_word(&check_text, "remix") {
        requested_variant = Some(RequestedVariantType::Remix);
    } else if contains_word(&check_text, "cover") || contains_word(&check_text, "piano") {
        requested_variant = Some(RequestedVariantType::Cover);
    } else if contains_word(&check_text, "live")
        || contains_word(&check_text, "concert")
        || contains_word(&check_text, "performance")
    {
        requested_variant = Some(RequestedVariantType::Live);
    } else if contains_word(&check_text, "acoustic") {
        requested_variant = Some(RequestedVariantType::Acoustic);
    } else if contains_word(&check_text, "instrumental") {
        requested_variant = Some(RequestedVariantType::Instrumental);
    } else if contains_word(&check_text, "karaoke") {
        requested_variant = Some(RequestedVariantType::Karaoke);
    } else if contains_word(&check_text, "slowed")
        || check_text.contains("slowed+reverb")
        || check_text.contains("slowed + reverb")
    {
        requested_variant = Some(RequestedVariantType::SlowedReverb);
    } else if contains_word(&check_text, "sped up")
        || contains_word(&check_text, "sped-up")
        || contains_word(&check_text, "speed up")
    {
        requested_variant = Some(RequestedVariantType::SpedUp);
    } else if contains_word(&check_text, "nightcore") {
        requested_variant = Some(RequestedVariantType::Nightcore);
    } else if contains_word(&check_text, "clean") {
        requested_variant = Some(RequestedVariantType::CleanCensored);
    } else if contains_word(&check_text, "extended") {
        requested_variant = Some(RequestedVariantType::Extended);
    }

    let core_title = clean_title(expected_title);
    let artist_tokens = expected_artist.map(get_tokens).unwrap_or_default();

    ParsedQueryContext {
        core_title,
        artist_tokens,
        remixer,
        requested_variant,
    }
}

fn is_clean_variant_in_title(title: &str, artist: &str) -> bool {
    let artist_lower = artist.to_lowercase();
    if artist_lower.split_whitespace().any(|w| w == "clean") {
        return false;
    }
    let title_lower = title.to_lowercase();
    title_lower.contains("(clean")
        || title_lower.contains("[clean")
        || title_lower.contains("clean version")
        || title_lower.contains("clean edit")
        || title_lower.contains("clean lyric")
}

fn classify_media_type_v2(title: &str, artist: &str, is_topic: bool) -> MediaType {
    let title_lower = title.to_lowercase();

    if contains_word(&title_lower, "live")
        || contains_word(&title_lower, "concert")
        || contains_word(&title_lower, "performance")
        || contains_word(&title_lower, "tour")
    {
        return MediaType::LivePerformance;
    }

    if title_lower.contains("official audio") {
        return MediaType::OfficialAudio;
    }

    let has_video_indicators = contains_word(&title_lower, "video")
        || contains_word(&title_lower, "mv")
        || contains_word(&title_lower, "visualizer")
        || contains_word(&title_lower, "lyric")
        || contains_word(&title_lower, "lyrics");

    if is_topic && !has_video_indicators {
        return MediaType::TopicAudio;
    }

    if contains_word(&title_lower, "audio") {
        return MediaType::Audio;
    }

    let is_clean = is_clean_variant_in_title(title, artist);

    if contains_word(&title_lower, "lyric") || contains_word(&title_lower, "lyrics") {
        if is_clean {
            return MediaType::CleanVersion;
        }
        return MediaType::LyricVideo;
    }

    if is_clean {
        return MediaType::CleanVersion;
    }

    if contains_word(&title_lower, "visualizer") {
        return MediaType::Visualizer;
    }

    if title_lower.contains("official video")
        || title_lower.contains("music video")
        || contains_word(&title_lower, "mv")
        || title_lower.contains("official music video")
        || contains_word(&title_lower, "video")
    {
        return MediaType::MusicVideo;
    }

    MediaType::Unknown
}

fn score_remixer_alignment(candidate: &TrackCandidate, ctx: &ParsedQueryContext) -> f64 {
    let Some(ref remixer) = ctx.remixer else {
        return 0.0;
    };

    let remixer_norm = normalize_string(remixer);
    let cand_title_norm = normalize_string(&candidate.title);
    let cand_artist_norm = normalize_string(&candidate.artist);

    let found = cand_title_norm.contains(&remixer_norm) || cand_artist_norm.contains(&remixer_norm);

    if found { 0.15 } else { -0.80 }
}

fn mv_intro_outro_penalty(expected: Duration, candidate: Duration, media_type: MediaType) -> f64 {
    if !matches!(media_type, MediaType::MusicVideo) {
        return 0.0;
    }

    let delta = candidate.as_secs_f64() - expected.as_secs_f64();
    if delta <= 12.0 {
        return 0.0;
    }

    ((delta - 12.0) / 60.0).min(0.10)
}

pub fn has_critical_risks(
    candidate: &TrackCandidate,
    query: &str,
    expected_title: &str,
    expected_artist: Option<&str>,
    confidence: MetadataConfidence,
) -> bool {
    let query_lower = query.to_lowercase();
    let candidate_title_lower = candidate.title.to_lowercase();
    let candidate_artist_lower = candidate.artist.to_lowercase();
    let expected_title_lower = expected_title.to_lowercase();
    let expected_artist_lower = expected_artist.map(|a| a.to_lowercase());

    if find_unrequested_variant(
        &candidate_title_lower,
        &candidate_artist_lower,
        &query_lower,
        &expected_title_lower,
        expected_artist_lower.as_deref(),
    )
    .is_some()
    {
        return true;
    }

    let is_montage_or_shorts = [
        "montage",
        "trailer",
        "shorts",
        "gameplay",
        "fragmovie",
        "teaser",
        "preview",
        "behind the scenes",
        "behind the scene",
        "behind scene",
        "making of",
        "making video",
        "kill montage",
        "funny moments",
        "best moments",
        "ranked montage",
        "cinematic",
        "fan made",
        "fan-made",
    ]
    .iter()
    .any(|term| {
        contains_word(&candidate_title_lower, term)
            && !contains_word(&query_lower, term)
            && !contains_word(&expected_title_lower, term)
    });
    if is_montage_or_shorts {
        return true;
    }

    if query_lower.split_whitespace().count() == 1 && confidence == MetadataConfidence::Untrusted {
        return true;
    }

    false
}

pub fn score_candidates(
    candidates: Vec<TrackCandidate>,
    query: &str,
    expected_title: &str,
    expected_artist: Option<&str>,
    expected_duration: Option<Duration>,
    confidence: MetadataConfidence,
) -> Vec<(TrackCandidate, f64)> {
    let query_lower = query.to_lowercase();
    let expected_title_lower = expected_title.to_lowercase();
    let clean_expected_title_lower = clean_title(expected_title).to_lowercase();
    let expected_artist_lower = expected_artist.map(|a| a.to_lowercase());
    let mut scored = Vec::new();

    // Stage 0: ParsedQueryContext
    let ctx = parse_query_context(query, expected_title, expected_artist);

    // Parse query for artist-title split (Phase 1: " - " only)
    let parsed_query = if query.contains(" - ") {
        let parts: Vec<&str> = query.split(" - ").collect();
        if parts.len() >= 2 {
            Some((
                parts[0].trim().to_lowercase(),
                parts[1].trim().to_lowercase(),
            ))
        } else {
            None
        }
    } else {
        None
    };

    for (rank_idx, candidate) in candidates.into_iter().enumerate() {
        let candidate_title_lower = candidate.title.to_lowercase();
        let candidate_artist_lower = candidate.artist.to_lowercase();

        // Stage 1: Pre-Filter (Hard Rejects)
        if let Some(expected) = expected_duration
            && let Some(candidate_dur) = candidate.duration
        {
            let diff = (expected.as_secs_f64() - candidate_dur.as_secs_f64()).abs();
            let candidate_title_norm = normalize_string(&candidate_title_lower);

            // 1.3 Loop gate: candidate_duration > 2.5 × expected_duration
            if candidate_dur.as_secs_f64() > 2.5 * expected.as_secs_f64() {
                tracing::info!(
                    "candidate_rejected reason=loop_gate expected={}s actual={}s",
                    expected.as_secs(),
                    candidate_dur.as_secs()
                );
                continue;
            }

            // 1.2 Shorts gate: candidate_duration < 65s AND expected_duration > 120s
            if candidate_dur.as_secs() < 65 && expected.as_secs() > 120 {
                tracing::info!(
                    "candidate_rejected reason=shorts_gate expected={}s actual={}s",
                    expected.as_secs(),
                    candidate_dur.as_secs()
                );
                continue;
            }

            let is_relaxed = ["live", "mix", "extended", "dj", "concert"]
                .iter()
                .any(|term| {
                    contains_word(&query_lower, term) || contains_word(&expected_title_lower, term)
                })
                || contains_word(&candidate_title_norm, "mv")
                || contains_word(&candidate_title_norm, "m/v")
                || candidate_title_norm.contains("official video")
                || candidate_title_norm.contains("music video")
                || candidate_title_norm.contains("video ca nhac")
                || candidate_title_norm.contains("phim ca nhac");

            let tolerance = if is_relaxed {
                (expected.as_secs_f64() * 0.35).max(90.0)
            } else {
                match confidence {
                    MetadataConfidence::Trusted => (expected.as_secs_f64() * 0.10).max(15.0),
                    MetadataConfidence::SemiTrusted => (expected.as_secs_f64() * 0.15).max(25.0),
                    MetadataConfidence::Untrusted => (expected.as_secs_f64() * 0.20).max(35.0),
                }
            };

            if diff > tolerance {
                tracing::info!(
                    "candidate_rejected reason=duration_mismatch expected={}s actual={}s diff={}s tolerance={}s",
                    expected.as_secs(),
                    candidate_dur.as_secs(),
                    diff,
                    tolerance
                );
                continue;
            }
        }

        // Expanded hard reject keywords gate
        let is_montage_or_shorts = [
            "montage",
            "trailer",
            "shorts",
            "gameplay",
            "fragmovie",
            "teaser",
            "preview",
            "behind the scenes",
            "behind the scene",
            "behind scene",
            "making of",
            "making video",
            "kill montage",
            "funny moments",
            "best moments",
            "ranked montage",
            "cinematic",
            "fan made",
            "fan-made",
        ]
        .iter()
        .any(|term| {
            contains_word(&candidate_title_lower, term)
                && !contains_word(&query_lower, term)
                && !contains_word(&expected_title_lower, term)
        });
        if is_montage_or_shorts {
            tracing::info!(
                "candidate_rejected reason=montage_or_shorts_gate title={}",
                candidate.title
            );
            continue;
        }

        if let Some(rule) = find_unrequested_variant(
            &candidate_title_lower,
            &candidate_artist_lower,
            &query_lower,
            &expected_title_lower,
            expected_artist_lower.as_deref(),
        ) && rule.hard_reject_with_duration
        {
            tracing::info!(
                "candidate_rejected reason=variant_conflict variant={}",
                rule.term
            );
            continue;
        }

        // Stage 3: Identity Scoring
        let clean_cand_title_lower = clean_title(&candidate.title).to_lowercase();
        let is_cjk = contains_cjk(expected_title) || contains_cjk(&candidate.title);

        let mut title_similarity = if is_cjk {
            // Normalized substring match for CJK
            let clean_exp = clean_expected_title_lower.replace(' ', "");
            let clean_cand = clean_cand_title_lower.replace(' ', "");
            if clean_cand.contains(&clean_exp) || clean_exp.contains(&clean_cand) {
                0.95f64.max(strsim::jaro_winkler(&clean_exp, &clean_cand))
            } else {
                strsim::jaro_winkler(&clean_exp, &clean_cand)
            }
        } else {
            let mut sim =
                strsim::jaro_winkler(&clean_expected_title_lower, &clean_cand_title_lower).max(
                    strsim::jaro_winkler(&expected_title_lower, &candidate_title_lower),
                );
            if let Some((_, ref title_part)) = parsed_query {
                sim = sim.max(strsim::jaro_winkler(title_part, &clean_cand_title_lower));
            }
            sim
        };

        // Substring / exact containment boost: only if ≥ 2 tokens
        let expected_tokens = get_tokens(expected_title);
        let contains_exact = clean_cand_title_lower.contains(&clean_expected_title_lower)
            || candidate_title_lower.contains(&expected_title_lower);
        if contains_exact && expected_tokens.len() >= 2 {
            title_similarity = title_similarity.max(0.95);
        }

        let mut artist_similarity = 1.0;
        if let Some(ref art_lower) = expected_artist_lower {
            let exp_art_tokens = get_tokens(art_lower);
            let cand_art_tokens = get_tokens(&candidate.artist);
            let overlap_count = cand_art_tokens
                .iter()
                .filter(|t| exp_art_tokens.contains(t))
                .count();

            // Check for remixer channel matches: if candidate artist matches a query/expected title token not in original artist
            let query_tokens = get_tokens(query);
            let expected_title_tokens = get_tokens(expected_title);
            let is_remixer_match = cand_art_tokens.iter().any(|t| {
                (query_tokens.contains(t) || expected_title_tokens.contains(t))
                    && !exp_art_tokens.contains(t)
            });

            artist_similarity = if is_remixer_match || exp_art_tokens.is_empty() {
                1.0
            } else {
                let overlap_ratio = overlap_count as f64 / exp_art_tokens.len() as f64;
                overlap_ratio.max(strsim::jaro_winkler(
                    art_lower.as_str(),
                    &candidate_artist_lower,
                ))
            };
        } else if let Some((ref artist_part, _)) = parsed_query {
            artist_similarity = strsim::jaro_winkler(artist_part, &candidate_artist_lower);
        }

        let duration_similarity = if let Some(expected) = expected_duration {
            if let Some(candidate_dur) = candidate.duration {
                let diff = (expected.as_secs_f64() - candidate_dur.as_secs_f64()).abs();
                let diff_ratio = diff / expected.as_secs_f64();
                (1.0 - diff_ratio).max(0.0)
            } else {
                0.5
            }
        } else {
            1.0
        };

        let mut score = if expected_artist.is_some() {
            title_similarity * 0.5 + artist_similarity * 0.3 + duration_similarity * 0.2
        } else {
            title_similarity * 0.8 + duration_similarity * 0.2
        };

        // 3.3 Remixer alignment (identity check)
        let remixer_align = score_remixer_alignment(&candidate, &ctx);
        score += remixer_align;

        // Stage 4: Quality Scoring
        if !is_cjk {
            let exp_tokens_set: std::collections::HashSet<String> =
                expected_tokens.into_iter().collect();
            let exp_artist_tokens = expected_artist.map(get_tokens).unwrap_or_default();
            let artist_tokens_set: std::collections::HashSet<String> =
                exp_artist_tokens.into_iter().collect();

            let mut extra_tokens = 0;
            for token in get_tokens(&candidate.title) {
                if !exp_tokens_set.contains(&token) && !artist_tokens_set.contains(&token) {
                    let is_negligible = matches!(
                        token.as_str(),
                        "the"
                            | "a"
                            | "an"
                            | "of"
                            | "and"
                            | "or"
                            | "in"
                            | "by"
                            | "to"
                            | "official"
                            | "audio"
                            | "video"
                            | "mv"
                            | "lyrics"
                            | "lyric"
                            | "topic"
                            | "music"
                    );
                    if !is_negligible {
                        extra_tokens += 1;
                    }
                }
            }
            let extra_token_penalty = (extra_tokens as f64 * 0.04).min(0.20);
            score = (score - extra_token_penalty).max(0.0);
        }

        // Media type classification (Stage 2) and adjustments (Stage 4)
        let media_type = classify_media_type_v2(
            &candidate.title,
            &candidate.artist,
            candidate.is_topic_channel,
        );
        let pass_gates =
            title_similarity >= 0.70 && (expected_artist.is_none() || artist_similarity >= 0.50);
        if pass_gates && confidence == MetadataConfidence::Trusted {
            let media_type_boost = match media_type {
                MediaType::OfficialAudio => 0.18,
                MediaType::TopicAudio => 0.20,
                MediaType::Audio => 0.12,
                MediaType::CleanVersion => -0.05,
                MediaType::LyricVideo => 0.10,
                MediaType::Visualizer => -0.03,
                MediaType::MusicVideo => {
                    let base = -0.15;
                    if let Some(expected) = expected_duration
                        && let Some(candidate_dur) = candidate.duration
                    {
                        base - mv_intro_outro_penalty(expected, candidate_dur, media_type)
                    } else {
                        base
                    }
                }
                MediaType::LivePerformance => -0.25,
                MediaType::Unknown => 0.0,
            };
            score += media_type_boost * (title_similarity * artist_similarity);
        }

        let is_chu_de_channel = candidate_artist_lower.ends_with(" - chủ đề")
            || candidate_artist_lower.ends_with("- chủ đề")
            || candidate_artist_lower.ends_with(" - chu de")
            || candidate_artist_lower.ends_with("- chu de")
            || candidate_title_lower.contains("chủ đề")
            || candidate_title_lower.contains("chu de");

        if is_chu_de_channel {
            let chu_de_requested = query_lower.contains("chủ đề")
                || query_lower.contains("chu de")
                || expected_title_lower.contains("chủ đề")
                || expected_title_lower.contains("chu de");
            if !chu_de_requested {
                score -= 0.35 * (title_similarity * artist_similarity);
            }
        }

        let is_vevo = candidate_artist_lower.contains("vevo");
        let validity_scale = title_similarity * artist_similarity;
        if pass_gates && confidence == MetadataConfidence::Trusted {
            if candidate.is_official || is_vevo {
                score += 0.02 * validity_scale;
            }
            if let Some(pop) = candidate.popularity {
                let popularity_bonus = ((pop as f64 + 1.0).ln() / (100_000_000.0f64).ln()).min(1.0);
                score += popularity_bonus * 0.01 * validity_scale;
            }
            if rank_idx == 0 {
                score += 0.01 * validity_scale;
            }
            if title_similarity >= 0.98 {
                score += 0.10 * validity_scale;
            }
        }

        // Stage 5: Variant Alignment
        let mut cand_stripped = strip_artist(&candidate_title_lower, Some(&candidate_artist_lower));
        if let Some(expected_art) = expected_artist_lower.as_deref() {
            cand_stripped = strip_artist(&cand_stripped, Some(expected_art));
        }
        for rule in VARIANTS {
            let candidate_has_variant = contains_word(&cand_stripped, rule.term);
            let is_requested =
                variant_requested(&query_lower, expected_artist_lower.as_deref(), rule.term)
                    || variant_requested(
                        &expected_title_lower,
                        expected_artist_lower.as_deref(),
                        rule.term,
                    );

            if is_requested && candidate_has_variant {
                score += rule.penalty * 0.5;
            }
        }

        let mut final_score = score;

        for rule in VARIANTS {
            // Skip remix variant penalty if we have remixer alignment
            if rule.term == "remix" && ctx.remixer.is_some() {
                continue;
            }

            let candidate_has_variant = contains_word(&cand_stripped, rule.term);
            let is_requested =
                variant_requested(&query_lower, expected_artist_lower.as_deref(), rule.term)
                    || variant_requested(
                        &expected_title_lower,
                        expected_artist_lower.as_deref(),
                        rule.term,
                    );

            if is_requested {
                if !candidate_has_variant {
                    final_score = (final_score - rule.penalty * 1.5).max(0.0);
                }
            } else if candidate_has_variant {
                final_score = (final_score - rule.penalty).max(0.0);
            }
        }

        let mut critical_reasons = Vec::new();
        if find_unrequested_variant(
            &candidate_title_lower,
            &candidate_artist_lower,
            &query_lower,
            &expected_title_lower,
            expected_artist_lower.as_deref(),
        )
        .is_some()
        {
            critical_reasons.push("variant_conflict".to_owned());
        }
        if is_montage_or_shorts {
            critical_reasons.push("montage_or_shorts_mismatch".to_owned());
            final_score = (final_score - 0.40).max(0.0);
        }
        if query_lower.split_whitespace().count() == 1
            && confidence == MetadataConfidence::Untrusted
        {
            critical_reasons.push("single_word_untrusted".to_owned());
        }

        let mut warning_reasons = Vec::new();
        if expected_artist.is_some()
            && (candidate.artist.is_empty() || candidate.artist == "Unknown Artist")
        {
            warning_reasons.push("artist_missing".to_owned());
            final_score = (final_score - 0.05).max(0.0);
        }
        if expected_duration.is_some() && candidate.duration.is_none() {
            warning_reasons.push("duration_unknown".to_owned());
            final_score = (final_score - 0.05).max(0.0);
        }
        if let Some(expected) = expected_duration
            && let Some(candidate_dur) = candidate.duration
        {
            let diff = (expected.as_secs_f64() - candidate_dur.as_secs_f64()).abs();
            if diff > 10.0 {
                warning_reasons.push("minor_duration_mismatch".to_owned());
                final_score = (final_score - 0.02).max(0.0);
            }
        }

        final_score = adjust_single_word_score_with_expected(
            &candidate.title,
            query,
            expected_title,
            final_score,
            confidence,
        );

        scored.push((candidate, final_score.clamp(0.0, 1.0)));
    }

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
        base_score.max(0.90)
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
    confidence: MetadataConfidence,
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
        if confidence == MetadataConfidence::Untrusted {
            base_score.min(0.85)
        } else {
            base_score.max(0.90)
        }
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
    }

    #[test]
    fn test_regression_destory() {
        let candidates = vec![
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
                title: "Destroy Lonely - VETERAN".to_owned(),
                artist: "Destroy Lonely".to_owned(),
                url: "https://youtube/veteran".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(10000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
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
        ];

        let scored = score_candidates(
            candidates,
            "Destory",
            "Destory",
            None,
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        assert_eq!(scored[0].0.title, "Destory");
        assert!(scored[0].1 >= 0.90);
        assert!(scored[1].1 < 0.80);
    }

    #[test]
    fn test_regression_clarity_valorant() {
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
            MetadataConfidence::Trusted,
        );

        assert_eq!(scored[0].0.url, "https://youtube.com/watch?v=official");
    }

    #[test]
    fn test_regression_alive_vs_live() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Alive (Live Performance)".to_owned(),
                artist: "Artist".to_owned(),
                url: "https://youtube/live".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(100),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Alive".to_owned(),
                artist: "Artist".to_owned(),
                url: "https://youtube/alive".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(100),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Alive",
            "Alive",
            Some("Artist"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        assert_eq!(scored[0].0.url, "https://youtube/alive");
    }

    #[test]
    fn test_regression_vietnamese_folding() {
        let s1 = "Thiệp hồng";
        let s2 = "Thiep hong";
        assert_eq!(normalize_string(s1), normalize_string(s2));
    }

    #[test]
    fn test_regression_on_my_way_remix() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "On My Way".to_owned(),
                artist: "Alan Walker".to_owned(),
                url: "https://youtube/original".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(100000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "On My Way (Da Tweekaz Remix)".to_owned(),
                artist: "Da Tweekaz".to_owned(),
                url: "https://youtube/remix".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(10000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "On My Way Da Tweekaz Remix",
            "On My Way (Da Tweekaz Remix)",
            Some("Alan Walker"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        assert_eq!(scored[0].0.url, "https://youtube/remix");
    }

    #[test]
    fn test_regression_eclipse_vs_clips() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Eclipse Clips".to_owned(),
                artist: "Artist".to_owned(),
                url: "https://youtube/clips".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(100),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Eclipse".to_owned(),
                artist: "Artist".to_owned(),
                url: "https://youtube/eclipse".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(10000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Eclipse",
            "Eclipse",
            Some("Artist"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        assert_eq!(scored[0].0.url, "https://youtube/eclipse");
    }

    #[test]
    fn test_regression_clean_bandit() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Rather Be (Clean Edit)".to_owned(),
                artist: "Clean Bandit".to_owned(),
                url: "https://youtube/clean_edit".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Rather Be".to_owned(),
                artist: "Clean Bandit".to_owned(),
                url: "https://youtube/rather_be".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(100000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Rather Be Clean Bandit",
            "Rather Be",
            Some("Clean Bandit"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        assert_eq!(scored[0].0.url, "https://youtube/rather_be");
    }

    #[test]
    fn test_regression_official_audio_vs_mv() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Clarity (Official MV)".to_owned(),
                artist: "Zedd".to_owned(),
                url: "https://youtube/mv".to_owned(),
                duration: Some(Duration::from_secs(222)), // 3:42
                popularity: Some(100000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Clarity (Official Audio)".to_owned(),
                artist: "Zedd".to_owned(),
                url: "https://youtube/audio".to_owned(),
                duration: Some(Duration::from_secs(204)), // 3:24
                popularity: Some(50000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Clarity Zedd",
            "Clarity",
            Some("Zedd"),
            Some(Duration::from_secs(204)),
            MetadataConfidence::Trusted,
        );

        assert_eq!(scored[0].0.url, "https://youtube/audio");
    }

    #[test]
    fn test_regression_single_word_faded() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Faded (Alan Walker Cover)".to_owned(),
                artist: "Cover Artist".to_owned(),
                url: "https://youtube/cover".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Faded".to_owned(),
                artist: "Alan Walker".to_owned(),
                url: "https://youtube/faded".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(10000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Faded",
            "Faded",
            None,
            Some(Duration::from_secs(200)),
            MetadataConfidence::Untrusted,
        );

        assert_eq!(scored[0].0.url, "https://youtube/faded");
    }

    #[test]
    fn test_regression_cjk() {
        let s1 = "東京 Hot";
        let s2 = "东京 Hot";
        assert!(jaro_winkler_similarity(s1, s2) > 0.85);
    }

    #[test]
    fn test_music_video_relaxation() {
        let candidates = vec![TrackCandidate {
            source: "YouTube".to_owned(),
            title: "SON TUNG M-TP x TYGA | COME MY WAY | OFFICIAL MUSIC VIDEO".to_owned(),
            artist: "Sơn Tùng M-TP x Tyga".to_owned(),
            url: "https://youtube/come_my_way".to_owned(),
            duration: Some(Duration::from_secs(235)),
            popularity: Some(100000),
            is_official: true,
            is_topic_channel: false,
            thumbnail: None,
        }];

        let scored = score_candidates(
            candidates,
            "Son Tung M-TP - Come My Way",
            "Come My Way",
            Some("Son Tung M-TP"),
            Some(Duration::from_secs(192)),
            MetadataConfidence::Trusted,
        );

        assert!(!scored.is_empty());
        assert_eq!(scored[0].0.url, "https://youtube/come_my_way");
    }

    #[test]
    fn test_karaoke_heavy_penalty() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Nếu Mai Chia Tay (Karaoke)".to_owned(),
                artist: "Monstar".to_owned(),
                url: "https://youtube/karaoke".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Nếu Mai Chia Tay".to_owned(),
                artist: "Monstar".to_owned(),
                url: "https://youtube/clean".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Nếu Mai Chia Tay Monstar",
            "Nếu Mai Chia Tay",
            Some("Monstar"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        // Clean candidate should be first, and the karaoke candidate should be completely rejected
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].0.url, "https://youtube/clean");
        assert!(scored.iter().all(|c| c.0.url != "https://youtube/karaoke"));
    }

    #[test]
    fn test_official_audio_boost_over_video() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Nếu Mai Chia Tay (Official MV)".to_owned(),
                artist: "Monstar".to_owned(),
                url: "https://youtube/mv".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Nếu Mai Chia Tay (Official Audio)".to_owned(),
                artist: "Monstar".to_owned(),
                url: "https://youtube/audio".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Nếu Mai Chia Tay Monstar",
            "Nếu Mai Chia Tay",
            Some("Monstar"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        // Official Audio should be first
        assert_eq!(scored[0].0.url, "https://youtube/audio");
        let mv_score = scored
            .iter()
            .find(|c| c.0.url == "https://youtube/mv")
            .unwrap()
            .1;
        let audio_score = scored
            .iter()
            .find(|c| c.0.url == "https://youtube/audio")
            .unwrap()
            .1;
        assert!(
            audio_score > mv_score,
            "Audio score ({}) should be greater than MV score ({})",
            audio_score,
            mv_score
        );
    }

    #[test]
    fn test_clean_version_vs_lyric_video() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "God's Plan (Clean Lyric Video)".to_owned(),
                artist: "Drake".to_owned(),
                url: "https://youtube/clean_lyric".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "God's Plan (Lyric Video)".to_owned(),
                artist: "Drake".to_owned(),
                url: "https://youtube/lyric".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "God's Plan Drake",
            "God's Plan",
            Some("Drake"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        // Lyric Video should beat Clean Lyric
        assert_eq!(scored[0].0.url, "https://youtube/lyric");
    }

    #[test]
    fn test_remixer_identity_bunn_vs_zephier() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Clarity (Zephier Remix)".to_owned(),
                artist: "Zedd".to_owned(),
                url: "https://youtube/zephier".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Clarity (Bunn Remix)".to_owned(),
                artist: "Zedd".to_owned(),
                url: "https://youtube/bunn".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Clarity Bunn Remix",
            "Clarity",
            Some("Zedd"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        // Bunn Remix should win because it aligns with requested remixer
        assert_eq!(scored[0].0.url, "https://youtube/bunn");
    }

    #[test]
    fn test_mv_intro_outro_penalty_scaling() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Blinding Lights (Official MV)".to_owned(),
                artist: "The Weeknd".to_owned(),
                url: "https://youtube/mv_long".to_owned(),
                duration: Some(Duration::from_secs(290)), // +90s delta
                popularity: Some(100000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Blinding Lights (Official MV)".to_owned(),
                artist: "The Weeknd".to_owned(),
                url: "https://youtube/mv_short".to_owned(),
                duration: Some(Duration::from_secs(205)), // +5s delta
                popularity: Some(100000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Blinding Lights",
            "Blinding Lights",
            Some("The Weeknd"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        // MV with smaller duration delta should win
        assert_eq!(scored[0].0.url, "https://youtube/mv_short");
    }

    #[test]
    fn test_shorts_hard_reject_by_duration() {
        let candidates = vec![TrackCandidate {
            source: "YouTube".to_owned(),
            title: "Faded teaser".to_owned(),
            artist: "Alan Walker".to_owned(),
            url: "https://youtube/teaser".to_owned(),
            duration: Some(Duration::from_secs(45)),
            popularity: Some(1000),
            is_official: false,
            is_topic_channel: false,
            thumbnail: None,
        }];

        let scored = score_candidates(
            candidates,
            "Faded Alan Walker",
            "Faded",
            Some("Alan Walker"),
            Some(Duration::from_secs(210)),
            MetadataConfidence::Trusted,
        );

        // Should be rejected at Stage 1
        assert!(scored.is_empty());
    }

    #[test]
    fn test_visualizer_beats_mv() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Faded (Official MV)".to_owned(),
                artist: "Alan Walker".to_owned(),
                url: "https://youtube/mv".to_owned(),
                duration: Some(Duration::from_secs(258)), // +58s delta
                popularity: Some(10000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Faded (Visualizer)".to_owned(),
                artist: "Alan Walker".to_owned(),
                url: "https://youtube/visualizer".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(10000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Faded Alan Walker",
            "Faded",
            Some("Alan Walker"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        // Visualizer should win over MV because it has better media type boost (-0.03 vs -0.15 - delta)
        assert_eq!(scored[0].0.url, "https://youtube/visualizer");
    }

    #[test]
    fn test_loop_hard_reject() {
        let candidates = vec![TrackCandidate {
            source: "YouTube".to_owned(),
            title: "Faded 1 hour loop".to_owned(),
            artist: "Alan Walker".to_owned(),
            url: "https://youtube/loop".to_owned(),
            duration: Some(Duration::from_secs(3600)),
            popularity: Some(1000),
            is_official: false,
            is_topic_channel: false,
            thumbnail: None,
        }];

        let scored = score_candidates(
            candidates,
            "Faded Alan Walker",
            "Faded",
            Some("Alan Walker"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        // Loop should be rejected at Stage 1
        assert!(scored.is_empty());
    }

    #[test]
    fn test_clean_bandit_not_penalized() {
        let media_type = classify_media_type_v2("Symphony", "Clean Bandit", false);
        assert_ne!(media_type, MediaType::CleanVersion);
    }

    #[test]
    fn test_user_song_destroy() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Destroy (Karaoke)".to_owned(),
                artist: "Artist A".to_owned(),
                url: "https://youtube/karaoke".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Destroy (Official MV)".to_owned(),
                artist: "Artist A".to_owned(),
                url: "https://youtube/mv".to_owned(),
                duration: Some(Duration::from_secs(240)),
                popularity: Some(5000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Destroy".to_owned(),
                artist: "Artist A".to_owned(),
                url: "https://youtube/original".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(10000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Destroy Artist A",
            "Destroy",
            Some("Artist A"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        // Karaoke must be hard rejected (only 2 candidates left)
        assert_eq!(scored.len(), 2);
        // Original must beat MV
        assert_eq!(scored[0].0.url, "https://youtube/original");
    }

    #[test]
    fn test_user_song_come_my_way() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "SON TUNG M-TP x TYGA | COME MY WAY | OFFICIAL MUSIC VIDEO".to_owned(),
                artist: "Son Tung M-TP".to_owned(),
                url: "https://youtube/mv".to_owned(),
                duration: Some(Duration::from_secs(235)),
                popularity: Some(100000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Come My Way (Karaoke)".to_owned(),
                artist: "Son Tung M-TP".to_owned(),
                url: "https://youtube/karaoke".to_owned(),
                duration: Some(Duration::from_secs(192)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Come My Way".to_owned(),
                artist: "Son Tung M-TP".to_owned(),
                url: "https://youtube/original".to_owned(),
                duration: Some(Duration::from_secs(192)),
                popularity: Some(50000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Come My Way Son Tung M-TP",
            "Come My Way",
            Some("Son Tung M-TP"),
            Some(Duration::from_secs(192)),
            MetadataConfidence::Trusted,
        );

        // Karaoke rejected
        assert_eq!(scored.len(), 2);
        assert_eq!(scored[0].0.url, "https://youtube/original");
    }

    #[test]
    fn test_user_song_chay_ngay_di() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "CHẠY NGAY ĐI | RUN NOW | SƠN TÙNG M-TP | Official Music Video".to_owned(),
                artist: "Sơn Tùng M-TP".to_owned(),
                url: "https://youtube/mv".to_owned(),
                duration: Some(Duration::from_secs(274)),
                popularity: Some(200000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "[ Karaoke HD ] Chạy Ngay Đi".to_owned(),
                artist: "Sơn Tùng M-TP".to_owned(),
                url: "https://youtube/karaoke".to_owned(),
                duration: Some(Duration::from_secs(272)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Chạy ngay đi Sơn Tùng M-TP",
            "Chạy ngay đi",
            Some("Sơn Tùng M-TP"),
            Some(Duration::from_secs(272)),
            MetadataConfidence::Trusted,
        );

        // Karaoke rejected, only MV survives
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].0.url, "https://youtube/mv");
    }

    #[test]
    fn test_user_song_nang_am_xa_dan() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Nắng Ấm Xa Dần (Karaoke HD)".to_owned(),
                artist: "Sơn Tùng M-TP".to_owned(),
                url: "https://youtube/karaoke".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Nắng Ấm Xa Dần".to_owned(),
                artist: "Sơn Tùng M-TP".to_owned(),
                url: "https://youtube/original".to_owned(),
                duration: Some(Duration::from_secs(200)),
                popularity: Some(50000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Nắng ấm xa dần Sơn Tùng M-TP",
            "Nắng ấm xa dần",
            Some("Sơn Tùng M-TP"),
            Some(Duration::from_secs(200)),
            MetadataConfidence::Trusted,
        );

        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].0.url, "https://youtube/original");
    }

    #[test]
    fn test_user_song_overdose() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "OVERDO$E Lyrics [CLEAN]".to_owned(),
                artist: "PHARMACIST".to_owned(),
                url: "https://youtube/clean".to_owned(),
                duration: Some(Duration::from_secs(120)),
                popularity: Some(10000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "OVERDO$E [Karaoke]".to_owned(),
                artist: "PHARMACIST".to_owned(),
                url: "https://youtube/karaoke".to_owned(),
                duration: Some(Duration::from_secs(120)),
                popularity: Some(500),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "OVERDO$E".to_owned(),
                artist: "PHARMACIST".to_owned(),
                url: "https://youtube/original".to_owned(),
                duration: Some(Duration::from_secs(120)),
                popularity: Some(20000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "Overdo$e PHARMACIST",
            "Overdo$e",
            Some("PHARMACIST"),
            Some(Duration::from_secs(120)),
            MetadataConfidence::Trusted,
        );

        // Both Karaoke and Clean are rejected, only original remains
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].0.url, "https://youtube/original");
    }

    #[test]
    fn test_chu_de_channel_penalized() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Đi Để Trở Về".to_owned(),
                artist: "SOOBIN - Chủ đề".to_owned(),
                url: "https://youtube/chude".to_owned(),
                duration: Some(Duration::from_secs(202)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "Đi Để Trở Về".to_owned(),
                artist: "SOOBIN - Topic".to_owned(),
                url: "https://youtube/topic".to_owned(),
                duration: Some(Duration::from_secs(202)),
                popularity: Some(10000),
                is_official: true,
                is_topic_channel: true,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "SOOBIN - Đi Để Trở Về",
            "Đi Để Trở Về",
            Some("SOOBIN"),
            Some(Duration::from_secs(202)),
            MetadataConfidence::Trusted,
        );

        println!(
            "Topic score: {}, Chu de score: {}",
            scored[0].1, scored[1].1
        );
        assert_eq!(scored[0].0.url, "https://youtube/topic");
        assert!(
            scored[0].1 > scored[1].1,
            "Topic channel should beat Chủ đề channel, got {} vs {}",
            scored[0].1,
            scored[1].1
        );
    }

    #[test]
    fn test_clean_hard_reject() {
        // Test 1: Clean is NOT requested -> Clean version is hard rejected, original is kept
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "OVERDO$E [Clean]".to_owned(),
                artist: "PHARMACIST".to_owned(),
                url: "https://youtube/clean".to_owned(),
                duration: Some(Duration::from_secs(120)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "OVERDO$E".to_owned(),
                artist: "PHARMACIST".to_owned(),
                url: "https://youtube/original".to_owned(),
                duration: Some(Duration::from_secs(120)),
                popularity: Some(1000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates.clone(),
            "OVERDO$E PHARMACIST",
            "OVERDO$E",
            Some("PHARMACIST"),
            Some(Duration::from_secs(120)),
            MetadataConfidence::Trusted,
        );

        // Only the original should survive because the clean version is hard rejected
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].0.url, "https://youtube/original");

        // Test 2: Clean IS requested -> Clean version is kept and scores high
        let scored_requested = score_candidates(
            candidates,
            "OVERDO$E Clean PHARMACIST",
            "OVERDO$E",
            Some("PHARMACIST"),
            Some(Duration::from_secs(120)),
            MetadataConfidence::Trusted,
        );

        // Both survive, and the clean version should win or be kept
        assert_eq!(scored_requested.len(), 2);
        assert_eq!(scored_requested[0].0.url, "https://youtube/clean");
    }

    #[test]
    fn test_remixer_priority_tuca_donka() {
        let candidates = vec![
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "CURSEDEVIL, DJ FKU - TUCA DONKA".to_owned(),
                artist: "CURSEDEVIL".to_owned(),
                url: "https://youtube/original".to_owned(),
                duration: Some(Duration::from_secs(120)),
                popularity: Some(10000),
                is_official: true,
                is_topic_channel: false,
                thumbnail: None,
            },
            TrackCandidate {
                source: "YouTube".to_owned(),
                title: "TUCA DONKA (RXDXVIL Remix)".to_owned(),
                artist: "CURSEDEVIL".to_owned(),
                url: "https://youtube/remix".to_owned(),
                duration: Some(Duration::from_secs(120)),
                popularity: Some(1000),
                is_official: false,
                is_topic_channel: false,
                thumbnail: None,
            },
        ];

        let scored = score_candidates(
            candidates,
            "tuca donka rxdxvil remix",
            "TUCA DONKA",
            Some("CURSEDEVIL"),
            Some(Duration::from_secs(120)),
            MetadataConfidence::Trusted,
        );

        // Remix should win because the remixer is requested, and the original has no remixer
        assert_eq!(scored[0].0.url, "https://youtube/remix");
    }
}
