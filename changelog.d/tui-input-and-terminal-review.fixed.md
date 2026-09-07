Typing a query in the TUI no longer quits the application or fires unrelated commands. `is_quit` matched a plain `q` and was checked before the guard that treats printable characters as search text, so any word containing `q` closed the session. Quit is now `Ctrl+C`; the detail overlay is a modal with no text entry and still closes on `q`.

The `/`, `:` and `?` shortcuts work again. They are characters, so the same guard returned before their handlers ran and typed them into the search box instead.

A space inside a query survives the debounce. The leading-space guard read `app.input`, which is still empty while characters are queued, so typing "ab c" quickly produced "abc".

Switching mode with Tab discards the queued query instead of running the previous mode's text against the newly selected mode.

The terminal is restored on every exit path. Raw mode and the alternate screen were left in place whenever the loop propagated an I/O error, which `terminal.draw` can do every frame, and the user had to run `reset` by hand. A guard type now restores them as it drops.

Terminal events are read through `crossterm::event::EventStream` rather than the blocking `event::read()`, which tied up a runtime worker thread for the whole session.

A mouse click selects the row under the pointer after scrolling; the row was mapped to a result index without the scroll offset. The "Searching…" indicator is drawn before the search runs, instead of being set and cleared between two frames where it could never be seen.
