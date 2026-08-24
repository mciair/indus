use crate::theme::ThemeKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentSource {
    None,
    Theme,
    Effort,
    Values(&'static [ArgumentValue]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArgumentValue {
    pub value: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub usage: &'static str,
    pub argument_placeholder: Option<&'static str>,
    pub arguments_required: bool,
    pub argument_source: ArgumentSource,
}

impl SlashCommand {
    pub const fn plain(name: &'static str, description: &'static str, usage: &'static str) -> Self {
        Self {
            name,
            aliases: &[],
            description,
            usage,
            argument_placeholder: None,
            arguments_required: false,
            argument_source: ArgumentSource::None,
        }
    }

    pub const fn with_args(
        name: &'static str,
        description: &'static str,
        usage: &'static str,
        placeholder: &'static str,
        required: bool,
        argument_source: ArgumentSource,
    ) -> Self {
        Self {
            name,
            aliases: &[],
            description,
            usage,
            argument_placeholder: Some(placeholder),
            arguments_required: required,
            argument_source,
        }
    }

    pub fn accepts_arguments(self) -> bool {
        self.argument_placeholder.is_some()
    }

    pub fn matches_name(self, value: &str) -> bool {
        self.name.eq_ignore_ascii_case(value)
            || self
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(value))
    }
}

const TOGGLE_VALUES: &[ArgumentValue] = &[
    ArgumentValue {
        value: "on",
        description: "Enable this setting",
    },
    ArgumentValue {
        value: "off",
        description: "Disable this setting",
    },
];

