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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if is_prefix(key) {
            self.prefix_at = Some(now);
            app.pending_prefix = true;
            return Effect::None;
        }
        if app.pending_prefix {
            self.prefix_at = None;
            app.pending_prefix = false;
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
        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                app.should_close = true;
                Effect::Close
            }
            KeyCode::Enter => Effect::Confirm,
            KeyCode::Left | KeyCode::Char('h') if key.code == KeyCode::Left || ctrl => {
                app.switch_tab(app.tab.adjacent(-1));
                Effect::SelectionChanged
            }
            KeyCode::Right | KeyCode::Char('l') if key.code == KeyCode::Right || ctrl => {
                app.switch_tab(app.tab.adjacent(1));
                Effect::SelectionChanged
            }
            KeyCode::Up | KeyCode::Char('k') if key.code == KeyCode::Up || ctrl => {
                app.move_selection(-1);
                Effect::SelectionChanged
            }
            KeyCode::Down | KeyCode::Char('j') if key.code == KeyCode::Down || ctrl => {
                app.move_selection(1);
                Effect::SelectionChanged
            }
            KeyCode::Char('u') if ctrl => {
                app.state_mut().query.clear();
                app.reconcile_selection();
                Effect::SelectionChanged
            }
            KeyCode::Backspace => {
                app.state_mut().query.pop();
                app.reconcile_selection();
                Effect::SelectionChanged
            }
            KeyCode::Char(character) if !ctrl && !key.modifiers.contains(KeyModifiers::SUPER) => {
                app.state_mut().query.push(character);
                app.reconcile_selection();
                Effect::SelectionChanged
            }
            _ => Effect::None,
        }
    }
}

fn is_prefix(key: KeyEvent) -> bool {
    (key.code == KeyCode::Char(' ') && key.modifiers.contains(KeyModifiers::CONTROL))
        || key.code == KeyCode::Null
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
}
