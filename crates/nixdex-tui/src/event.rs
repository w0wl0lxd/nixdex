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
    pub fn is_quit(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
code: KeyCode::Char('c'), modifiers: KeyModifiers::CONTROL, .. } | KeyEvent {
code: KeyCode::Char('q'), modifiers: KeyModifiers::NONE, .. })
        )
    }

    pub fn is_up(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent { code: KeyCode::Up, modifiers: KeyModifiers::NONE, .. } |
KeyEvent { code: KeyCode::Char('k'), modifiers: KeyModifiers::NONE, .. })
        )
    }

    pub fn is_down(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent { code: KeyCode::Down, modifiers: KeyModifiers::NONE, .. }
| KeyEvent { code: KeyCode::Char('j'), modifiers: KeyModifiers::NONE, .. })
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

    pub fn is_space(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
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

    pub fn is_char_a(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_char_y(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('y'),
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_char_e(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('e'),
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_char_p(&self) -> bool {
        matches!(
            self,
            Self::Key(KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::NONE,
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
