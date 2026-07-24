use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent};

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
            Event::Key(key) => AppEvent::Key(key),
            Event::Mouse(mouse) => AppEvent::Mouse(mouse),
            Event::Resize(w, h) => AppEvent::Resize(w, h),
            Event::Paste(_) => AppEvent::Tick,
            Event::FocusGained => AppEvent::Tick,
            Event::FocusLost => AppEvent::Tick,
        }
    }
}

impl AppEvent {
    pub fn is_quit(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) | AppEvent::Key(KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_up(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Up,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_down(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Down,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_page_up(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::PageUp,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_page_down(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::PageDown,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_home(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Home,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_end(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::End,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_enter(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_escape(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_tab(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_ctrl_r(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Char('r'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_ctrl_n(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_ctrl_j(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        )
    }

    pub fn is_slash(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Char('/'),
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn is_colon(&self) -> bool {
        matches!(
            self,
            AppEvent::Key(KeyEvent {
                code: KeyCode::Char(':'),
                modifiers: KeyModifiers::NONE,
                ..
            })
        )
    }

    pub fn as_char(&self) -> Option<char> {
        if let AppEvent::Key(KeyEvent {
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
}