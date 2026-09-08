use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

#[derive(Debug, Clone)]
pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Tick,
    Quit,
}

impl From<Event> for AppEvent {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(key) => Self::Key(key),
            Event::Mouse(mouse) => Self::Mouse(mouse),
            Event::Resize(w, h) => Self::Resize(w, h),
            Event::Paste(_) | Event::FocusGained | Event::FocusLost => Self::Tick,
        }
    }
}

impl AppEvent {
    /// Whether this event asks the application to quit.
    ///
    /// Only `Ctrl+C`. The search box always has focus, so a plain `q` is a
    /// character the user is typing: matching it here quit the application in
    /// the middle of any query containing the letter. The detail overlay is a
    /// modal with no text entry and still closes on `q`, which it matches for
    /// itself.
    pub fn is_quit(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_up(&self) -> bool {
        matches!(
            self,
            Self::Key(
                KeyEvent {
                    code: KeyCode::Up,
                    modifiers: KeyModifiers::NONE,
                    ..
                } | KeyEvent {
                    code: KeyCode::Char('k'),
                    modifiers: KeyModifiers::NONE,
                    ..
                }
            )
        )
    }

    pub fn is_down(&self) -> bool {
        matches!(
            self,
            Self::Key(
                KeyEvent {
                    code: KeyCode::Down,
                    modifiers: KeyModifiers::NONE,
                    ..
                } | KeyEvent {
                    code: KeyCode::Char('j'),
                    modifiers: KeyModifiers::NONE,
                    ..
                }
            )
        )
    }

    pub fn is_page_up(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::PageUp,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_page_down(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::PageDown,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_home(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Home,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_end(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::End,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_enter(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_escape(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_tab(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    /// Ctrl+D pins or unpins the detail pane.
    ///
    /// This used to be plain Space. Every unmodified printable character is
    /// search input now, so Space types a space into the query and can no
    /// longer double as a command key.
    pub fn is_ctrl_d(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    /// Ctrl+H toggles the help overlay.
    ///
    /// It used to be `?`. Every unmodified printable character is search input,
    /// and `?` is meaningful in a regex query, so the shortcut needs a modifier.
    ///
    /// A terminal without `DISAMBIGUATE_ESCAPE_CODES` sends 0x08 for Ctrl+H,
    /// which decodes as `Backspace`. `Backspace` with CONTROL is accepted
    /// because it can only be Ctrl+H; a plain `Backspace` is not, because there
    /// the terminal has already lost the distinction and it is ordinary
    /// editing. F1 is the binding that works on every terminal.
    pub fn is_ctrl_h(&self) -> bool {
        matches!(
            self,
            Self::Key(
                KeyEvent {
                    code: KeyCode::Char('h') | KeyCode::Backspace,
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } | KeyEvent {
                    code: KeyCode::F(1),
                    modifiers: KeyModifiers::NONE,
                    ..
                }
            )
        )
    }

    pub fn is_ctrl_r(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('r'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    /// Ctrl+J toggles JSON output.
    ///
    /// Same ambiguity as [`Self::is_ctrl_h`]: a legacy terminal sends 0x0a for
    /// Ctrl+J, which decodes as `Enter`. `Enter` with CONTROL can only be
    /// Ctrl+J and is accepted; a plain `Enter` opens the detail pane and is
    /// not. F2 works on every terminal.
    pub fn is_ctrl_j(&self) -> bool {
        matches!(
            self,
            Self::Key(
                KeyEvent {
                    code: KeyCode::Char('j') | KeyCode::Enter,
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } | KeyEvent {
                    code: KeyCode::F(2),
                    modifiers: KeyModifiers::NONE,
                    ..
                }
            )
        )
    }

    pub fn is_ctrl_t(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_slash(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('/'),
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_colon(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char(':'),
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_question(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('?'),
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_ctrl_a(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_ctrl_y(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('y'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_ctrl_e(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('e'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_ctrl_p(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_ctrl_f(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('f'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_ctrl_x(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_ctrl_n(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn as_char(&self) -> Option<char> {
        if let Self::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            ..
        }) = self
        {
            Some(*c)
        } else {
            None
        }
    }

    pub fn is_mouse_scroll_down(&self) -> bool {
        matches!(
            self,
            Self::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                ..
            })
        )
    }

    pub fn is_mouse_scroll_up(&self) -> bool {
        matches!(
            self,
            Self::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                ..
            })
        )
    }

    pub fn is_mouse_click(&self) -> bool {
        matches!(
            self,
            Self::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                ..
            })
        )
    }

    pub fn mouse_row(&self) -> Option<u16> {
        if let Self::Mouse(MouseEvent { row, .. }) = self {
            Some(*row)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod ambiguous_control_key_tests {
    use super::AppEvent;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn ctrl_h_is_recognised_however_the_terminal_spells_it() {
        assert!(key(KeyCode::Char('h'), KeyModifiers::CONTROL).is_ctrl_h());
        // 0x08 on a terminal without DISAMBIGUATE_ESCAPE_CODES.
        assert!(key(KeyCode::Backspace, KeyModifiers::CONTROL).is_ctrl_h());
        assert!(key(KeyCode::F(1), KeyModifiers::NONE).is_ctrl_h());
    }

    #[test]
    fn ctrl_j_is_recognised_however_the_terminal_spells_it() {
        assert!(key(KeyCode::Char('j'), KeyModifiers::CONTROL).is_ctrl_j());
        assert!(key(KeyCode::Enter, KeyModifiers::CONTROL).is_ctrl_j());
        assert!(key(KeyCode::F(2), KeyModifiers::NONE).is_ctrl_j());
    }

    /// Plain Backspace and Enter keep their own meanings.
    ///
    /// The terminal has already lost the distinction there, so treating them
    /// as the control shortcuts would break ordinary editing and would stop
    /// Enter from opening the detail pane.
    #[test]
    fn an_unmodified_backspace_or_enter_is_not_a_shortcut() {
        assert!(!key(KeyCode::Backspace, KeyModifiers::NONE).is_ctrl_h());
        assert!(!key(KeyCode::Enter, KeyModifiers::NONE).is_ctrl_j());
        assert!(key(KeyCode::Enter, KeyModifiers::NONE).is_enter());
    }
}