pub static COMMANDS: &[SlashCommand] = &[
    SlashCommand::plain("quit", "Quit Indus", "/quit"),
    SlashCommand::plain("help", "Show available commands", "/help"),
    SlashCommand::plain("docs", "Open documentation", "/docs"),
    SlashCommand::plain("home", "Return to the home screen", "/home"),
    SlashCommand::plain("delete", "Delete the current session", "/delete"),
    SlashCommand::plain("new", "Start a new session", "/new"),
    SlashCommand::plain("fork", "Fork the current session", "/fork"),
    SlashCommand::plain("compact", "Compact conversation context", "/compact"),
    SlashCommand::plain("copy", "Copy the last response", "/copy"),
    SlashCommand::with_args(
        "find",
        "Search scrollback",
        "/find <query>",
        "<query>",
        true,
        ArgumentSource::None,
    ),
    SlashCommand::plain("history", "Open prompt history", "/history"),
    SlashCommand::plain("export", "Export the current transcript", "/export"),
    SlashCommand::plain("transcript", "View the full transcript", "/transcript"),
    SlashCommand::plain("edit-prompt", "Edit the previous prompt", "/edit-prompt"),
    SlashCommand::plain("expand", "Expand the current response", "/expand"),
    SlashCommand::plain("model", "Open the provider and model catalog", "/model"),
    SlashCommand::with_args(
        "effort",
        "Set reasoning effort",
        "/effort <level>",
        "<level>",
        true,
        ArgumentSource::Effort,
    ),
    SlashCommand::with_args(
        "always-approve",
        "Configure approval behavior",
        "/always-approve [on|off]",
        "[on|off]",
        false,
        ArgumentSource::Values(TOGGLE_VALUES),
    ),
    SlashCommand::with_args(
        "multiline",
        "Configure multiline input",
        "/multiline [on|off]",
        "[on|off]",
        false,
        ArgumentSource::Values(TOGGLE_VALUES),
    ),
    SlashCommand::with_args(
        "vim-mode",
        "Configure Vim keybindings",
        "/vim-mode [on|off]",
        "[on|off]",
        false,
        ArgumentSource::Values(TOGGLE_VALUES),
    ),
    SlashCommand::plain("share", "Prepare a shareable session link", "/share"),
    SlashCommand::plain("session-info", "Show session metadata", "/session-info"),
    SlashCommand::with_args(
        "rename",
        "Rename the current session",
        "/rename <name>",
        "<name>",
        true,
        ArgumentSource::None,
    ),
    SlashCommand::with_args(
        "theme",
        "Switch the color theme",
        "/theme <name>",
        "<theme>",
        false,
        ArgumentSource::Theme,
    ),
    SlashCommand::with_args(
        "feedback",
        "Send product feedback",
        "/feedback [message]",
        "[message]",
        false,
        ArgumentSource::None,
    ),
    SlashCommand::plain(
        "announcements",
        "Show product announcements",
        "/announcements",
    ),
    SlashCommand::plain("plan", "Enter planning mode", "/plan"),
    SlashCommand::plain("view-plan", "View the current plan", "/view-plan"),
    SlashCommand::plain("resume", "Resume a previous session", "/resume"),
    SlashCommand::plain("mcps", "Manage MCP servers", "/mcps"),
    SlashCommand::plain("skills", "Browse installed skills", "/skills"),
    SlashCommand::plain("workflows", "View saved workflows", "/workflows"),
    SlashCommand::with_args(
        "btw",
        "Ask a side question without interrupting",
        "/btw <question>",
        "<question>",
        true,
        ArgumentSource::None,
    ),
    SlashCommand::plain("recap", "Generate a session recap", "/recap"),
    SlashCommand::plain("doctor", "Run environment diagnostics", "/doctor"),
    SlashCommand::with_args(
        "voice",
        "Configure voice input",
        "/voice [on|off]",
        "[on|off]",
        false,
        ArgumentSource::Values(TOGGLE_VALUES),
    ),
    SlashCommand::with_args(
        "jobs",
        "Set up persistent background work",
        "/jobs <instructions>",
        "<instructions>",
        true,
        ArgumentSource::None,
    ),
    SlashCommand::with_args(
        "timestamps",
        "Configure message timestamps",
        "/timestamps [on|off]",
        "[on|off]",
        false,
        ArgumentSource::Values(TOGGLE_VALUES),
    ),
    SlashCommand::plain("timeline", "Open session timeline", "/timeline"),
    SlashCommand::plain("privacy", "Show privacy controls", "/privacy"),
    SlashCommand::plain("rewind", "Rewind to an earlier turn", "/rewind"),
    SlashCommand::plain("usage", "Show usage summary", "/usage"),
    SlashCommand::plain("queue", "Show queued prompts", "/queue"),
    SlashCommand::plain("release-notes", "Show release notes", "/release-notes"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionPhase {
    Command,
    Arguments { command_index: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Suggestion {
    pub display: String,
    pub description: String,
    pub insert_text: String,
    pub matched_indices: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct SlashMenu {
    pub open: bool,
    pub phase: CompletionPhase,
    pub suggestions: Vec<Suggestion>,
    pub selected: usize,
    pub command_range: std::ops::Range<usize>,
    pub argument_range: Option<std::ops::Range<usize>>,
    pub argument_placeholder: Option<&'static str>,
}

impl Default for SlashMenu {
    fn default() -> Self {
        Self {
            open: false,
            phase: CompletionPhase::Command,
            suggestions: Vec::new(),
            selected: 0,
            command_range: 0..0,
            argument_range: None,
            argument_placeholder: None,
        }
    }
}

impl SlashMenu {
    pub fn refresh(
        &mut self,
        input: &str,
        cursor: usize,
        active_theme: ThemeKind,
        effort_values: &[String],
    ) {
        let previous_insert = self.selection().map(|row| row.insert_text.clone());
        let Some(parsed) = ParsedSlash::from_input(input, cursor) else {
            *self = Self::default();
            return;
        };

        self.command_range = parsed.command_range.clone();
        self.argument_range = parsed.argument_range.clone();
        self.argument_placeholder = parsed
            .command_index
            .and_then(|index| COMMANDS[index].argument_placeholder);

        if let (Some(command_index), Some(argument_range)) =
            (parsed.command_index, parsed.argument_range)
        {
            self.phase = CompletionPhase::Arguments { command_index };
            self.suggestions = argument_suggestions(
                COMMANDS[command_index],
                &input[argument_range.start..cursor.min(argument_range.end)],
                active_theme,
                effort_values,
            );
        } else {
            self.phase = CompletionPhase::Command;
            self.suggestions = command_suggestions(&parsed.command_query);
        }

        self.open = !self.suggestions.is_empty();
        self.selected = previous_insert
            .and_then(|insert| {
                self.suggestions
                    .iter()
                    .position(|row| row.insert_text == insert)
            })
            .unwrap_or(0)
            .min(self.suggestions.len().saturating_sub(1));
    }

    pub fn selection(&self) -> Option<&Suggestion> {
        self.suggestions.get(self.selected)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.suggestions.len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(len as isize) as usize;
    }
}

struct ParsedSlash {
    command_query: String,
    command_index: Option<usize>,
    command_range: std::ops::Range<usize>,
    argument_range: Option<std::ops::Range<usize>>,
}

impl ParsedSlash {
    fn from_input(input: &str, cursor: usize) -> Option<Self> {
        if !input.starts_with('/') || cursor > input.len() {
            return None;
        }
        let token_end = input
            .char_indices()
            .skip(1)
            .find_map(|(index, ch)| ch.is_whitespace().then_some(index))
            .unwrap_or(input.len());
        let command_cursor = cursor.min(token_end);
        let command_query = input[1..command_cursor].to_string();
        let full_name = &input[1..token_end];
        let command_index = COMMANDS
            .iter()
            .position(|command| command.matches_name(full_name));

        let argument_range = if cursor > token_end && command_index.is_some() {
            let start = input[token_end..]
                .char_indices()
                .find_map(|(offset, ch)| (!ch.is_whitespace()).then_some(token_end + offset))
                .unwrap_or(input.len());
            Some(start..input.len())
        } else {
            None
        };

        Some(Self {
            command_query,
            command_index,
            command_range: 0..token_end,
            argument_range,
        })
    }
}

fn command_suggestions(query: &str) -> Vec<Suggestion> {
    let mut matches = COMMANDS
        .iter()
        .filter_map(|command| {
            let display = format!("/{}", command.name);
            let (score, matched_indices) = fuzzy_match(query, command.name)?;
            Some((
                score,
                Suggestion {
                    display,
                    description: command.description.to_string(),
                    insert_text: if command.accepts_arguments() {
                        format!("/{} ", command.name)
                    } else {
                        format!("/{}", command.name)
                    },
                    matched_indices: matched_indices.into_iter().map(|index| index + 1).collect(),
                },
            ))
        })
        .collect::<Vec<_>>();
    if !query.is_empty() {
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.display.cmp(&right.display))
        });
    }
    matches.into_iter().map(|(_, row)| row).collect()
}

fn argument_suggestions(
    command: SlashCommand,
    query: &str,
    active_theme: ThemeKind,
    effort_values: &[String],
) -> Vec<Suggestion> {
    let values = match command.argument_source {
        ArgumentSource::None => return Vec::new(),
        ArgumentSource::Theme => {
            return ThemeKind::ALL
                .into_iter()
                .filter_map(|kind| {
                    let (score, matched_indices) = fuzzy_match(query.trim(), kind.name())?;
                    let active = if kind == active_theme {
                        " (active)"
                    } else {
                        ""
                    };
                    Some((
                        score,
                        Suggestion {
                            display: kind.name().to_string(),
                            description: format!("{}{}", kind.description(), active),
                            insert_text: kind.name().to_string(),
                            matched_indices,
                        },
                    ))
                })
                .collect::<Vec<_>>()
                .pipe(sort_suggestions);
        }
        ArgumentSource::Effort => {
            return effort_values
                .iter()
                .filter_map(|value| {
                    let (score, matched_indices) = fuzzy_match(query.trim(), value)?;
                    Some((
                        score,
                        Suggestion {
                            display: value.clone(),
                            description: effort_description(value).to_string(),
                            insert_text: value.clone(),
                            matched_indices,
                        },
                    ))
                })
                .collect::<Vec<_>>()
                .pipe(sort_suggestions);
        }
        ArgumentSource::Values(values) => values,
    };

    values
        .iter()
        .filter_map(|value| {
            let (score, matched_indices) = fuzzy_match(query.trim(), value.value)?;
            Some((
                score,
                Suggestion {
                    display: value.value.to_string(),
                    description: value.description.to_string(),
                    insert_text: value.value.to_string(),
                    matched_indices,
                },
            ))
        })
        .collect::<Vec<_>>()
        .pipe(sort_suggestions)
}

fn effort_description(value: &str) -> &'static str {
    match value {
        "none" => "Disable model reasoning",
        "minimal" => "Minimal reasoning",
        "default" => "Let the model choose how much to reason",
        "low" => "Faster, lighter reasoning",
        "medium" => "Balanced reasoning",
        "high" => "Deeper reasoning",
        "xhigh" => "Extended reasoning for long tasks",
        "max" => "Maximum model reasoning",
        _ => "Provider-supported reasoning effort",
    }
}

fn sort_suggestions(mut rows: Vec<(i32, Suggestion)>) -> Vec<Suggestion> {
    rows.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.display.cmp(&right.display))
    });
    rows.into_iter().map(|(_, row)| row).collect()
}

