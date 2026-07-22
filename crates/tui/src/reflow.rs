//! The reflow ladder (#50): how the wizard frame degrades as the terminal shrinks.
//!
//! The screen sheds *chrome* before it squeezes the *body* — the body never drops below
//! [`MIN_BODY_ROWS`] usable rows. Tier selection ([`ReflowTier::for_height`]) is a pure
//! function of the available height, so it is unit-tested directly (no terminal, no render)
//! and the render path just asks each tier how many rows its header/footer want.
//!
//! Below the [`ReflowTier::Refuse`] cutoff the frame can't hold at all; the render draws a
//! single full-screen "terminal too small" notice instead of a broken frame.

/// The wizard body is never squeezed below this many rows: chrome sheds down to its
/// [`ReflowTier::Minimum`] form (a one-line stepper + a one-line footer) first, and the
/// [`ReflowTier::Refuse`] cutoff (< 10 rows) sits high enough that even the tightest laid-out
/// frame — 1 header + 1 banner + 1 footer over a 10-row terminal — still leaves 7 body rows.
pub const MIN_BODY_ROWS: u16 = 6;

/// The smallest terminal height that still renders the frame; anything shorter is
/// [`ReflowTier::Refuse`]d with a full-screen notice.
pub const MIN_FRAME_ROWS: u16 = 10;

/// The fixed rungs of the adaptive-height ladder (#50), widest terminal first. Each rung
/// fixes how much vertical chrome the header and footer are allowed to claim; the body takes
/// whatever is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflowTier {
    /// ≥ 30 rows: full stepper row plus a context/status line under it, full footer hints.
    Comfortable,
    /// 20–29 rows: stepper row only (the context line folds away), full footer hints.
    Default,
    /// 14–19 rows: the stepper compacts and the footer keeps only essential hints.
    Tight,
    /// 10–13 rows: a one-line compact stepper over a one-line minimal footer.
    Minimum,
    /// < 10 rows: the frame can't hold — the render shows a single full-screen notice.
    Refuse,
}

impl ReflowTier {
    /// Selects the tier for a terminal `height`. Pure and total, so the reducer/layout tests
    /// pin the exact rung boundaries without a terminal.
    #[must_use]
    pub fn for_height(height: u16) -> Self {
        match height {
            h if h >= 30 => Self::Comfortable,
            20..=29 => Self::Default,
            14..=19 => Self::Tight,
            10..=13 => Self::Minimum,
            _ => Self::Refuse,
        }
    }

    /// Rows the fixed header claims at this tier. `Comfortable` spends a second row on the
    /// context/status line; every laid-out tier keeps the one stepper row; `Refuse` draws no
    /// frame at all.
    #[must_use]
    pub fn header_rows(self) -> u16 {
        match self {
            Self::Comfortable => 2,
            Self::Default | Self::Tight | Self::Minimum => 1,
            Self::Refuse => 0,
        }
    }

    /// Rows the fixed footer claims at this tier (one hint line for every laid-out tier).
    #[must_use]
    pub fn footer_rows(self) -> u16 {
        match self {
            Self::Refuse => 0,
            _ => 1,
        }
    }

    /// Whether this tier has the room for the context/status line under the stepper (only
    /// `Comfortable` does — below it the line folds into REVIEW and the footer, per #50).
    #[must_use]
    pub fn shows_context_line(self) -> bool {
        matches!(self, Self::Comfortable)
    }

    /// Whether the stepper renders in its compact glyphs-plus-current-title form (`Tight` and
    /// below) rather than the full `glyph Title → …` row.
    #[must_use]
    pub fn compact_stepper(self) -> bool {
        matches!(self, Self::Tight | Self::Minimum)
    }

    /// Whether the frame can be drawn at all. `Refuse` renders the full-screen notice instead.
    #[must_use]
    pub fn renders_frame(self) -> bool {
        !matches!(self, Self::Refuse)
    }
}

/// The WHAT explorer breadcrumb's responsive rungs (#85), mirroring [`ReflowTier`]'s pure
/// `for_width` shape but keyed off the explorer pane's *inner* column width (post-border)
/// rather than terminal height — the breadcrumb sheds width the same way the frame sheds
/// height, one ladder per axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreadcrumbWidth {
    /// >= 60 inner cols: `Category / segment / segment` — spaced separators, Category shown.
    Comfortable,
    /// 30-59 inner cols: `Category/segment/segment` — tight separators, Category still shown.
    Tight,
    /// < 30 inner cols: `../segment/segment` — Category dropped, `../` prefix; if still too
    /// wide, [`render_breadcrumb`] sheds the oldest remaining segment first.
    Compact,
}

