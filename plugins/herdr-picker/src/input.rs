use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::model::{App, Tab};

const PREFIX_TIMEOUT: Duration = Duration::from_millis(700);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    SelectionChanged,
    Confirm,
    Close,
}

pub struct Reducer {
    prefix_at: Option<Instant>,
}

impl Reducer {
    pub fn new() -> Self {
        Self { prefix_at: None }
    }

    pub fn expire_prefix(&mut self, app: &mut App, now: Instant) {
        if self
            .prefix_at
            .is_some_and(|at| now.duration_since(at) >= PREFIX_TIMEOUT)
        {
            self.prefix_at = None;
            app.pending_prefix = false;
        }
    }

    pub fn key(&mut self, app: &mut App, key: KeyEvent, now: Instant) -> Effect {
        self.expire_prefix(app, now);
        if is_prefix(key) {
            self.prefix_at = Some(now);
            app.pending_prefix = true;
            return Effect::None;
        }
        if app.pending_prefix {
            self.prefix_at = None;
            app.pending_prefix = false;
            if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
                return Effect::None;
            }
            let requested = match key.code {
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    Some(Tab::Agents)
                }
                KeyCode::Char('S') => Some(Tab::Agents),
                KeyCode::Char('s') => Some(Tab::Workspaces),
                _ => None,
            };
            if let Some(tab) = requested {
                if app.tab == tab {
                    app.should_close = true;
                    return Effect::Close;
                }
                app.switch_tab(tab);
                return Effect::SelectionChanged;
            }
            return Effect::None;
        }
        if let Some(shortcut) = control_shortcut(key) {
            return match shortcut {
                'c' => {
                    app.should_close = true;
                    Effect::Close
                }
                'h' => {
                    app.switch_tab(app.tab.adjacent(-1));
                    Effect::SelectionChanged
                }
                'l' => {
                    app.switch_tab(app.tab.adjacent(1));
                    Effect::SelectionChanged
                }
                'k' | 'p' => {
                    app.move_selection(-1);
                    Effect::SelectionChanged
                }
                'j' => {
                    app.move_selection(1);
                    Effect::SelectionChanged
                }
                'u' => {
                    app.state_mut().query.clear();
                    app.reconcile_selection();
                    Effect::SelectionChanged
                }
                'o' => {
                    app.cycle_sort();
                    Effect::SelectionChanged
                }
                _ => Effect::None,
            };
        }
        match key.code {
            KeyCode::Esc => {
                app.should_close = true;
                Effect::Close
            }
            KeyCode::Enter => Effect::Confirm,
            KeyCode::Left => {
                app.switch_tab(app.tab.adjacent(-1));
                Effect::SelectionChanged
            }
            KeyCode::Right => {
                app.switch_tab(app.tab.adjacent(1));
                Effect::SelectionChanged
            }
            KeyCode::Up => {
                app.move_selection(-1);
                Effect::SelectionChanged
            }
            KeyCode::Down => {
                app.move_selection(1);
                Effect::SelectionChanged
            }
            KeyCode::Backspace => {
                app.state_mut().query.pop();
                app.reconcile_selection();
                Effect::SelectionChanged
            }
            KeyCode::Char(character)
                if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
            {
                app.state_mut().query.push(character);
                app.reconcile_selection();
                Effect::SelectionChanged
            }
            _ => Effect::None,
        }
    }
}