fn fuzzy_match(query: &str, candidate: &str) -> Option<(i32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }

    let query = query.to_ascii_lowercase();
    let candidate_lower = candidate.to_ascii_lowercase();
    let mut query_chars = query.chars();
    let mut wanted = query_chars.next()?;
    let mut indices = Vec::new();

    for (index, ch) in candidate_lower.chars().enumerate() {
        if ch != wanted {
            continue;
        }
        indices.push(index);
        match query_chars.next() {
            Some(next) => wanted = next,
            None => {
                let prefix_bonus = if candidate_lower.starts_with(&query) {
                    100
                } else {
                    0
                };
                let compactness = indices
                    .last()
                    .zip(indices.first())
                    .map_or(0, |(last, first)| 30 - (*last as i32 - *first as i32));
                return Some((prefix_bonus + compactness, indices));
            }
        }
    }
    None
}

trait Pipe: Sized {
    fn pipe<T>(self, transform: impl FnOnce(Self) -> T) -> T {
        transform(self)
    }
}

impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_commands_are_not_registered() {
        for name in [
            "plugins",
            "imagine",
            "imagine-video",
            "remember",
            "login",
            "logout",
            "context",
            "auto",
            "settings",
            "jump",
            "config-agents",
            "loop",
            "tutorial",
            "tasks",
        ] {
            assert!(COMMANDS.iter().all(|command| command.name != name));
        }
    }

    #[test]
    fn theme_command_changes_to_argument_suggestions() {
        let mut menu = SlashMenu::default();
        menu.refresh("/theme ", 7, ThemeKind::IndusNight, &[]);
        assert!(matches!(
            menu.phase,
            CompletionPhase::Arguments { command_index }
                if COMMANDS[command_index].name == "theme"
        ));
        assert_eq!(menu.suggestions.len(), ThemeKind::ALL.len());
        assert!(
            menu.suggestions
                .iter()
                .find(|row| row.display == "indus-night")
                .is_some_and(|row| row.description.contains("active"))
        );
    }

    #[test]
    fn command_matching_is_ranked_and_highlighted() {
        let rows = command_suggestions("thm");
        assert_eq!(rows.first().map(|row| row.display.as_str()), Some("/theme"));
        assert!(!rows[0].matched_indices.is_empty());
    }

    #[test]
    fn unavailable_persona_and_dashboard_commands_are_hidden() {
        let names = COMMANDS
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>();
        assert!(!names.contains(&"personas"));
        assert!(!names.contains(&"dashboard"));
        assert!(names.contains(&"compact"));
        assert!(!names.contains(&"compact-mode"));
        assert!(!names.contains(&"cd"));
    }

    #[test]
    fn effort_suggestions_use_only_selected_model_capabilities() {
        let mut menu = SlashMenu::default();
        let efforts = vec!["low".to_string(), "max".to_string()];
        menu.refresh("/effort ", 8, ThemeKind::IndusNight, &efforts);
        assert_eq!(
            menu.suggestions
                .iter()
                .map(|row| row.display.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "max"]
        );
    }
}
