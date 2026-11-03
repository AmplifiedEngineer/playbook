// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The tab layout of pbtop: which tabs exist and which panels each one shows.
//! The layout is a plain text line in the `usage.ui.layout` config key, for
//! example `Charts=spend,tokens;Numbers=models,breakdown,budget,projection`. Anything
//! that does not parse falls back to the default layout, so a bad value never
//! stops pbtop from starting.

/// A panel that can be placed on a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelId {
    Spend,
    Tokens,
    Models,
    Breakdown,
    Live,
    Sessions,
    Budget,
    Projection,
    Burnup,
}

/// Every panel, in the order the layout editor lists them.
pub const ALL: [PanelId; 9] = [
    PanelId::Spend,
    PanelId::Tokens,
    PanelId::Models,
    PanelId::Breakdown,
    PanelId::Live,
    PanelId::Sessions,
    PanelId::Budget,
    PanelId::Projection,
    PanelId::Burnup,
];

impl PanelId {
    /// The word used in the config value.
    pub fn word(self) -> &'static str {
        match self {
            PanelId::Spend => "spend",
            PanelId::Tokens => "tokens",
            PanelId::Models => "models",
            PanelId::Breakdown => "breakdown",
            PanelId::Live => "live",
            PanelId::Sessions => "sessions",
            PanelId::Budget => "budget",
            PanelId::Projection => "projection",
            PanelId::Burnup => "burnup",
        }
    }

    /// One line saying what the panel shows.
    pub fn describe(self) -> &'static str {
        match self {
            PanelId::Spend => "cost per day as a chart or a daily table",
            PanelId::Tokens => "tokens per day as a chart",
            PanelId::Models => "cost and tokens per model",
            PanelId::Breakdown => "projects, agents, skills, tools or health (v switches)",
            PanelId::Live => "sessions with a message in the last 15 minutes",
            PanelId::Sessions => "every session of the period, grouped by day",
            PanelId::Budget => "limits and how fast you are using them",
            PanelId::Projection => "when each limit is reached at the current pace",
            PanelId::Burnup => "how much of a limit is used over its period, with a projection",
        }
    }

    /// The focusable panel behind this id. The budget strip has none.
    pub fn panel(self) -> Option<super::sort::Panel> {
        use super::sort::Panel;
        match self {
            PanelId::Spend => Some(Panel::Spend),
            PanelId::Tokens => Some(Panel::Tokens),
            PanelId::Models => Some(Panel::Models),
            PanelId::Breakdown => Some(Panel::Projects),
            PanelId::Live => Some(Panel::Live),
            PanelId::Sessions => Some(Panel::Sessions),
            PanelId::Budget | PanelId::Projection | PanelId::Burnup => None,
        }
    }

    pub fn parse(word: &str) -> Option<PanelId> {
        let word = word.trim().to_ascii_lowercase();
        ALL.into_iter().find(|p| p.word() == word)
    }
}

/// One tab: a name and its panels, top left to bottom right. `rows` holds the
/// panel count of each row (1 or 2) for a tab the user arranged, and is `None`
/// for an automatic tab whose grid is picked from the panel count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub name: String,
    pub panels: Vec<PanelId>,
    pub rows: Option<Vec<u8>>,
}

/// The most tabs, so the tab bar fits and each has a number key.
pub const MAX_TABS: usize = 12;
/// The most panels in one row of an arranged tab.
pub const MAX_ROW_PANELS: usize = 2;
/// The most rows in an arranged tab.
pub const MAX_ROWS: usize = 4;
/// The most panels in an arranged tab.
pub const MAX_ARRANGED_PANELS: usize = 8;
/// Separates the rows of an arranged tab in the config value.
const ROW_SEPARATOR: char = '|';
/// The longest tab name, in characters.
pub const MAX_NAME: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub tabs: Vec<Tab>,
}

impl Default for Layout {
    fn default() -> Layout {
        let tab = |name: &str, panels: &[PanelId]| Tab {
            name: name.to_string(),
            panels: panels.to_vec(),
            rows: None,
        };
        Layout {
            tabs: vec![
                tab(
                    "Charts",
                    &[PanelId::Spend, PanelId::Tokens, PanelId::Burnup],
                ),
                tab(
                    "Numbers",
                    &[
                        PanelId::Models,
                        PanelId::Breakdown,
                        PanelId::Budget,
                        PanelId::Projection,
                    ],
                ),
                tab("Sessions", &[PanelId::Live, PanelId::Sessions]),
            ],
        }
    }
}