impl BreadcrumbWidth {
    /// Selects the rung for the explorer pane's inner `cols`. Pure and total, so the 60/30
    /// rung boundaries are unit-tested without a terminal (the thresholds are tunable starting
    /// cut points, not load-bearing constants).
    #[must_use]
    pub fn for_width(cols: u16) -> Self {
        match cols {
            c if c >= 60 => Self::Comfortable,
            30..=59 => Self::Tight,
            _ => Self::Compact,
        }
    }
}

/// Renders the WHAT breadcrumb's plain text for `tier`: the Category head plus every
/// descended `segments` component, never with a leading `/` (the head is always a Category
/// display name, never a path root). `Comfortable`/`Tight` differ only in separator spacing;
/// `Compact` drops the Category and prefixes `../`, shedding the **oldest** remaining segment
/// first when the full trail still overflows `cols` — the tail (what the user just descended
/// into) is what they're looking at, so it is never shed, even if the result still overflows.
#[must_use]
pub fn render_breadcrumb(
    tier: BreadcrumbWidth,
    category: &str,
    segments: &[String],
    cols: u16,
) -> String {
    match tier {
        BreadcrumbWidth::Comfortable => join_with_category(category, segments, " / "),
        BreadcrumbWidth::Tight => join_with_category(category, segments, "/"),
        BreadcrumbWidth::Compact => compact_breadcrumb(segments, cols),
    }
}