fn is_prefix(key: KeyEvent) -> bool {
    (key.code == KeyCode::Char(' ') && has_control_modifiers(key.modifiers))
        || (key.code == KeyCode::Null
            && key
                .modifiers
                .difference(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
                .is_empty())
}

fn control_shortcut(key: KeyEvent) -> Option<char> {
    if !has_control_modifiers(key.modifiers) {
        return None;
    }
    match key.code {
        KeyCode::Char(character) if character.is_ascii_alphabetic() => {
            Some(character.to_ascii_lowercase())
        }
        _ => None,
    }
}

fn has_control_modifiers(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::CONTROL)
        && modifiers
            .difference(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
            .is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn printable_keys_type_immediately_by_default() {
        let mut app = App::new(Tab::Workspaces);
        Reducer::new().key(
            &mut app,
            key(KeyCode::Char('x'), KeyModifiers::NONE),
            Instant::now(),
        );
        assert_eq!(app.state().query, "x");
    }

    #[test]
    fn prefix_chords_switch_or_close() {
        let now = Instant::now();
        let mut reducer = Reducer::new();
        let mut app = App::new(Tab::Workspaces);
        reducer.key(&mut app, key(KeyCode::Null, KeyModifiers::NONE), now);
        assert_eq!(
            reducer.key(&mut app, key(KeyCode::Char('S'), KeyModifiers::SHIFT), now),
            Effect::SelectionChanged
        );
        assert_eq!(app.tab, Tab::Agents);
        reducer.key(&mut app, key(KeyCode::Null, KeyModifiers::NONE), now);
        assert_eq!(
            reducer.key(&mut app, key(KeyCode::Char('s'), KeyModifiers::SHIFT), now),
            Effect::Close
        );
    }

    #[test]
    fn prefix_s_closes_workspaces_when_already_active() {
        let now = Instant::now();
        let mut reducer = Reducer::new();
        let mut app = App::new(Tab::Workspaces);
        reducer.key(
            &mut app,
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            now,
        );
        assert_eq!(
            reducer.key(&mut app, key(KeyCode::Char('s'), KeyModifiers::NONE), now),
            Effect::Close
        );
    }

    #[test]
    fn ctrl_navigation_keys_move_tabs_and_items() {
        let now = Instant::now();
        let mut reducer = Reducer::new();
        let mut app = App::new(Tab::Workspaces);
        app.set_items(Tab::Workspaces, vec![test_item("one"), test_item("two")]);
        reducer.key(
            &mut app,
            key(KeyCode::Char('l'), KeyModifiers::CONTROL),
            now,
        );
        assert_eq!(app.tab, Tab::Agents);
        reducer.key(
            &mut app,
            key(KeyCode::Char('h'), KeyModifiers::CONTROL),
            now,
        );
        assert_eq!(app.tab, Tab::Workspaces);
        reducer.key(
            &mut app,
            key(KeyCode::Char('j'), KeyModifiers::CONTROL),
            now,
        );
        assert_eq!(app.state().selected_id.as_deref(), Some("two"));
        reducer.key(
            &mut app,
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            now,
        );
        assert_eq!(app.state().selected_id.as_deref(), Some("one"));
    }

    #[test]
    fn backspace_edits_but_ctrl_h_changes_tabs() {
        let now = Instant::now();
        let mut reducer = Reducer::new();
        let mut app = App::new(Tab::Agents);
        app.state_mut().query = "ab".into();
        reducer.key(&mut app, key(KeyCode::Backspace, KeyModifiers::NONE), now);
        assert_eq!(app.state().query, "a");
        reducer.key(
            &mut app,
            key(KeyCode::Char('h'), KeyModifiers::CONTROL),
            now,
        );
        assert_eq!(app.tab, Tab::Workspaces);
    }

    #[test]
    fn escape_and_ctrl_c_close() {
        for event in [
            key(KeyCode::Esc, KeyModifiers::NONE),
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let mut app = App::new(Tab::Workspaces);
            assert_eq!(
                Reducer::new().key(&mut app, event, Instant::now()),
                Effect::Close
            );
        }
    }

    #[test]
    fn expired_prefix_does_not_consume_following_text() {
        let now = Instant::now();
        let mut reducer = Reducer::new();
        let mut app = App::new(Tab::Workspaces);
        reducer.key(&mut app, key(KeyCode::Null, KeyModifiers::NONE), now);
        reducer.key(
            &mut app,
            key(KeyCode::Char('s'), KeyModifiers::NONE),
            now + PREFIX_TIMEOUT,
        );
        assert_eq!(app.state().query, "s");
        assert!(!app.pending_prefix);
    }

    #[test]
    fn ghostty_kitty_shift_s_event_opens_agents() {
        let now = Instant::now();
        let mut reducer = Reducer::new();
        let mut app = App::new(Tab::Zoxide);
        reducer.key(&mut app, key(KeyCode::Null, KeyModifiers::NONE), now);
        let shifted_s = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT);
        assert_eq!(
            reducer.key(&mut app, shifted_s, now),
            Effect::SelectionChanged
        );
        assert_eq!(app.tab, Tab::Agents);
    }

    fn test_item(id: &str) -> crate::model::Item {
        crate::model::Item {
            id: id.into(),
            label: id.into(),
            detail: String::new(),
            search: id.into(),
            preview_pane: None,
            match_paths: Vec::new(),
            target: crate::model::Target::Workspace { id: id.into() },
        }
    }

    #[test]
    fn ctrl_p_moves_up_with_wrap_while_plain_p_types() {
        let now = Instant::now();
        let mut reducer = Reducer::new();
        let mut app = App::new(Tab::Workspaces);
        app.set_items(Tab::Workspaces, vec![test_item("one"), test_item("two")]);
        reducer.key(
            &mut app,
            key(KeyCode::Char('p'), KeyModifiers::CONTROL),
            now,
        );
        assert_eq!(app.state().selected_id.as_deref(), Some("two"));
        reducer.key(&mut app, key(KeyCode::Char('p'), KeyModifiers::NONE), now);
        assert_eq!(app.state().query, "p");
    }

    #[test]
    fn ctrl_o_cycles_sort_without_editing_query() {
        let now = Instant::now();
        let mut reducer = Reducer::new();
        let mut app = App::new(Tab::Workspaces);
        app.state_mut().query = "text".into();
        for expected in [
            crate::model::SortOrder::AgeAscending,
            crate::model::SortOrder::AgeDescending,
            crate::model::SortOrder::Recent,
        ] {
            reducer.key(
                &mut app,
                key(KeyCode::Char('o'), KeyModifiers::CONTROL),
                now,
            );
            assert_eq!(app.state().sort, expected);
            assert_eq!(app.state().query, "text");
        }
    }

    #[test]
    fn ctrl_shortcut_modifier_matrix_is_consistent() {
        let now = Instant::now();
        for (code, modifiers) in [
            (KeyCode::Char('p'), KeyModifiers::CONTROL),
            (
                KeyCode::Char('P'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        ] {
            let mut app = App::new(Tab::Workspaces);
            app.set_items(Tab::Workspaces, vec![test_item("one"), test_item("two")]);
            Reducer::new().key(&mut app, key(code, modifiers), now);
            assert_eq!(app.state().selected_id.as_deref(), Some("two"));
        }
        for (code, modifiers, expected) in [
            (KeyCode::Char('p'), KeyModifiers::NONE, "p"),
            (KeyCode::Char('P'), KeyModifiers::SHIFT, "P"),
            (KeyCode::Char('o'), KeyModifiers::NONE, "o"),
            (KeyCode::Char('O'), KeyModifiers::SHIFT, "O"),
        ] {
            let mut app = App::new(Tab::Workspaces);
            Reducer::new().key(&mut app, key(code, modifiers), now);
            assert_eq!(app.state().query, expected);
        }
        for forbidden in [
            KeyModifiers::ALT,
            KeyModifiers::SUPER,
            KeyModifiers::META,
            KeyModifiers::HYPER,
        ] {
            for character in ['P', 'O'] {
                let mut app = App::new(Tab::Workspaces);
                app.set_items(Tab::Workspaces, vec![test_item("one"), test_item("two")]);
                Reducer::new().key(
                    &mut app,
                    key(KeyCode::Char(character), KeyModifiers::CONTROL | forbidden),
                    now,
                );
                assert_eq!(app.state().selected_id.as_deref(), Some("one"));
                assert_eq!(app.state().sort, crate::model::SortOrder::Recent);
                assert!(app.state().query.is_empty());
            }
        }
    }

    #[test]
    fn ctrl_shift_o_cycles_sort() {
        let mut app = App::new(Tab::Workspaces);
        Reducer::new().key(
            &mut app,
            key(
                KeyCode::Char('O'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            Instant::now(),
        );
        assert_eq!(app.state().sort, crate::model::SortOrder::AgeAscending);
        assert!(app.state().query.is_empty());
    }

    #[test]
    fn prefix_rejects_extra_modifiers() {
        let now = Instant::now();
        for forbidden in [
            KeyModifiers::ALT,
            KeyModifiers::SUPER,
            KeyModifiers::META,
            KeyModifiers::HYPER,
        ] {
            let mut app = App::new(Tab::Workspaces);
            let mut reducer = Reducer::new();
            reducer.key(
                &mut app,
                key(KeyCode::Char(' '), KeyModifiers::CONTROL | forbidden),
                now,
            );
            assert!(!app.pending_prefix);
            reducer.key(&mut app, key(KeyCode::Null, forbidden), now);
            assert!(!app.pending_prefix);
        }
    }
}