impl Tab {
    /// The panel list of the config value: commas for an automatic tab, rows
    /// separated by `|` for an arranged one.
    fn panel_text(&self) -> String {
        let words = self.panels.iter().map(|p| p.word());
        let Some(rows) = &self.rows else {
            return words.collect::<Vec<_>>().join(",");
        };
        let mut words = words;
        rows.iter()
            .map(|n| {
                words
                    .by_ref()
                    .take(usize::from(*n))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect::<Vec<_>>()
            .join(&ROW_SEPARATOR.to_string())
    }
}

/// Reads the part after `=` of one tab into panels and, when it holds `|`, the
/// size of each row.
fn parse_rows(name: &str, list: &str) -> Result<(Vec<PanelId>, Option<Vec<u8>>), String> {
    let lines: Vec<&str> = list.split(ROW_SEPARATOR).collect();
    let arranged = lines.len() > 1;
    let mut panels: Vec<PanelId> = Vec::new();
    let mut rows: Vec<u8> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let before = panels.len();
        for word in line.split(',').filter(|w| !w.trim().is_empty()) {
            let panel = PanelId::parse(word).ok_or_else(|| {
                format!(
                    "{:?} is not a panel; use {}",
                    word.trim(),
                    ALL.map(PanelId::word).join(", ")
                )
            })?;
            if panels.contains(&panel) {
                return Err(format!(
                    "the panel `{}` appears twice in `{name}`",
                    panel.word()
                ));
            }
            panels.push(panel);
        }
        let count = panels.len() - before;
        if arranged && count == 0 {
            return Err(format!("row {} of `{name}` is empty", i + 1));
        }
        if arranged && count > MAX_ROW_PANELS {
            return Err(format!(
                "row {} of `{name}` has {count} panels, use at most {MAX_ROW_PANELS}",
                i + 1
            ));
        }
        rows.push(
            u8::try_from(count).map_err(|_| format!("row {} of `{name}` is too long", i + 1))?,
        );
    }
    if panels.is_empty() {
        return Err(format!("the tab `{name}` has no panels"));
    }
    if !arranged {
        return Ok((panels, None));
    }
    if rows.len() > MAX_ROWS || panels.len() > MAX_ARRANGED_PANELS {
        return Err(format!(
            "`{name}` has {} panels in {} rows, use at most {MAX_ARRANGED_PANELS} panels in {MAX_ROWS} rows",
            panels.len(),
            rows.len()
        ));
    }
    Ok((panels, Some(rows)))
}

impl Layout {
    /// One tab with the panels of the single screen pbtop had before tabs, which
    /// is every panel except Projection and Burnup.
    pub fn classic() -> Layout {
        Layout {
            tabs: vec![Tab {
                name: "All".to_string(),
                panels: ALL
                    .iter()
                    .copied()
                    .filter(|p| !matches!(p, PanelId::Projection | PanelId::Burnup))
                    .collect(),
                rows: None,
            }],
        }
    }

    /// Reads `Name=panel,panel;Name=panel`. A `|` in the panel list starts a
    /// new row of one or two panels: `Name=spend,tokens|burnup`. The error says
    /// what is wrong.
    pub fn parse(text: &str) -> Result<Layout, String> {
        let mut tabs: Vec<Tab> = Vec::new();
        for part in text.split(';').filter(|p| !p.trim().is_empty()) {
            let (name, list) = part
                .split_once('=')
                .ok_or_else(|| format!("{:?} needs a name, `=` and panels", part.trim()))?;
            let name = name.trim();
            if name.is_empty() || name.chars().count() > MAX_NAME {
                return Err(format!("a tab name needs 1 to {MAX_NAME} characters"));
            }
            if name.chars().any(char::is_control) {
                return Err("a tab name cannot hold control characters".to_string());
            }
            if tabs.iter().any(|t| t.name.eq_ignore_ascii_case(name)) {
                return Err(format!("the tab `{name}` appears twice"));
            }
            let (panels, rows) = parse_rows(name, list)?;
            tabs.push(Tab {
                name: name.to_string(),
                panels,
                rows,
            });
        }
        if tabs.is_empty() {
            return Err("the layout has no tabs".to_string());
        }
        if tabs.len() > MAX_TABS {
            return Err(format!("at most {MAX_TABS} tabs"));
        }
        Ok(Layout { tabs })
    }

    /// The text form that `parse` reads back.
    pub fn to_text(&self) -> String {
        self.tabs
            .iter()
            .map(|t| format!("{}={}", t.name, t.panel_text()))
            .collect::<Vec<_>>()
            .join(";")
    }

