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
    pub fn is_ctrl_h(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
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

    pub fn is_ctrl_j(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
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