/// The `Comfortable`/`Tight` form: the Category head followed by every segment, joined by
/// `sep`. With no segments this is just the Category name (no dangling separator).
fn join_with_category(category: &str, segments: &[String], sep: &str) -> String {
    std::iter::once(category)
        .chain(segments.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(sep)
}

/// The `Compact` form: no Category, a `../` prefix, oldest-first shedding until the trail fits
/// `cols` — stopping at the tail regardless of fit, since dropping it would leave nothing of
/// the current location on screen. Empty `segments` (the explorer's top level) renders empty:
/// there is nothing descended into to abbreviate.
fn compact_breadcrumb(segments: &[String], cols: u16) -> String {
    if segments.is_empty() {
        return String::new();
    }
    let cols = usize::from(cols);
    let mut start = 0;
    loop {
        let candidate = format!("../{}", segments[start..].join("/"));
        if candidate.chars().count() <= cols || start + 1 == segments.len() {
            return candidate;
        }
        start += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_boundaries_match_the_fixed_ladder() {
        assert_eq!(ReflowTier::for_height(45), ReflowTier::Comfortable);
        assert_eq!(ReflowTier::for_height(30), ReflowTier::Comfortable);
        assert_eq!(ReflowTier::for_height(29), ReflowTier::Default);
        assert_eq!(ReflowTier::for_height(20), ReflowTier::Default);
        assert_eq!(ReflowTier::for_height(19), ReflowTier::Tight);
        assert_eq!(ReflowTier::for_height(14), ReflowTier::Tight);
        assert_eq!(ReflowTier::for_height(13), ReflowTier::Minimum);
        assert_eq!(ReflowTier::for_height(10), ReflowTier::Minimum);
        assert_eq!(ReflowTier::for_height(9), ReflowTier::Refuse);
        assert_eq!(ReflowTier::for_height(0), ReflowTier::Refuse);
    }

    #[test]
    fn every_laid_out_tier_leaves_at_least_the_minimum_body() {
        // The floor the ladder promises: even at the tightest laid-out height, with a gate
        // banner claiming its extra row, the body never drops below `MIN_BODY_ROWS`.
        for height in MIN_FRAME_ROWS..=29 {
            let tier = ReflowTier::for_height(height);
            assert!(
                tier.renders_frame(),
                "height {height} should render a frame"
            );
            let banner_row = 1;
            let chrome = tier.header_rows() + tier.footer_rows() + banner_row;
            let body = height - chrome;
            assert!(
                body >= MIN_BODY_ROWS,
                "height {height} ({tier:?}) left only {body} body rows"
            );
        }
    }

    #[test]
    fn refuse_draws_no_frame_chrome() {
        let tier = ReflowTier::for_height(4);
        assert!(!tier.renders_frame());
        assert_eq!(tier.header_rows(), 0);
        assert_eq!(tier.footer_rows(), 0);
    }

    #[test]
    fn only_comfortable_shows_the_context_line() {
        assert!(ReflowTier::Comfortable.shows_context_line());
        assert!(!ReflowTier::Default.shows_context_line());
        assert!(!ReflowTier::Tight.shows_context_line());
    }

    #[test]
    fn stepper_compacts_at_tight_and_below() {
        assert!(!ReflowTier::Comfortable.compact_stepper());
        assert!(!ReflowTier::Default.compact_stepper());
        assert!(ReflowTier::Tight.compact_stepper());
        assert!(ReflowTier::Minimum.compact_stepper());
    }

    fn segs(names: &[&str]) -> Vec<String> {
        names.iter().map(|&s| s.to_string()).collect()
    }

    #[test]
    fn breadcrumb_width_rung_boundaries() {
        assert_eq!(BreadcrumbWidth::for_width(61), BreadcrumbWidth::Comfortable);
        assert_eq!(BreadcrumbWidth::for_width(60), BreadcrumbWidth::Comfortable);
        assert_eq!(BreadcrumbWidth::for_width(59), BreadcrumbWidth::Tight);
        assert_eq!(BreadcrumbWidth::for_width(30), BreadcrumbWidth::Tight);
        assert_eq!(BreadcrumbWidth::for_width(29), BreadcrumbWidth::Compact);
        assert_eq!(BreadcrumbWidth::for_width(0), BreadcrumbWidth::Compact);
    }

    #[test]
    fn comfortable_spaces_the_separators_and_shows_the_category() {
        let text = render_breadcrumb(
            BreadcrumbWidth::Comfortable,
            "Custom paths",
            &segs(&["projects", "src", "actions"]),
            60,
        );
        assert_eq!(text, "Custom paths / projects / src / actions");
    }

    #[test]
    fn tight_packs_the_separators_and_still_shows_the_category() {
        let text = render_breadcrumb(
            BreadcrumbWidth::Tight,
            "Custom paths",
            &segs(&["projects", "src", "actions"]),
            40,
        );
        assert_eq!(text, "Custom paths/projects/src/actions");
    }

    #[test]
    fn comfortable_and_tight_show_just_the_category_at_the_top_level() {
        assert_eq!(
            render_breadcrumb(BreadcrumbWidth::Comfortable, "Shell", &[], 60),
            "Shell"
        );
        assert_eq!(
            render_breadcrumb(BreadcrumbWidth::Tight, "Shell", &[], 40),
            "Shell"
        );
    }

    #[test]
    fn compact_drops_the_category_and_prefixes_dotdot_slash() {
        let text = render_breadcrumb(
            BreadcrumbWidth::Compact,
            "Custom paths",
            &segs(&["projects", "src", "actions"]),
            23, // exactly fits the untruncated trail
        );
        assert_eq!(text, "../projects/src/actions");
        assert!(!text.contains("Custom paths"));
        assert!(!text.starts_with('/'), "no leading /: {text}");
    }

    #[test]
    fn compact_sheds_the_oldest_segment_first_when_it_overflows() {
        let text = render_breadcrumb(
            BreadcrumbWidth::Compact,
            "Custom paths",
            &segs(&["projects", "src", "actions"]),
            14, // too narrow for the full trail, exactly fits after shedding "projects"
        );
        assert_eq!(text, "../src/actions");
    }

    #[test]
    fn compact_never_sheds_the_tail_even_if_still_overflowing() {
        let text = render_breadcrumb(
            BreadcrumbWidth::Compact,
            "Custom paths",
            &segs(&["projects", "src", "actions"]),
            5, // narrower than even the last segment alone
        );
        assert_eq!(text, "../actions", "the tail survives regardless of fit");
    }

    #[test]
    fn compact_at_the_top_level_renders_nothing() {
        assert_eq!(
            render_breadcrumb(BreadcrumbWidth::Compact, "Shell", &[], 20),
            ""
        );
    }
}