    /// The layout from a stored value. An empty or missing value is the
    /// default without a message; an invalid one is the default and a reason.
    pub fn from_stored(text: Option<&str>) -> (Layout, Option<String>) {
        match text.map(str::trim).filter(|t| !t.is_empty()) {
            None => (Layout::default(), None),
            Some(t) => match Layout::parse(t) {
                Ok(layout) => (layout, None),
                Err(why) => (
                    Layout::default(),
                    Some(format!("ignoring usage.ui.layout: {why}")),
                ),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_layout_has_the_charts_numbers_and_sessions_tabs() {
        let layout = Layout::default();

        let names: Vec<&str> = layout.tabs.iter().map(|t| t.name.as_str()).collect();

        assert_eq!(names, ["Charts", "Numbers", "Sessions"]);
    }

    #[test]
    fn a_layout_round_trips_through_its_text() {
        let layout = Layout::default();

        let back = Layout::parse(&layout.to_text()).unwrap();

        assert_eq!(back, layout);
    }

    #[test]
    fn a_custom_layout_keeps_its_order_and_names() {
        let layout = Layout::parse("Money = budget , models ; Live=live").unwrap();

        assert_eq!(layout.tabs[0].name, "Money");
        assert_eq!(layout.tabs[0].panels, [PanelId::Budget, PanelId::Models]);
        assert_eq!(layout.tabs[1].panels, [PanelId::Live]);
    }

    #[test]
    fn invalid_layouts_are_refused_with_a_reason() {
        for bad in [
            "",
            "NoPanels=",
            "A=nothing",
            "A=spend,spend",
            "A=spend;a=tokens",
            "just text",
            "=spend",
            "ThisNameIsFarTooLongToFit=spend",
        ] {
            assert!(Layout::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn twelve_tabs_are_accepted_and_thirteen_refused() {
        let tabs = |n: usize| -> String {
            (0..n)
                .map(|i| format!("T{i}=spend"))
                .collect::<Vec<_>>()
                .join(";")
        };

        assert_eq!(Layout::parse(&tabs(12)).unwrap().tabs.len(), 12);
        assert!(Layout::parse(&tabs(13)).unwrap_err().contains("at most 12"));
    }

    #[test]
    fn a_tab_without_a_bar_stays_automatic() {
        let layout = Layout::parse("Charts=spend,tokens,burnup").unwrap();

        assert_eq!(layout.tabs[0].rows, None);
        assert_eq!(layout.to_text(), "Charts=spend,tokens,burnup");
    }

    #[test]
    fn a_bar_starts_a_new_row_and_sets_the_row_sizes() {
        let layout = Layout::parse("A=spend,tokens|burnup").unwrap();

        assert_eq!(
            layout.tabs[0].panels,
            [PanelId::Spend, PanelId::Tokens, PanelId::Burnup]
        );
        assert_eq!(layout.tabs[0].rows, Some(vec![2, 1]));
    }

    #[test]
    fn a_tab_with_rows_round_trips() {
        let text = "A=spend,tokens|burnup|models,live;B=budget";

        let layout = Layout::parse(text).unwrap();

        assert_eq!(layout.to_text(), text);
        assert_eq!(Layout::parse(&layout.to_text()).unwrap(), layout);
    }

    #[test]
    fn rows_that_are_too_big_or_empty_are_refused_with_the_row_named() {
        for (bad, hint) in [
            ("Charts=spend|tokens,models,live", "row 2 of `Charts` has 3"),
            ("Charts=spend||tokens", "row 2 of `Charts` is empty"),
            ("Charts=spend|", "row 2 of `Charts` is empty"),
            ("Charts=|spend", "row 1 of `Charts` is empty"),
            ("Charts=spend|spend", "appears twice"),
        ] {
            let why = Layout::parse(bad).unwrap_err();

            assert!(why.contains(hint), "{bad}: {why}");
        }
    }

    #[test]
    fn an_arranged_tab_holds_at_most_eight_panels_and_four_rows() {
        let eight = "A=spend,tokens|models,live|sessions,budget|projection,burnup";
        let five_rows = "A=spend|tokens|models|live|sessions";
        let nine = "A=spend,tokens|models,live|sessions,budget|projection,burnup|breakdown";

        assert!(Layout::parse(eight).is_ok());
        assert!(Layout::parse(five_rows).unwrap_err().contains("4 rows"));
        assert!(Layout::parse(nine).unwrap_err().contains("4 rows"));
    }

    #[test]
    fn an_old_stored_name_that_holds_a_bar_still_loads() {
        let layout = Layout::parse("a|b=spend,tokens").unwrap();

        assert_eq!(layout.tabs[0].name, "a|b");
        assert_eq!(layout.tabs[0].rows, None);
        assert_eq!(layout.to_text(), "a|b=spend,tokens");
    }

    #[test]
    fn every_old_layout_string_loads_unchanged() {
        let default_text = Layout::default().to_text();
        for old in [
            "Charts=spend,tokens",
            "Money=budget,models;Live=live",
            "Charts=spend,tokens;Numbers=models,breakdown,budget,projection;Sessions=live,sessions",
            "T0=spend;T1=spend;T2=spend;T3=spend;T4=spend;T5=spend;T6=spend;T7=spend;T8=spend",
            default_text.as_str(),
        ] {
            let layout = Layout::parse(old).unwrap();

            assert!(layout.tabs.iter().all(|t| t.rows.is_none()), "{old}");
            assert_eq!(layout.to_text(), old);
        }
        let spaced = Layout::parse("Money = budget , models ; Live=live").unwrap();
        assert_eq!(spaced.to_text(), "Money=budget,models;Live=live");
    }

    #[test]
    fn a_fresh_start_uses_the_default_without_a_message() {
        let (layout, warning) = Layout::from_stored(None);

        assert_eq!(layout, Layout::default());
        assert!(warning.is_none());
    }

    #[test]
    fn an_invalid_stored_value_falls_back_to_the_default_with_a_message() {
        let (layout, warning) = Layout::from_stored(Some("A=bogus"));

        assert_eq!(layout, Layout::default());
        assert!(warning.unwrap().contains("bogus"));
    }
}
